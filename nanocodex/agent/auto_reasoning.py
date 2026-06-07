"""Adaptive reasoning-effort selection for the ``auto`` tier.

Ported from DeepSeek-TUI's ``auto_reasoning.rs`` (#663), adapted to nanocodex.

nanocodex's config defaults ``reasoning_effort = "auto"``, but until now the
DeepSeek provider treated ``"auto"`` as a no-op (``_apply_reasoning_effort``
returns early on it), so every turn silently used the model's own default tier.
This module fills that gap: given the last user message, it picks a CONCRETE
tier the provider already understands (``max`` / ``low`` / ``high``):

* a hard/debugging request → ``max`` (spend the most thinking)
* a quick lookup/search → ``low`` (don't overthink a search)
* a sub-agent context → ``low`` (sub-agents stay cheap)
* everything else → ``high`` (a sensible default for real work)

The keyword tables deliberately include Chinese and Japanese, not just English,
because a non-English user typing "报错" or "调试" should get the same Max tier
an English user gets for "error"/"debug" — without this they silently got the
plain default. Pure functions over strings, so they unit-test offline.
"""

from __future__ import annotations

from typing import Any

# Keywords that bump the tier to ``max``. Latin terms are lowercase (the caller
# lowercases the message first); CJK has no case so literal forms match as-is.
_HIGH_EFFORT_KEYWORDS = (
    # English
    "debug",
    "error",
    "stack trace",
    "traceback",
    "crash",
    # Simplified / Traditional Chinese
    "调试",
    "错误",
    "报错",
    "出错",
    "崩溃",
    "調試",
    "錯誤",
    "排查",
    "为什么",  # "why ..." — usually a diagnosis ask
    # Japanese
    "デバッグ",
    "エラー",
    "バグ",
)

# Keywords that drop the tier to ``low`` (cheap lookups, not hard reasoning).
_LOW_EFFORT_KEYWORDS = (
    # English
    "search",
    "lookup",
    "look up",
    "find",
    # Chinese
    "搜索",
    "查找",
    "查询",
    "搜一下",
    # Japanese
    "検索",
)

# Concrete tiers the DeepSeek provider's _apply_reasoning_effort understands.
EFFORT_MAX = "max"
EFFORT_HIGH = "high"
EFFORT_LOW = "low"


def select_auto_effort(last_msg: str, *, is_subagent: bool = False) -> str:
    """Pick a concrete reasoning tier for the next request (pure).

    Returns one of ``"max"`` / ``"high"`` / ``"low"``. Mirrors the rule order of
    DeepSeek-TUI's ``select``: sub-agent first, then high-effort keywords, then
    low-effort keywords, else the ``high`` default.
    """
    if is_subagent:
        return EFFORT_LOW
    lower = (last_msg or "").lower()
    if any(kw in lower for kw in _HIGH_EFFORT_KEYWORDS):
        return EFFORT_MAX
    if any(kw in lower for kw in _LOW_EFFORT_KEYWORDS):
        return EFFORT_LOW
    return EFFORT_HIGH


def _content_to_text(content: "str | list[dict[str, Any]] | None") -> str:
    """Flatten a message ``content`` (str or content-block list) to plain text."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: list[str] = []
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                parts.append(str(block.get("text", "")))
        return " ".join(p for p in parts if p)
    return ""


def last_user_text(messages: list[dict[str, Any]]) -> str:
    """Return the text of the most recent ``user`` message, or "" if none.

    Tolerates both plain-string content and the content-block list shape used
    when images are attached.
    """
    for msg in reversed(messages):
        if msg.get("role") == "user":
            return _content_to_text(msg.get("content")).strip()
    return ""


def resolve_effort(
    configured: str | None,
    messages: list[dict[str, Any]],
    *,
    is_subagent: bool = False,
) -> str | None:
    """Map a configured effort to a concrete tier when it's ``"auto"``.

    * ``configured`` is None/empty → return it unchanged (provider default).
    * ``configured`` is an explicit tier (``high``/``max``/``off``/…) → unchanged,
      so a user who pins a tier keeps it.
    * ``configured`` is ``"auto"`` → pick a concrete tier from the last user
      message via :func:`select_auto_effort`.
    """
    if not configured or configured.strip().lower() != "auto":
        return configured
    return select_auto_effort(last_user_text(messages), is_subagent=is_subagent)
