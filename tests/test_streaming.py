"""Tests for streaming: provider aggregation contract + loop streaming path."""

from __future__ import annotations

from pathlib import Path

from nanocodex.agent.loop import AgentLoop, LoopHooks
from nanocodex.agent.session import Session
from nanocodex.provider.base import ModelResponse, ToolCall
from nanocodex.sandbox.approval import ON_REQUEST, Approver
from nanocodex.sandbox.executor import make_executor
from nanocodex.sandbox.policy import WORKSPACE_WRITE, SandboxPolicy
from nanocodex.tools import ToolContext, ToolRegistry


class StreamingProvider:
    """Scripted provider that emits deltas via chat_stream, mirroring chat()."""

    supports_streaming = True
    model = "scripted-stream"

    def __init__(self, responses):
        # Each response is a tuple: (content_deltas: list[str], reasoning_deltas,
        # tool_calls, finish_reason)
        self._responses = list(responses)
        self.stream_calls = 0

    async def chat(self, messages, tools=None, **kwargs):
        # Non-streaming fallback: collapse deltas into a full response.
        c, r, tcs, fr = self._responses.pop(0)
        return ModelResponse("".join(c), tool_calls=list(tcs), finish_reason=fr, reasoning="".join(r))

    async def chat_stream(self, messages, tools=None, *, on_content_delta=None,
                          on_reasoning_delta=None, **kwargs):
        self.stream_calls += 1
        c, r, tcs, fr = self._responses.pop(0)
        for piece in r:
            if on_reasoning_delta:
                await on_reasoning_delta(piece)
        for piece in c:
            if on_content_delta:
                await on_content_delta(piece)
        return ModelResponse("".join(c), tool_calls=list(tcs), finish_reason=fr, reasoning="".join(r))


def _build(tmp_path: Path, provider) -> AgentLoop:
    policy = SandboxPolicy(mode=WORKSPACE_WRITE, workspace=tmp_path)

    async def auto_yes(_req):
        return True

    ctx = ToolContext(workspace=tmp_path, policy=policy,
                      approver=Approver(ON_REQUEST, auto_yes),
                      executor=make_executor(policy), plan=[])
    return AgentLoop(provider, ToolRegistry(ctx), Session("sys", log_path=None), max_iterations=10)


async def test_streaming_emits_content_deltas(tmp_path):
    provider = StreamingProvider([(["Hel", "lo", " world"], [], [], "stop")])
    loop = _build(tmp_path, provider)

    seen: list[str] = []
    ended = {"n": 0}

    async def on_delta(d): seen.append(d)
    async def on_end(): ended["n"] += 1

    hooks = LoopHooks(on_content_delta=on_delta, on_stream_end=on_end)
    result = await loop.run_turn("hi", hooks)

    assert provider.stream_calls == 1           # streaming path was taken
    assert seen == ["Hel", "lo", " world"]      # deltas arrived in order
    assert result.final_text == "Hello world"   # aggregate is correct
    assert ended["n"] == 1                       # stream_end fired once


async def test_streaming_separates_reasoning_and_content(tmp_path):
    provider = StreamingProvider([(["answer"], ["think ", "more"], [], "stop")])
    loop = _build(tmp_path, provider)
    reasoning: list[str] = []
    content: list[str] = []

    async def on_r(d): reasoning.append(d)
    async def on_c(d): content.append(d)

    hooks = LoopHooks(on_content_delta=on_c, on_reasoning_delta=on_r)
    result = await loop.run_turn("hi", hooks)
    assert "".join(reasoning) == "think more"
    assert "".join(content) == "answer"
    assert result.final_text == "answer"


async def test_no_streaming_when_hooks_absent(tmp_path):
    # Without streaming hooks, the loop must use the non-streaming chat() path.
    provider = StreamingProvider([(["hello"], [], [], "stop")])
    loop = _build(tmp_path, provider)
    result = await loop.run_turn("hi", LoopHooks())
    assert provider.stream_calls == 0
    assert result.final_text == "hello"


async def test_streaming_then_tool_call(tmp_path):
    patch = "*** Begin Patch\n*** Add File: s.txt\n+streamed\n*** End Patch"
    provider = StreamingProvider([
        (["working"], [], [ToolCall("c1", "apply_patch", {"patch": patch})], "tool_calls"),
        (["done"], [], [], "stop"),
    ])
    loop = _build(tmp_path, provider)
    deltas: list[str] = []

    async def on_c(d): deltas.append(d)

    result = await loop.run_turn("make s.txt", LoopHooks(on_content_delta=on_c))
    assert (tmp_path / "s.txt").read_text() == "streamed\n"
    assert provider.stream_calls == 2           # both model calls streamed
    assert result.final_text == "done"
    assert "apply_patch" in result.tools_used
