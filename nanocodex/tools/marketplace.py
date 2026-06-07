"""MCP marketplace: a browsable catalog of MCP servers with one-click install.

Two sources, both feeding the SAME ``McpStore`` the manual "MCP servers"
settings section uses:

* **Built-in catalog** (:data:`BUILTIN_CATALOG`) — a hand-curated list shipped
  with nanocodex: the project's own ``windows-computer-use-mcp`` plus a few
  well-known official servers from modelcontextprotocol. Pure data, no network.
* **Remote catalog** — fetched on demand from ``NANOCODEX_MARKETPLACE_URL``
  (env, no built-in default — nanocodex hosts no catalog service). The response
  is JSON describing the same :class:`CatalogEntry` shape.

Design (mirrors mcp_store.py / skills_store.py):

* **Pure core, injectable I/O.** Parsing/validation (:func:`parse_remote_catalog`)
  is a pure function unit-tested with no network; :func:`fetch_remote_catalog`
  takes an injectable ``opener`` so tests never touch the wire.
* **One install path.** :func:`install_entry` funnels everything through
  ``McpStore().add`` so the security validation (``is_valid_server_name``,
  non-empty command, dup check) is identical to a hand-typed server.

Security note
-------------
Installing a server writes it to ``~/.nanocodex/mcp.toml`` and it will launch a
subprocess OUTSIDE the sandbox on the next run — exactly like a hand-added
server. The marketplace does NOT lower that bar: every entry's ``name`` is
validated, and the remote catalog is treated as untrusted data (per-entry
validation, bad entries dropped, never executed at fetch time). The user is
still responsible for trusting what they install.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass, field
from typing import Any, Callable

from nanocodex.tools.mcp import McpServerConfig
from nanocodex.tools.mcp_store import McpStore, is_valid_server_name

# Env var holding the remote catalog URL. No built-in default: nanocodex hosts
# no catalog service, so a hardcoded URL would only ever 404.
MARKETPLACE_URL_ENV = "NANOCODEX_MARKETPLACE_URL"

# Cap on remote entries parsed from one response — a crude guard against a
# hostile/huge JSON blowing up memory or the UI.
_MAX_REMOTE_ENTRIES = 200

# Valid `source` tags. "builtin" = ships in this repo, "official" = well-known
# upstream server, "remote" = came from the configured catalog URL.
VALID_SOURCES = ("builtin", "official", "remote")

# (url, timeout) -> bytes. Injectable so tests never hit the network.
OpenerFn = Callable[[str, float], bytes]


@dataclass
class CatalogEntry:
    """One installable MCP server in the marketplace.

    ``env_keys`` lists the NAMES of environment variables the server needs the
    user to provide (e.g. an API token) — never the values. The GUI prompts for
    these at install time. ``path_arg_index`` marks an entry whose command needs
    a machine-specific absolute path filled in (the project-local server, whose
    location differs per machine) — see :data:`BUILTIN_CATALOG`.
    """

    name: str
    command: str
    args: list[str] = field(default_factory=list)
    env_keys: list[str] = field(default_factory=list)
    description: str = ""
    source: str = "remote"
    # When set, the user must supply an absolute path at install time, slotted
    # into args at this index. Used for the project-local server (McpServerConfig
    # has no cwd field and the path is machine-specific, so we can't hardcode it).
    path_arg_index: int | None = None
    path_label: str = ""


# --- built-in catalog -------------------------------------------------------
#
# Hand-curated. The project-local server (windows_computer_use) needs an
# absolute path to its server.py, which is machine-specific — McpServerConfig
# has no cwd field, so we mark it needs-path and let the user fill it at install
# time (path_arg_index points at the args slot to replace). The official servers
# launch via npx/uvx and assume the user has Node/uv on PATH.

BUILTIN_CATALOG: list[CatalogEntry] = [
    CatalogEntry(
        name="windows_computer_use",
        command="python",
        args=["<path-to>/windows-computer-use-mcp/server.py"],
        description=("Project-local Windows desktop executor (clicks, typing, "
                     "screenshots, window control) exposed over MCP. Runs OUTSIDE "
                     "the sandbox."),
        source="builtin",
        path_arg_index=0,
        path_label="Absolute path to windows-computer-use-mcp/server.py",
    ),
    CatalogEntry(
        name="filesystem",
        command="npx",
        args=["-y", "@modelcontextprotocol/server-filesystem", "<allowed-dir>"],
        description=("Official MCP filesystem server: read/write files under a "
                     "directory you allow. Needs Node.js (npx)."),
        source="official",
        path_arg_index=2,
        path_label="Absolute path to the directory to expose",
    ),
    CatalogEntry(
        name="fetch",
        command="uvx",
        args=["mcp-server-fetch"],
        description=("Official MCP fetch server: retrieve a URL and convert it "
                     "to markdown for the model. Needs uv (uvx)."),
        source="official",
    ),
    CatalogEntry(
        name="git",
        command="uvx",
        args=["mcp-server-git"],
        description=("Official MCP git server: read/search/inspect a git repo. "
                     "Needs uv (uvx)."),
        source="official",
    ),
    CatalogEntry(
        name="memory",
        command="npx",
        args=["-y", "@modelcontextprotocol/server-memory"],
        description=("Official MCP memory server: a simple knowledge-graph "
                     "persistent memory. Needs Node.js (npx)."),
        source="official",
    ),
]


def marketplace_url() -> str | None:
    """The configured remote catalog URL, or None when the env var is unset/blank."""
    val = os.environ.get(MARKETPLACE_URL_ENV, "").strip()
    return val or None


def _entry_from_dict(raw: Any) -> CatalogEntry | None:
    """Build a validated :class:`CatalogEntry` from one remote JSON object.

    Returns None (caller drops it) when the entry is unusable: not an object,
    bad/missing name, or empty command. Unknown fields are ignored. This is the
    per-entry trust boundary for untrusted remote data.
    """
    if not isinstance(raw, dict):
        return None
    name = str(raw.get("name", "")).strip()
    command = str(raw.get("command", "")).strip()
    if not is_valid_server_name(name) or not command:
        return None
    args_raw = raw.get("args", [])
    args = [str(a) for a in args_raw] if isinstance(args_raw, list) else []
    env_raw = raw.get("env_keys", [])
    env_keys = [str(k) for k in env_raw if str(k).strip()] if isinstance(env_raw, list) else []
    description = str(raw.get("description", "")).strip()
    return CatalogEntry(
        name=name,
        command=command,
        args=args,
        env_keys=env_keys,
        description=description,
        source="remote",
    )


def parse_remote_catalog(raw: str | bytes) -> list[CatalogEntry]:
    """Parse a remote catalog JSON document into validated entries (pure).

    Accepts either a top-level list of entries, or an object with an
    ``entries``/``servers`` list. Malformed JSON yields an empty list (the GUI
    shows a friendly error rather than crashing). Individual bad entries are
    dropped; the rest survive. Capped at :data:`_MAX_REMOTE_ENTRIES`.
    """
    if isinstance(raw, bytes):
        try:
            raw = raw.decode("utf-8")
        except UnicodeDecodeError:
            return []
    try:
        data = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return []

    if isinstance(data, dict):
        items = data.get("entries")
        if items is None:
            items = data.get("servers")
    else:
        items = data
    if not isinstance(items, list):
        return []

    out: list[CatalogEntry] = []
    seen: set[str] = set()
    for item in items[:_MAX_REMOTE_ENTRIES]:
        entry = _entry_from_dict(item)
        if entry is None or entry.name in seen:
            continue
        seen.add(entry.name)
        out.append(entry)
    return out


def _default_opener(url: str, timeout: float) -> bytes:
    """Fetch *url* and return the raw body bytes (network required; lazy import)."""
    import urllib.request

    with urllib.request.urlopen(url, timeout=timeout) as resp:  # noqa: S310
        return resp.read()


def fetch_remote_catalog(
    url: str,
    *,
    opener: OpenerFn | None = None,
    timeout: float = 10.0,
) -> list[CatalogEntry]:
    """Fetch and parse the remote catalog at *url*.

    *opener* is injectable so tests run fully offline. Network/parse errors
    propagate as exceptions to the caller (the GUI catches and shows them);
    a well-formed-but-empty catalog returns ``[]``.
    """
    fetch = opener or _default_opener
    body = fetch(url, timeout)
    return parse_remote_catalog(body)


def install_entry(
    entry: CatalogEntry,
    env_values: dict[str, str] | None = None,
    *,
    path_value: str | None = None,
    store: McpStore | None = None,
    overwrite: bool = False,
) -> McpServerConfig:
    """Install *entry* into mcp.toml via :class:`McpStore`.

    *env_values* supplies values for the env vars the entry declares; only keys
    in ``entry.env_keys`` are kept (extras ignored). *path_value* fills the
    machine-specific path slot when ``entry.path_arg_index`` is set — raises
    ValueError if such an entry is installed without one. Everything funnels
    through ``McpStore().add`` so validation matches a hand-typed server.
    """
    st = store or McpStore()
    args = list(entry.args)
    if entry.path_arg_index is not None:
        if not (path_value and path_value.strip()):
            raise ValueError(
                f"{entry.name!r} needs a path: {entry.path_label or 'absolute path'}."
            )
        idx = entry.path_arg_index
        if 0 <= idx < len(args):
            args[idx] = path_value.strip()
        else:
            args.append(path_value.strip())

    env: dict[str, str] = {}
    declared = set(entry.env_keys)
    for key, val in (env_values or {}).items():
        if key in declared and str(val).strip():
            env[key] = str(val)

    return st.add(entry.name, entry.command, args, env, overwrite=overwrite)
