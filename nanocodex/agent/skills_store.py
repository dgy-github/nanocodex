"""Skills: installable, reusable capability docs (SKILL.md), Codex/Claude-style.

A "skill" is a folder under ``~/.nanocodex/skills/<name>/`` containing a
``SKILL.md`` file. The file starts with a small frontmatter block naming the
skill and giving a one-line description, followed by a markdown body with the
actual how-to instructions::

    ---
    name: wechat-reply
    description: Read a WeChat chat and reply in my voice using the MCP desktop tools.
    ---

    # How to reply on WeChat
    1. read_wechat_chat(contact=...) to get the latest messages
    2. ... full instructions the model reads on demand ...

Design (mirrors agents_md.py + schedule.py):

* **Progressive disclosure.** Only each skill's name + description go into the
  system prompt (cheap, always-on). The full body is read on demand by the model
  via ``read_file`` — so installing 20 skills costs ~20 lines of context, not 20
  full documents. This is the same pattern DeepSeek-TUI / Claude use.
* **Pure functions over data.** Parsing and discovery are pure (path in, data
  out, no globals, no clocks) so they unit-test offline. The store just wraps
  filesystem CRUD on the skills directory.
* **Plain files the user owns.** Everything is plaintext under a visible dir the
  user can read, edit, or delete by hand — no hidden state, no DB.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path

DEFAULT_SKILLS_DIR = Path.home() / ".nanocodex" / "skills"
_SKILL_FILENAME = "SKILL.md"

# Built-in skills ship inside the package (``nanocodex/builtin_skills/``) so a
# fresh install has useful general-purpose skills (code-review, debug, …)
# without the user installing anything. They're read-only knowledge that
# travels with the release — distinct from user data under DEFAULT_SKILLS_DIR.
# A user-installed skill with the same name shadows the built-in one.
BUILTIN_SKILLS_DIR = Path(__file__).resolve().parent.parent / "builtin_skills"

# A skill name must be a safe single path segment: letters/digits/dot/dash/
# underscore, no separators or traversal. Used to gate install + lookup so a
# crafted name can't escape the skills directory.
_NAME_RE = re.compile(r"^[A-Za-z0-9._-]+$")

# Keep the per-skill description short in the system prompt; a runaway
# description shouldn't be able to bloat the prompt. The full text lives in the
# body, which the model reads on demand.
_MAX_DESCRIPTION_CHARS = 500


@dataclass
class Skill:
    """One discovered skill: its name, model-visible description, and body."""

    name: str
    description: str
    body: str = ""
    source: str = ""  # absolute path to the SKILL.md, for read_file references

    def header_line(self) -> str:
        """The single line shown per skill in the system prompt."""
        desc = self.description.strip() or "(no description)"
        return f"- {self.name}: {desc}"


def _split_frontmatter(text: str) -> tuple[dict[str, str], str]:
    """Parse a leading ``---`` frontmatter block into {key: value} + the body.

    Deliberately tiny (no YAML dep, matching nanocodex's no-heavy-deps rule):
    only flat ``key: value`` lines are recognized, which is all SKILL.md needs.
    If there's no well-formed frontmatter, returns ({}, original_text).
    """
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return {}, text
    meta: dict[str, str] = {}
    for i in range(1, len(lines)):
        stripped = lines[i].strip()
        if stripped == "---":
            body = "\n".join(lines[i + 1:]).strip()
            return meta, body
        if not stripped or stripped.startswith("#"):
            continue
        if ":" in stripped:
            key, _, value = stripped.partition(":")
            meta[key.strip().lower()] = value.strip()
    # No closing '---': treat the whole thing as body (frontmatter unterminated).
    return {}, text


def parse_skill(text: str, *, fallback_name: str = "", source: str = "") -> Skill | None:
    """Parse SKILL.md text into a Skill (pure). Returns None if unusable.

    A skill is usable when it has a name (from frontmatter, else the folder name)
    and a non-empty description. The description is trimmed to a sane length so a
    single skill can't dominate the prompt.
    """
    meta, body = _split_frontmatter(text)
    name = (meta.get("name") or fallback_name).strip()
    description = (meta.get("description") or "").strip()
    if not name or not description:
        return None
    if len(description) > _MAX_DESCRIPTION_CHARS:
        description = description[:_MAX_DESCRIPTION_CHARS].rstrip() + "…"
    return Skill(name=name, description=description, body=body, source=source)


def is_valid_skill_name(name: str) -> bool:
    """True if *name* is a safe single path segment (no separators/traversal)."""
    name = (name or "").strip()
    return bool(name) and name not in (".", "..") and _NAME_RE.match(name) is not None


@dataclass
class SkillsCollection:
    """All discovered skills + any parse warnings, for the prompt and the tool."""

    skills: list[Skill] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    @property
    def is_empty(self) -> bool:
        return not self.skills

    def render_for_prompt(self) -> str:
        """The compact block injected into the system prompt (names + one-liners)."""
        if not self.skills:
            return ""
        return "\n".join(s.header_line() for s in self.skills)


def _scan_one_dir(base: Path, collection: SkillsCollection) -> None:
    """Scan a single skills root, appending parsed skills/warnings into *collection*.

    A skill whose name was already added (e.g. by an earlier, higher-priority
    root) is skipped, so a user skill shadows a built-in of the same name.
    """
    if not base.is_dir():
        return
    seen = {s.name for s in collection.skills}
    for entry in sorted(base.iterdir(), key=lambda p: p.name.lower()):
        if not entry.is_dir():
            continue
        skill_file = entry / _SKILL_FILENAME
        if not skill_file.is_file():
            continue
        try:
            text = skill_file.read_text(encoding="utf-8")
        except OSError as exc:
            collection.warnings.append(f"{entry.name}: unreadable ({exc})")
            continue
        skill = parse_skill(text, fallback_name=entry.name, source=str(skill_file))
        if skill is None:
            collection.warnings.append(
                f"{entry.name}: SKILL.md missing name/description frontmatter"
            )
            continue
        if skill.name in seen:
            continue  # shadowed by a higher-priority root already scanned
        seen.add(skill.name)
        collection.skills.append(skill)


def discover_skills(skills_dir: Path | None = None) -> SkillsCollection:
    """Discover skills, parsing each ``<name>/SKILL.md`` (pure-ish I/O).

    With no argument, scans the user skills dir *and* the package's built-in
    skills, with user skills shadowing same-named built-ins. When an explicit
    *skills_dir* is given (e.g. SkillsStore CRUD on a specific dir), only that
    directory is scanned — built-ins are never touched by install/remove.

    Skips unreadable or malformed skills, recording a warning rather than
    raising, so one bad skill never blocks the rest (or startup).
    """
    collection = SkillsCollection()
    if skills_dir is not None:
        _scan_one_dir(Path(skills_dir), collection)
        return collection
    # Default mode: user dir first (so it shadows built-ins), then built-ins.
    _scan_one_dir(DEFAULT_SKILLS_DIR, collection)
    _scan_one_dir(BUILTIN_SKILLS_DIR, collection)
    collection.skills.sort(key=lambda s: s.name.lower())
    return collection


class SkillsStore:
    """Filesystem CRUD over the skills directory (install / list / remove / show)."""

    def __init__(self, skills_dir: Path | None = None) -> None:
        self.dir = Path(skills_dir) if skills_dir is not None else DEFAULT_SKILLS_DIR

    def list(self) -> SkillsCollection:
        return discover_skills(self.dir)

    def get(self, name: str) -> Skill | None:
        if not is_valid_skill_name(name):
            return None
        skill_file = self.dir / name / _SKILL_FILENAME
        try:
            text = skill_file.read_text(encoding="utf-8")
        except OSError:
            return None
        return parse_skill(text, fallback_name=name, source=str(skill_file))

    def install(self, name: str, description: str, body: str = "",
                *, overwrite: bool = False) -> Skill:
        """Create ``<dir>/<name>/SKILL.md`` from name + description + body.

        Raises ValueError on an invalid name, an empty description, or an
        existing skill when overwrite is False.
        """
        name = (name or "").strip()
        description = (description or "").strip()
        if not is_valid_skill_name(name):
            raise ValueError(
                f"invalid skill name {name!r}: use letters, digits, '.', '-', '_' "
                "only (no path separators)."
            )
        if not description:
            raise ValueError("a skill needs a non-empty description.")
        target_dir = self.dir / name
        target_file = target_dir / _SKILL_FILENAME
        if target_file.exists() and not overwrite:
            raise ValueError(
                f"skill {name!r} already exists; pass overwrite=true to replace it."
            )
        content = self._render_skill_md(name, description, body)
        target_dir.mkdir(parents=True, exist_ok=True)
        target_file.write_text(content, encoding="utf-8")
        return Skill(name=name, description=description, body=body.strip(),
                     source=str(target_file))

    def remove(self, name: str) -> bool:
        """Delete a skill's folder. Returns False if it wasn't there."""
        if not is_valid_skill_name(name):
            return False
        target_dir = self.dir / name
        target_file = target_dir / _SKILL_FILENAME
        if not target_file.is_file():
            return False
        try:
            target_file.unlink()
            # Remove the now-empty folder (ignore if other files linger).
            try:
                target_dir.rmdir()
            except OSError:
                pass
            return True
        except OSError:
            return False

    @staticmethod
    def _render_skill_md(name: str, description: str, body: str) -> str:
        """Compose a SKILL.md with frontmatter + body (pure)."""
        body = (body or "").strip()
        # Keep descriptions to one physical line in frontmatter (our parser is
        # line-based); collapse any newlines the caller passed in.
        one_line_desc = " ".join(description.split())
        parts = ["---", f"name: {name}", f"description: {one_line_desc}", "---", ""]
        if body:
            parts.append(body)
            parts.append("")
        return "\n".join(parts)
