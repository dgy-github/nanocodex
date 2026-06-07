"""MCP plugin store: CRUD over nanocodex's own ``~/.nanocodex/mcp.toml``.

The GUI plugin manager needs to add, edit, enable/disable, and remove MCP server
definitions. ``tomllib`` (stdlib) only *reads* TOML, and nanocodex avoids heavy
deps, so this module ships a tiny, purpose-built TOML *writer* for exactly the
shape mcp.toml uses: ``[mcp_servers.<name>]`` tables with string ``command``, a
string-list ``args``, an optional ``[mcp_servers.<name>.env]`` string table, and
an optional ``enabled`` bool.

Design (mirrors skills_store.py / schedule.py):

* **Pure serializer.** ``dump_mcp_toml(servers) -> str`` is a pure function over
  a list of :class:`~nanocodex.tools.mcp.McpServerConfig`, unit-tested with no
  filesystem. The round-trip (dump -> ``tomllib.loads`` -> parse) is the test
  contract, so we never hand-roll fragile string assertions.
* **Store wraps the file.** :class:`McpStore` reads via the existing
  ``discover_mcp_servers`` (so it sees the same servers the connector does,
  including disabled ones) and writes via the serializer. Edits take effect on
  the NEXT launch — the live MCP connection is a long-lived stdio session bound
  to its own event loop, and hot-reconnecting it is deliberately out of scope.
* **Plain file the user owns.** Everything stays human-readable TOML the user
  can also edit by hand.
"""

from __future__ import annotations

from pathlib import Path

from nanocodex.tools.mcp import (
    NANOCODEX_MCP_CONFIG,
    McpServerConfig,
    discover_mcp_servers,
)

# A server name must be a safe, single TOML key segment: letters/digits/dot/
# dash/underscore. This keeps it a bare key (no quoting needed) and blocks a
# crafted name from injecting extra TOML structure.
_VALID_NAME = set(
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-"
)


def is_valid_server_name(name: str) -> bool:
    """True if *name* is a safe bare TOML key (no quoting/escaping needed)."""
    name = (name or "").strip()
    return bool(name) and name not in (".", "..") and all(c in _VALID_NAME for c in name)


def _esc(value: str) -> str:
    """Escape a string for a TOML basic (double-quoted) string."""
    # Order matters: backslash first, then the quote. Newlines/tabs are escaped
    # so a multi-line value can't break the single-line key = "value" form.
    return (
        str(value)
        .replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t")
    )


def _dump_one(server: McpServerConfig) -> str:
    """Render one server as a ``[mcp_servers.<name>]`` TOML block (pure)."""
    lines = [f"[mcp_servers.{server.name}]"]
    lines.append(f'command = "{_esc(server.command)}"')
    args_items = ", ".join(f'"{_esc(a)}"' for a in server.args)
    lines.append(f"args = [{args_items}]")
    # Only write `enabled` when False — absent means True, so a normal enabled
    # server stays visually identical to a hand-written config.
    if not server.enabled:
        lines.append("enabled = false")
    if server.env:
        lines.append(f"[mcp_servers.{server.name}.env]")
        for k, v in server.env.items():
            lines.append(f'{k} = "{_esc(v)}"')
    return "\n".join(lines)


def dump_mcp_toml(servers: list[McpServerConfig]) -> str:
    """Serialize servers into mcp.toml text (pure; round-trips via tomllib)."""
    header = (
        "# nanocodex MCP server config. Managed by the GUI plugin manager, but\n"
        "# also safe to edit by hand. Each [mcp_servers.<name>] launches a server\n"
        "# over stdio. Set `enabled = false` to keep a definition without\n"
        "# connecting it. Tools run OUTSIDE the sandbox — only add servers you trust.\n"
    )
    blocks = [_dump_one(s) for s in servers]
    return header + "\n" + "\n\n".join(blocks) + ("\n" if blocks else "")


class McpStore:
    """Add / edit / enable / disable / remove MCP servers in mcp.toml.

    Changes are persisted immediately but only take effect on the next launch
    (the live connection is not hot-reloaded). Callers should tell the user that.
    """

    def __init__(self, path: Path | None = None) -> None:
        self.path = Path(path) if path is not None else NANOCODEX_MCP_CONFIG

    def list(self) -> list[McpServerConfig]:
        """All configured servers (including disabled), as the connector sees them."""
        return discover_mcp_servers(self.path)

    def get(self, name: str) -> McpServerConfig | None:
        for s in self.list():
            if s.name == name:
                return s
        return None

    def _save(self, servers: list[McpServerConfig]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text(dump_mcp_toml(servers), encoding="utf-8")

    def add(self, name: str, command: str, args: list[str] | None = None,
            env: dict[str, str] | None = None, *, enabled: bool = True,
            overwrite: bool = False) -> McpServerConfig:
        """Create a server. Raises ValueError on bad name / dup / empty command."""
        name = (name or "").strip()
        command = (command or "").strip()
        if not is_valid_server_name(name):
            raise ValueError(
                f"invalid server name {name!r}: use letters, digits, '.', '-', '_' only."
            )
        if not command:
            raise ValueError("a server needs a non-empty command.")
        servers = self.list()
        if any(s.name == name for s in servers) and not overwrite:
            raise ValueError(
                f"server {name!r} already exists; pass overwrite=true to replace it."
            )
        servers = [s for s in servers if s.name != name]
        new = McpServerConfig(
            name=name, command=command,
            args=[str(a) for a in (args or [])],
            env={str(k): str(v) for k, v in (env or {}).items()},
            enabled=bool(enabled),
        )
        servers.append(new)
        self._save(servers)
        return new

    def remove(self, name: str) -> bool:
        servers = self.list()
        kept = [s for s in servers if s.name != name]
        if len(kept) == len(servers):
            return False
        self._save(kept)
        return True

    def set_enabled(self, name: str, enabled: bool) -> bool:
        servers = self.list()
        found = False
        for s in servers:
            if s.name == name:
                s.enabled = bool(enabled)
                found = True
        if not found:
            return False
        self._save(servers)
        return True
