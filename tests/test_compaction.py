"""Tests for context compaction (deterministic, zero-cost path)."""

from __future__ import annotations

from nanocodex.agent.compaction import (
    CompactionConfig,
    compact,
    estimate_tokens,
)


def _sys():
    return {"role": "system", "content": "SYS"}


def _u(text):
    return {"role": "user", "content": text}


def _a(text):
    return {"role": "assistant", "content": text}


async def test_disabled_returns_original():
    msgs = [_sys(), _u("hi"), _a("yo")]
    out = await compact(msgs, CompactionConfig(token_budget=0))
    assert out is msgs


async def test_under_budget_returns_original():
    msgs = [_sys(), _u("hi"), _a("yo")]
    # Huge budget -> nothing to do.
    out = await compact(msgs, CompactionConfig(token_budget=100_000))
    assert out is msgs


async def test_over_budget_folds_middle_and_keeps_system():
    # Build a long history that exceeds a tiny budget.
    body = []
    for i in range(40):
        body.append(_u(f"user message number {i} with some padding text"))
        body.append(_a(f"assistant reply number {i} with some padding text"))
    msgs = [_sys(), *body]
    cfg = CompactionConfig(token_budget=50, keep_recent=6)

    out = await compact(msgs, cfg)
    assert out is not msgs
    assert out[0] == _sys()                       # system preserved at front
    assert out[1]["role"] == "user"               # summary injected as user msg
    assert "compacted" in out[1]["content"]       # deterministic digest
    assert len(out) < len(msgs)                    # actually shrank
    # The kept tail starts at a user message (after system + summary).
    assert out[2]["role"] == "user"


async def test_kept_tail_starts_at_user_not_midpair():
    # Construct history where the naive cut would land on a tool result.
    body = [
        _u("start"),
        _a("step 1"),
        _u("again"),
        {"role": "assistant", "content": "", "tool_calls": [
            {"id": "c1", "type": "function", "function": {"name": "shell", "arguments": "{}"}}]},
        {"role": "tool", "tool_call_id": "c1", "name": "shell", "content": "x" * 400},
        _u("more"),
        _a("step 2"),
        _u("latest question here"),
        _a("latest answer here"),
    ]
    msgs = [_sys(), *body]
    cfg = CompactionConfig(token_budget=10, keep_recent=3)
    out = await compact(msgs, cfg)

    # First non-system, non-summary message must be a user role (clean cut).
    tail = out[2:]
    assert tail[0]["role"] == "user"
    # No orphaned tool messages survive in the tail.
    declared = {
        tc["id"]
        for m in tail if m.get("role") == "assistant"
        for tc in m.get("tool_calls") or []
    }
    for m in tail:
        if m.get("role") == "tool":
            assert m["tool_call_id"] in declared


async def test_summarizer_hook_used_when_provided():
    calls = {"n": 0}

    async def fake_summarizer(folded):
        calls["n"] += 1
        return "LLM SUMMARY"

    body = [_u(f"m{i} padding padding padding") for i in range(30)]
    msgs = [_sys(), *body, _u("final user msg")]
    cfg = CompactionConfig(token_budget=20, keep_recent=4, summarizer=fake_summarizer)
    out = await compact(msgs, cfg)
    assert calls["n"] == 1
    assert out[1]["content"] == "LLM SUMMARY"


def test_estimate_tokens_counts_content_and_tool_calls():
    msgs = [
        {"role": "user", "content": "abcd" * 4},  # 16 chars
        {"role": "assistant", "content": "", "tool_calls": [
            {"id": "c1", "function": {"name": "shell", "arguments": "{}"}}]},
    ]
    est = estimate_tokens(msgs)
    assert est > 0


def test_estimate_tokens_uses_calibrated_ratio():
    # Lock the chars/token ratio so it can't silently drift back to the old 4
    # (which under-counted Chinese badly). At 2 chars/token:
    #   msg1: 16 content chars + 8 framing            = 24
    #   msg2: 0 content + ("shell"=5 + "{}"=2) + 8     = 15
    #   total 39 chars // 2                            = 19
    msgs = [
        {"role": "user", "content": "abcd" * 4},
        {"role": "assistant", "content": "", "tool_calls": [
            {"id": "c1", "function": {"name": "shell", "arguments": "{}"}}]},
    ]
    assert estimate_tokens(msgs) == 19
