"""Context compaction: keep the prompt within a token budget.

Long conversations grow without bound; once the message list exceeds the
model's context window the backend truncates or rejects it. This module folds
the *middle* of the history into a compact summary while preserving:

* the system message (always first), and
* a recent tail of messages large enough to keep the task coherent.

Two strategies share one interface:

* ``deterministic`` (default, ZERO API cost): the folded middle becomes a
  factual, rule-based digest (counts + a few recent tool names). No model call.
* ``summarizer`` (opt-in, COSTS tokens): a caller-supplied async function turns
  the middle into prose. Off unless explicitly wired, to honor the zero-cost
  default.

Hard correctness rule mirrored from the backend's contract: the kept tail must
start at a ``user`` message, never mid tool-call/tool-result pair, or the next
request is rejected. We also strip any tool messages that would be orphaned by
the cut.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Awaitable, Callable

# Approx chars/token for the trigger estimate only (never billing). DeepSeek's
# docs give ~1.67 Chinese chars/token (0.6 token/char) and ~3.3 English
# chars/token (0.3 token/char). The old flat 4 badly UNDER-counted Chinese
# (chat here is mostly Chinese), so a long zh conversation looked half its real
# token size and compaction triggered too late. 2 is a deliberately
# Chinese-leaning middle: close for zh, slightly conservative (over-counts) for
# English — which is the safe direction for a "when to fold" trigger.
_CHARS_PER_TOKEN = 2

Summarizer = Callable[[list[dict[str, Any]]], Awaitable[str]]


@dataclass
class CompactionConfig:
    """When and how to compact."""

    # Approximate token budget for the whole prompt. 0 disables compaction.
    token_budget: int = 0
    # Always keep at least this many of the most recent messages.
    keep_recent: int = 12
    # Optional model-backed summarizer (opt-in; costs tokens when set).
    summarizer: Summarizer | None = None

    @property
    def enabled(self) -> bool:
        return self.token_budget > 0


def estimate_tokens(messages: list[dict[str, Any]]) -> int:
    """Cheap, deterministic token estimate (no tokenizer, no network)."""
    total = 0
    for m in messages:
        content = m.get("content")
        if isinstance(content, str):
            total += len(content)
        elif isinstance(content, list):
            for block in content:
                if isinstance(block, dict):
                    total += len(str(block.get("text", "")))
        for tc in m.get("tool_calls") or []:
            fn = tc.get("function") or {}
            total += len(str(fn.get("name", ""))) + len(str(fn.get("arguments", "")))
        total += 8  # per-message role/framing overhead
    return total // _CHARS_PER_TOKEN


def _first_user_index(messages: list[dict[str, Any]]) -> int | None:
    for i, m in enumerate(messages):
        if m.get("role") == "user":
            return i
    return None


def _drop_orphan_tools(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Remove tool messages whose tool_call_id has no preceding assistant call."""
    declared: set[str] = set()
    out: list[dict[str, Any]] = []
    for m in messages:
        if m.get("role") == "assistant":
            for tc in m.get("tool_calls") or []:
                if tc.get("id"):
                    declared.add(str(tc["id"]))
        if m.get("role") == "tool":
            tid = m.get("tool_call_id")
            if not tid or str(tid) not in declared:
                continue  # orphan: drop it
        out.append(m)
    return out


def _digest(folded: list[dict[str, Any]]) -> str:
    """Deterministic, zero-cost summary of the folded middle."""
    users = sum(1 for m in folded if m.get("role") == "user")
    assistants = sum(1 for m in folded if m.get("role") == "assistant")
    tool_names: list[str] = []
    for m in folded:
        if m.get("role") == "assistant":
            for tc in m.get("tool_calls") or []:
                name = (tc.get("function") or {}).get("name")
                if name:
                    tool_names.append(name)
    recent_tools = ", ".join(tool_names[-8:]) if tool_names else "none"
    return (
        "[Earlier conversation compacted to stay within the context budget. "
        f"Folded {len(folded)} message(s): {users} user, {assistants} assistant, "
        f"{len(tool_names)} tool call(s). Most recent tools: {recent_tools}. "
        "Ask the user if you need details that were summarized away.]"
    )


async def compact(
    messages: list[dict[str, Any]],
    config: CompactionConfig,
) -> list[dict[str, Any]]:
    """Return a compacted copy of *messages* if over budget, else the original.

    Structure of the result: [system?, (summary user msg), <recent tail>].
    The tail always begins at a user message and contains no orphaned tools.
    """
    if not config.enabled:
        return messages
    if estimate_tokens(messages) <= config.token_budget:
        return messages

    system = [m for m in messages if m.get("role") == "system"]
    body = [m for m in messages if m.get("role") != "system"]
    if len(body) <= config.keep_recent:
        return messages  # nothing safe to fold

    # Choose a tail of at least keep_recent that starts at a user message.
    cut = len(body) - config.keep_recent
    tail_start = None
    for i in range(cut, len(body)):
        if body[i].get("role") == "user":
            tail_start = i
            break
    if tail_start is None or tail_start == 0:
        return messages  # no clean cut point; leave history intact

    folded = body[:tail_start]
    tail = _drop_orphan_tools(body[tail_start:])

    if config.summarizer is not None:
        summary_text = await config.summarizer(folded)
    else:
        summary_text = _digest(folded)

    summary_msg = {"role": "user", "content": summary_text}
    return [*system, summary_msg, *tail]
