"""AGENTS.md project-instruction loading, Codex-style.

Codex layers project instructions from several sources, outermost first:

1. A global file at ``~/.codex/AGENTS.md`` (user-wide defaults).
2. Each ``AGENTS.md`` from the git repository root down to the workspace,
   so a nested directory can refine instructions from its parents.

The collected text is injected into the system prompt as untrusted-but-trusted
project guidance. We cap total size so a huge AGENTS.md can't blow the context.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

GLOBAL_AGENTS = Path.home() / ".codex" / "AGENTS.md"
_AGENTS_FILENAME = "AGENTS.md"
_MAX_TOTAL_CHARS = 32_000


@dataclass
class AgentsDoc:
    """One discovered AGENTS.md: where it came from and its text."""

    source: str
    text: str


@dataclass
class AgentsInstructions:
    docs: list[AgentsDoc] = field(default_factory=list)

    @property
    def is_empty(self) -> bool:
        return not self.docs

    def render(self) -> str:
        """Concatenate all docs into a single block for the system prompt."""
        if not self.docs:
            return ""
        parts: list[str] = []
        for doc in self.docs:
            parts.append(f"## From {doc.source}\n\n{doc.text.strip()}")
        return "\n\n".join(parts)


def _git_root(start: Path) -> Path | None:
    """Walk up from *start* looking for a .git directory."""
    for parent in [start, *start.parents]:
        if (parent / ".git").exists():
            return parent
    return None


def _read(path: Path) -> str | None:
    try:
        if path.is_file():
            text = path.read_text(encoding="utf-8").strip()
            return text or None
    except OSError:
        return None
    return None


def discover_agents(
    workspace: Path,
    *,
    global_path: Path | None = None,
) -> AgentsInstructions:
    """Collect AGENTS.md docs: global, then git-root .. workspace (outermost first)."""
    workspace = Path(workspace).resolve()
    docs: list[AgentsDoc] = []
    seen: set[Path] = set()

    gp = GLOBAL_AGENTS if global_path is None else global_path
    if gp is not None:
        gp = Path(gp)
        text = _read(gp)
        if text is not None:
            docs.append(AgentsDoc(source=str(gp), text=text))
            seen.add(gp.resolve())

    # Build the directory chain from the git root (or workspace) down to workspace.
    root = _git_root(workspace) or workspace
    chain: list[Path] = []
    cur = workspace
    while True:
        chain.append(cur)
        if cur == root or cur.parent == cur:
            break
        cur = cur.parent
    chain.reverse()  # outermost (root) first

    for directory in chain:
        candidate = (directory / _AGENTS_FILENAME).resolve()
        if candidate in seen:
            continue
        text = _read(candidate)
        if text is not None:
            docs.append(AgentsDoc(source=str(candidate), text=text))
            seen.add(candidate)

    # Enforce a total budget, keeping the most-specific (last) docs.
    total = 0
    kept_reversed: list[AgentsDoc] = []
    for doc in reversed(docs):
        total += len(doc.text)
        if total > _MAX_TOTAL_CHARS and kept_reversed:
            break
        kept_reversed.append(doc)
    kept_reversed.reverse()
    return AgentsInstructions(docs=kept_reversed)
