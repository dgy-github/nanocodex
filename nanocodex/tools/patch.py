"""apply_patch: Codex's V4A patch format.

The format, as emitted by Codex-family models::

    *** Begin Patch
    *** Add File: path/to/new.py
    +line one
    +line two
    *** Update File: path/to/existing.py
    @@ optional_context_header
     unchanged context line
    -removed line
    +added line
    *** Delete File: path/to/gone.py
    *** Move to: path/to/renamed.py        (optional, follows an Update File)
    *** End Patch

Rules mirrored from Codex:

* Each hunk line is prefixed with a single space (context), ``+`` (add), or
  ``-`` (remove). The leading marker is stripped to recover the real line.
* ``@@`` lines are *locators*: text that must appear before the change, used to
  disambiguate which occurrence to patch. Multiple ``@@`` lines nest.
* Context is matched against the file with three fallbacks: exact, then
  ignoring trailing whitespace, then ignoring all surrounding whitespace.
* The patch is applied atomically: if any hunk fails to locate, nothing is
  written and a descriptive error is returned.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path


class PatchError(ValueError):
    """Raised when a patch cannot be parsed or applied."""


class ActionType(Enum):
    ADD = "add"
    UPDATE = "update"
    DELETE = "delete"


@dataclass
class Chunk:
    """A contiguous change inside an Update hunk."""

    context_before: list[str] = field(default_factory=list)
    del_lines: list[str] = field(default_factory=list)
    ins_lines: list[str] = field(default_factory=list)
    # The locator (@@) lines that must precede this chunk, outermost first.
    locators: list[str] = field(default_factory=list)


@dataclass
class FileAction:
    type: ActionType
    path: str
    # For ADD: the new file's lines. For UPDATE: the parsed chunks.
    new_lines: list[str] = field(default_factory=list)
    chunks: list[Chunk] = field(default_factory=list)
    move_to: str | None = None


_BEGIN = "*** Begin Patch"
_END = "*** End Patch"
_ADD = "*** Add File: "
_UPDATE = "*** Update File: "
_DELETE = "*** Delete File: "
_MOVE = "*** Move to: "
_HUNK_AT = "@@"


def parse_patch(text: str) -> list[FileAction]:
    """Parse a V4A patch envelope into structured file actions."""
    lines = text.splitlines()
    if not lines or lines[0].strip() != _BEGIN:
        raise PatchError("patch must start with '*** Begin Patch'")
    # Find the End marker; ignore anything after it.
    try:
        end_idx = next(i for i, ln in enumerate(lines) if ln.strip() == _END)
    except StopIteration as exc:
        raise PatchError("patch must end with '*** End Patch'") from exc

    body = lines[1:end_idx]
    actions: list[FileAction] = []
    i = 0
    n = len(body)
    while i < n:
        line = body[i]
        if line.startswith(_ADD):
            path = line[len(_ADD):].strip()
            i += 1
            new_lines: list[str] = []
            while i < n and not body[i].startswith("*** "):
                content = body[i]
                if content and content[0] not in "+ ":
                    raise PatchError(
                        f"Add File '{path}': every line must start with '+' "
                        f"(got {content!r})"
                    )
                new_lines.append(content[1:] if content else "")
                i += 1
            actions.append(FileAction(ActionType.ADD, path, new_lines=new_lines))
            continue
        if line.startswith(_DELETE):
            path = line[len(_DELETE):].strip()
            actions.append(FileAction(ActionType.DELETE, path))
            i += 1
            continue
        if line.startswith(_UPDATE):
            path = line[len(_UPDATE):].strip()
            i += 1
            move_to = None
            if i < n and body[i].startswith(_MOVE):
                move_to = body[i][len(_MOVE):].strip()
                i += 1
            chunks, i = _parse_update_body(body, i, n, path)
            actions.append(
                FileAction(ActionType.UPDATE, path, chunks=chunks, move_to=move_to)
            )
            continue
        if not line.strip():
            i += 1
            continue
        raise PatchError(f"unexpected line in patch: {line!r}")

    if not actions:
        raise PatchError("patch contained no file actions")
    return actions


def _parse_update_body(
    body: list[str], i: int, n: int, path: str
) -> tuple[list[Chunk], int]:
    """Parse the hunk body of an Update File section."""
    chunks: list[Chunk] = []
    pending_locators: list[str] = []
    current: Chunk | None = None

    def flush() -> None:
        nonlocal current
        if current is not None and (current.del_lines or current.ins_lines):
            chunks.append(current)
        current = None

    while i < n and not body[i].startswith("*** "):
        raw = body[i]
        if raw.startswith(_HUNK_AT):
            flush()
            locator = raw[len(_HUNK_AT):].strip()
            if locator:
                pending_locators.append(locator)
            i += 1
            continue

        marker = raw[0] if raw else " "
        content = raw[1:] if raw else ""
        if marker == " ":
            # Context line: closes any in-progress change chunk.
            flush()
            i += 1
            continue
        if marker in "+-":
            if current is None:
                current = Chunk(locators=list(pending_locators))
                pending_locators = []
            if marker == "-":
                current.del_lines.append(content)
            else:
                current.ins_lines.append(content)
            i += 1
            continue
        raise PatchError(
            f"Update File '{path}': line must start with ' ', '+', '-', or '@@' "
            f"(got {raw!r})"
        )
    flush()
    if not chunks:
        raise PatchError(f"Update File '{path}': no changes found")
    return chunks, i


# --- application ---------------------------------------------------------


def _match_at(haystack: list[str], needle: list[str], start: int) -> int:
    """Find *needle* in *haystack* at or after *start*; return index or -1.

    Three-level fallback: exact, rstrip, then full-strip equality.
    """
    if not needle:
        return start
    for normalize in (lambda s: s, lambda s: s.rstrip(), lambda s: s.strip()):
        norm_needle = [normalize(x) for x in needle]
        for idx in range(start, len(haystack) - len(needle) + 1):
            window = [normalize(haystack[idx + k]) for k in range(len(needle))]
            if window == norm_needle:
                return idx
    return -1


def _apply_update(original: str, action: FileAction) -> str:
    lines = original.splitlines()
    keepends_trailing_nl = original.endswith("\n")
    cursor = 0
    out_offset = 0  # net change applied so far, to keep cursor aligned

    result = list(lines)
    for chunk in action.chunks:
        search_from = cursor
        # Honor locators: advance the cursor past each locator line in order.
        for locator in chunk.locators:
            loc_idx = _match_at(result, [locator], search_from)
            if loc_idx == -1:
                raise PatchError(
                    f"Update File '{action.path}': locator {locator!r} not found"
                )
            search_from = loc_idx + 1

        if chunk.del_lines:
            idx = _match_at(result, chunk.del_lines, search_from)
            if idx == -1:
                raise PatchError(
                    f"Update File '{action.path}': could not locate the lines to "
                    f"replace:\n" + "\n".join(chunk.del_lines)
                )
            result[idx: idx + len(chunk.del_lines)] = chunk.ins_lines
            cursor = idx + len(chunk.ins_lines)
        else:
            # Pure insertion at the cursor / after locators.
            insert_at = search_from
            result[insert_at:insert_at] = chunk.ins_lines
            cursor = insert_at + len(chunk.ins_lines)
        out_offset += len(chunk.ins_lines) - len(chunk.del_lines)

    text = "\n".join(result)
    if keepends_trailing_nl or (action.chunks and result):
        text += "\n"
    return text


@dataclass
class ApplyOutcome:
    """What changed, for reporting back to the model and CLI."""

    added: list[str] = field(default_factory=list)
    updated: list[str] = field(default_factory=list)
    deleted: list[str] = field(default_factory=list)
    moved: list[tuple[str, str]] = field(default_factory=list)

    def summary(self) -> str:
        parts: list[str] = []
        for p in self.added:
            parts.append(f"  A {p}")
        for src, dst in self.moved:
            parts.append(f"  R {src} -> {dst}")
        for p in self.updated:
            parts.append(f"  M {p}")
        for p in self.deleted:
            parts.append(f"  D {p}")
        return "\n".join(parts)


def apply_patch(
    text: str,
    *,
    root: Path,
    can_write,
) -> ApplyOutcome:
    """Parse and apply a V4A patch under *root*.

    *can_write(path) -> bool* gates every file the patch would create, modify,
    move, or delete. The patch is staged fully in memory first; if any hunk
    fails to apply or any path is unwritable, nothing is written to disk.
    """
    actions = parse_patch(text)
    root = Path(root).resolve()

    def resolve(rel: str) -> Path:
        p = (root / rel).resolve()
        return p

    # --- validate + stage ------------------------------------------------
    staged_writes: list[tuple[Path, str]] = []
    staged_deletes: list[Path] = []
    staged_moves: list[tuple[Path, Path]] = []
    outcome = ApplyOutcome()

    for action in actions:
        target = resolve(action.path)
        if not can_write(target):
            raise PatchError(
                f"path is outside the writable sandbox: {action.path}"
            )

        if action.type is ActionType.ADD:
            if target.exists():
                raise PatchError(f"Add File: {action.path} already exists")
            content = "\n".join(action.new_lines)
            if action.new_lines:
                content += "\n"
            staged_writes.append((target, content))
            outcome.added.append(action.path)

        elif action.type is ActionType.DELETE:
            if not target.is_file():
                raise PatchError(f"Delete File: {action.path} not found")
            staged_deletes.append(target)
            outcome.deleted.append(action.path)

        elif action.type is ActionType.UPDATE:
            if not target.is_file():
                raise PatchError(f"Update File: {action.path} not found")
            original = target.read_text(encoding="utf-8").replace("\r\n", "\n")
            new_text = _apply_update(original, action)
            if action.move_to:
                dest = resolve(action.move_to)
                if not can_write(dest):
                    raise PatchError(
                        f"Move target is outside the writable sandbox: {action.move_to}"
                    )
                staged_writes.append((dest, new_text))
                staged_moves.append((target, dest))
                outcome.moved.append((action.path, action.move_to))
            else:
                staged_writes.append((target, new_text))
                outcome.updated.append(action.path)

    # --- commit ----------------------------------------------------------
    for path, content in staged_writes:
        path.parent.mkdir(parents=True, exist_ok=True)
        # newline="" disables newline translation so '\n' is written verbatim
        # (Windows text mode would otherwise rewrite it to '\r\n').
        path.write_text(content, encoding="utf-8", newline="")
    for src, _dst in staged_moves:
        # The new content was already written to dst above; remove the source.
        if src.exists():
            src.unlink()
    for path in staged_deletes:
        path.unlink()

    return outcome
