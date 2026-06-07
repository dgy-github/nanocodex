"""manage_skills: let the agent install/list/remove/show reusable SKILL.md guides.

A "skill" is a saved how-to guide (a SKILL.md under ~/.nanocodex/skills/<name>/)
that the model can follow for a specific recurring task — e.g. "reply on WeChat
in my voice". Installed skills' names + one-line descriptions are injected into
the system prompt (progressive disclosure); the full body is read on demand via
`read_file`. This tool is the CRUD surface over that directory.

Mirrors ManageScheduleTool: one tool, several actions, wrapping the pure
SkillsStore so a skill created here is discovered on the next prompt build.
"""

from __future__ import annotations

from typing import Any

from nanocodex.tools.base import Tool


class ManageSkillsTool(Tool):
    @property
    def name(self) -> str:
        return "manage_skills"

    @property
    def description(self) -> str:
        return (
            "Install, list, remove, or show SKILLS — saved step-by-step how-to "
            "guides for recurring tasks, so a workflow learned once is reused "
            "without re-explaining it every session. Use this when the user says "
            "'save this as a skill', 'remember how to do X', 'install a skill', "
            "or asks what skills exist. Actions: 'install' (needs name + "
            "description; optional body with the full instructions, overwrite to "
            "replace), 'list', 'show' (needs name; returns the full SKILL.md path "
            "+ body), 'remove' (needs name). A skill's name + description are "
            "always visible in the system prompt; the body is read on demand with "
            "`read_file`. Names must be a single safe path segment (letters, "
            "digits, '.', '-', '_'). Installing a skill does NOT run anything and "
            "grants no new permissions — it just saves text the model can follow."
        )

    @property
    def parameters(self) -> dict[str, Any]:
        return {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["install", "list", "show", "remove"],
                },
                "name": {
                    "type": "string",
                    "description": (
                        "Skill name (a single safe path segment). Required for "
                        "install/show/remove."
                    ),
                },
                "description": {
                    "type": "string",
                    "description": (
                        "One-line description shown in the system prompt (for "
                        "'install'). This is how the model decides when the skill "
                        "applies, so make it specific."
                    ),
                },
                "body": {
                    "type": "string",
                    "description": (
                        "The full how-to instructions (markdown) saved as the "
                        "SKILL.md body, read on demand (for 'install'). Optional "
                        "but strongly recommended — without it the skill is just a "
                        "title."
                    ),
                },
                "overwrite": {
                    "type": "boolean",
                    "description": (
                        "When true, replace an existing skill of the same name "
                        "(for 'install'). Default false (refuses if it exists)."
                    ),
                },
            },
            "required": ["action"],
        }

    async def execute(self, **kwargs: Any) -> str:
        from nanocodex.agent.skills_store import SkillsStore

        action = str(kwargs.get("action", "")).strip()
        store = SkillsStore()

        if action == "list":
            return self._render_list(store)
        if action == "install":
            return self._install(store, kwargs)
        if action == "show":
            return self._show(store, kwargs)
        if action == "remove":
            name = str(kwargs.get("name", "")).strip()
            if not name:
                return "Error: 'remove' needs a 'name'. Call action='list' to see names."
            if not store.remove(name):
                return f"Error: no skill named '{name}'."
            return f"Skill '{name}' removed."

        return f"Error: unknown action {action!r}. Use install/list/show/remove."

    def _install(self, store, kwargs: dict[str, Any]) -> str:
        name = str(kwargs.get("name", "")).strip()
        description = str(kwargs.get("description", "")).strip()
        body = str(kwargs.get("body", "") or "")
        overwrite = bool(kwargs.get("overwrite", False))
        if not name:
            return "Error: 'install' needs a 'name'."
        if not description:
            return "Error: 'install' needs a 'description'."
        try:
            skill = store.install(name, description, body, overwrite=overwrite)
        except ValueError as exc:
            return f"Error: {exc}"
        note = "" if body.strip() else (
            "\nNote: no body was given, so this skill is just a title. Re-install "
            "with a 'body' containing the full how-to instructions."
        )
        return (
            f"Skill '{skill.name}' installed at {skill.source}.\n"
            "It's now listed in the system prompt; its full instructions are read "
            "on demand via read_file." + note
        )

    def _show(self, store, kwargs: dict[str, Any]) -> str:
        name = str(kwargs.get("name", "")).strip()
        if not name:
            return "Error: 'show' needs a 'name'. Call action='list' to see names."
        skill = store.get(name)
        if skill is None:
            return f"Error: no skill named '{name}'."
        return (
            f"name: {skill.name}\n"
            f"description: {skill.description}\n"
            f"path: {skill.source}\n"
            f"---\n{skill.body or '(no body)'}"
        )

    def _render_list(self, store) -> str:
        collection = store.list()
        if collection.is_empty and not collection.warnings:
            return "No skills installed."
        lines: list[str] = []
        for s in collection.skills:
            lines.append(f"{s.name}: {s.description}")
        if collection.warnings:
            lines.append("")
            lines.append("Warnings (skipped):")
            for w in collection.warnings:
                lines.append(f"  - {w}")
        return "\n".join(lines) if lines else "No skills installed."
