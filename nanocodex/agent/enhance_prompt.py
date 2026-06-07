"""Input prompt enhancement: rewrite a user's raw input into a clearer prompt.

The GUI's ✨ button takes whatever the user typed and, before sending it as a
turn, asks the model to rewrite it into a better-structured prompt — explicit
goal, concrete sub-steps, no lost intent. The user PREVIEWS the rewrite and
chooses to use it, keep the original, or cancel (so a rewrite never silently
replaces their words).

Design (mirrors auto_reasoning.py / memory_store.py): the message construction
and response cleanup are PURE functions over strings, unit-tested offline with
no Tk and no network. The actual model call is one `provider.chat(...)` made on
the GUI's worker thread (see gui.py `_run_enhance_thread`).

Why a dedicated system prompt rather than the agent's: enhancement is a
text-transformation task, not an agent turn — we don't want tools, planning, or
the coding-agent persona. A tight instruction keeps the rewrite faithful and
short, and explicitly forbids the model from ANSWERING the request (a common
failure: the model "helpfully" solves it instead of rewriting it).
"""

from __future__ import annotations

from typing import Any

# Keep the enhancer cheap and bounded: a huge paste shouldn't be sent to the
# rewrite call (it's a structuring pass, not a summarizer). Longer inputs are
# returned unchanged by `should_enhance` so the caller skips the model entirely.
_MAX_ENHANCE_CHARS = 4000

_SYSTEM = (
    "You rewrite a user's rough request into a single, clearer, better-"
    "structured prompt for a coding agent. Rules:\n"
    "- PRESERVE the user's intent and every concrete detail (names, paths, "
    "numbers, constraints). Never invent requirements they didn't state.\n"
    "- Make the goal explicit; if the task has natural steps, list them as a "
    "short ordered list. Keep it concise — no preamble, no padding.\n"
    "- Keep the user's original language (if they wrote Chinese, rewrite in "
    "Chinese).\n"
    "- Do NOT answer or solve the request. Output ONLY the rewritten prompt, "
    "nothing else — no quotes, no explanation, no 'Here is'."
)


def should_enhance(text: str) -> bool:
    """True when *text* is worth sending to the rewrite model.

    Skips empty/whitespace, slash-ish meta lines, and over-long inputs (a big
    paste is already explicit; rewriting it just burns tokens). Pure.
    """
    t = (text or "").strip()
    if not t:
        return False
    if len(t) > _MAX_ENHANCE_CHARS:
        return False
    return True


def build_enhance_messages(text: str) -> list[dict[str, Any]]:
    """Build the chat messages for one rewrite call (pure).

    A fixed system instruction plus the raw user text as the only user turn.
    No tools, no history — enhancement is a stateless transformation.
    """
    return [
        {"role": "system", "content": _SYSTEM},
        {"role": "user", "content": (text or "").strip()},
    ]


def clean_enhanced(raw: str, *, original: str) -> str:
    """Tidy the model's rewrite, falling back to *original* when it's unusable.

    Strips surrounding whitespace and a single layer of wrapping quotes/fences
    the model sometimes adds despite instructions. Returns *original* (stripped)
    when the result is empty, so a failed/blank rewrite never sends emptiness.
    Pure.
    """
    text = (raw or "").strip()
    text = _strip_code_fence(text)
    text = _strip_wrapping_quotes(text)
    text = text.strip()
    return text or (original or "").strip()


def _strip_code_fence(text: str) -> str:
    """Remove a single ``` fenced block wrapper if the whole text is fenced."""
    if not text.startswith("```"):
        return text
    lines = text.splitlines()
    if len(lines) < 2 or not lines[-1].strip().startswith("```"):
        return text
    # Drop the opening fence (with any language tag) and the closing fence.
    return "\n".join(lines[1:-1]).strip()


def _strip_wrapping_quotes(text: str) -> str:
    """Remove one layer of matching surrounding quotes, if the whole is wrapped."""
    if len(text) >= 2 and text[0] == text[-1] and text[0] in ("'", '"', "“", "「"):
        inner = text[1:-1].strip()
        # Only unwrap when there's no other quote of the same kind inside, so we
        # don't mangle a legitimately quoted phrase mid-text.
        if text[0] not in inner:
            return inner
    # Handle the smart-quote / bracket pairs whose open != close.
    pairs = {"“": "”", "「": "」"}
    if len(text) >= 2 and text[0] in pairs and text[-1] == pairs[text[0]]:
        return text[1:-1].strip()
    return text
