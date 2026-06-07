"""User memory: a persistent personal note file the model sees every turn.

Ported from DeepSeek-TUI's ``memory.rs`` MVP, adapted to nanocodex's idiom
(plain file the user owns, pure functions over data, no heavy deps). A single
markdown file at ``~/.nanocodex/memory.md`` holds durable facts and preferences
that should survive across sessions and projects — e.g. how the user likes
replies phrased, names/relationships, conventions — distinct from:

* **AGENTS.md** — project-scoped instructions discovered per workspace, and
* **skills** — reusable how-to guides for specific tasks.

Memory is "about who/what" (preferences, facts); skills are "how to do X".

How it surfaces / is written:

* **Load + inject.** ``load()`` reads the file; ``as_system_block()`` wraps it
  in a ``<user_memory>`` section prepended into the system prompt every turn.
* **`remember` tool.** The model appends a durable note when it notices a
  lasting preference worth keeping (see tools/remember_tool.py).
* **`#` quick-capture.** Typing ``# something`` in the GUI composer appends a
  timestamped bullet without leaving the chat (wired in gui.py).

Design (mirrors skills_store.py / schedule.py): parsing/rendering are pure
functions over text, so they unit-test offline with zero filesystem. The store
wraps the file with append/read. Always-on when the file exists (same as
AGENTS.md) — no config flag, matching nanocodex's no-hidden-state simplicity.
"""

from __future__ import annotations

from datetime import datetime
from pathlib import Path

DEFAULT_MEMORY_PATH = Path.home() / ".nanocodex" / "memory.md"

# Cap how much memory is injected, so a runaway file can't dominate the prompt.
# Larger files still load but are truncated with a visible marker (the model is
# told it only saw a slice). Mirrors DeepSeek-TUI's MAX_MEMORY_SIZE.
_MAX_MEMORY_CHARS = 100 * 1024


def _now_stamp() -> str:
    """Local timestamp for a captured bullet (date + minute is enough)."""
    return datetime.now().strftime("%Y-%m-%d %H:%M")


def load(path: Path | None = None) -> str | None:
    """Read the memory file, or None when it's absent or blank after trimming."""
    p = Path(path) if path is not None else DEFAULT_MEMORY_PATH
    try:
        content = p.read_text(encoding="utf-8")
    except OSError:
        return None
    return content if content.strip() else None


def as_system_block(content: str, *, source: Path | None = None) -> str:
    """Wrap memory text in a ``<user_memory>`` block for the system prompt (pure).

    Returns "" for empty content. Oversized content is truncated to
    ``_MAX_MEMORY_CHARS`` with a marker so the model knows it saw only a slice.
    """
    trimmed = content.strip()
    if not trimmed:
        return ""
    src = str(source) if source is not None else str(DEFAULT_MEMORY_PATH)
    body = trimmed
    truncated_note = ""
    if len(body) > _MAX_MEMORY_CHARS:
        body = body[:_MAX_MEMORY_CHARS].rstrip()
        truncated_note = (
            f'\n<truncated note="memory exceeded {_MAX_MEMORY_CHARS} chars; '
            'only the start is shown" />'
        )
    return (
        f'<user_memory source="{src}">\n'
        f"{body}{truncated_note}\n"
        "</user_memory>"
    )


def render_for_prompt(path: Path | None = None) -> str:
    """Convenience: load + wrap in one call. "" when there's no memory."""
    content = load(path)
    if content is None:
        return ""
    p = Path(path) if path is not None else DEFAULT_MEMORY_PATH
    return as_system_block(content, source=p)


def format_bullet(note: str, *, now: str | None = None) -> str:
    """Render one timestamped markdown bullet (pure). Collapses inner newlines."""
    one_line = " ".join(str(note).split())
    stamp = now if now is not None else _now_stamp()
    return f"- [{stamp}] {one_line}"


class MemoryStore:
    """Append-and-read wrapper over the user memory file."""

    def __init__(self, path: Path | None = None) -> None:
        self.path = Path(path) if path is not None else DEFAULT_MEMORY_PATH

    def load(self) -> str | None:
        return load(self.path)

    def render_for_prompt(self) -> str:
        return render_for_prompt(self.path)

    def append(self, note: str, *, now: str | None = None) -> str:
        """Append a timestamped bullet. Returns the bullet line written.

        Raises ValueError on an empty note. Creates the file (and parent dir)
        on first write. A heading is added once so a fresh file reads sensibly.
        """
        note = (note or "").strip()
        if not note:
            raise ValueError("a memory note must be non-empty.")
        bullet = format_bullet(note, now=now)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        existing = ""
        try:
            existing = self.path.read_text(encoding="utf-8")
        except OSError:
            existing = ""
        parts: list[str] = []
        if not existing.strip():
            # Fresh (or blank) file: start with a heading so it reads as a doc.
            parts.append("# nanocodex user memory\n")
        elif not existing.endswith("\n"):
            parts.append("\n")
        parts.append(bullet + "\n")
        try:
            with self.path.open("a", encoding="utf-8") as fh:
                fh.write("".join(parts))
        except OSError as exc:
            raise ValueError(f"could not write memory file: {exc}") from exc
        return bullet
