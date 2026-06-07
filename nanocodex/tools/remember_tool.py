"""remember: let the model append a durable note to the user's memory file.

A "memory" is a persistent, user-level fact or preference the user wants kept
across every session and project — how they like replies phrased, names and
relationships, conventions. Distinct from skills ("how to do X") and AGENTS.md
(project-scoped). The stored notes are injected into the system prompt every
turn as a ``<user_memory>`` block (see agent/memory_store.py).

Use this when you notice a LASTING preference worth keeping — not transient
task state. Mirrors ManageScheduleTool / ManageSkillsTool: one tool wrapping
the pure MemoryStore, so a note saved here is loaded on the next prompt build.
"""

from __future__ import annotations

from typing import Any

from nanocodex.tools.base import Tool


class RememberTool(Tool):
    @property
    def name(self) -> str:
        return "remember"

    @property
    def description(self) -> str:
        return (
            "Save a durable, user-level fact or preference to long-term MEMORY, "
            "so it carries across every future session and project. Use this when "
            "the user states a lasting preference or fact worth remembering — how "
            "they like replies phrased, names/relationships, recurring "
            "conventions, tools they prefer ('always use pytest', 'my partner is "
            "called X', 'reply in Chinese'). The note is appended as a timestamped "
            "bullet to ~/.nanocodex/memory.md and shown in your system prompt every "
            "turn. Do NOT use it for transient task state or one-off details — only "
            "things that should outlive this conversation. Saving a memory grants "
            "no permissions and runs nothing; it just records text."
        )

    @property
    def parameters(self) -> dict[str, Any]:
        return {
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": (
                        "The fact or preference to remember, as a short standalone "
                        "sentence (e.g. 'User prefers replies in Chinese.'). It is "
                        "stored as one timestamped bullet."
                    ),
                },
            },
            "required": ["note"],
        }

    async def execute(self, **kwargs: Any) -> str:
        from nanocodex.agent.memory_store import MemoryStore

        note = str(kwargs.get("note", "")).strip()
        if not note:
            return "Error: 'remember' needs a non-empty 'note'."
        store = MemoryStore()
        try:
            bullet = store.append(note)
        except ValueError as exc:
            return f"Error: {exc}"
        return (
            f"Remembered: {bullet}\n"
            f"Saved to {store.path}. It will be in your memory every future session."
        )
