"""DeepSeek provider request-shaping tests."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from nanocodex.provider.deepseek import (
    DeepSeekProvider,
    ProviderError,
    _extract_reasoning,
    _stream_open_timeout_s,
)


def _provider(model: str = "deepseek-v4-pro") -> DeepSeekProvider:
    return DeepSeekProvider(api_key="sk-test", base_url="https://example.invalid", model=model)


def test_provider_replays_reasoning_placeholder_for_deepseek_tool_history():
    provider = _provider("deepseek-v4-pro")
    messages = [
        {"role": "user", "content": "read a file"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "c1",
                "type": "function",
                "function": {"name": "read_file", "arguments": "{}"},
            }],
        },
    ]

    kwargs = provider._build_kwargs(messages, tools=None, temperature=None, max_tokens=None)

    assistant = kwargs["messages"][1]
    assert assistant["reasoning_content"] == "(reasoning omitted)"
    assert "reasoning_content" not in messages[1]  # original history is not mutated


def test_provider_preserves_existing_reasoning_content():
    provider = _provider("deepseek-chat")
    messages = [
        {
            "role": "assistant",
            "content": "",
            "reasoning_content": "real reasoning",
            "tool_calls": [{
                "id": "c1",
                "type": "function",
                "function": {"name": "read_file", "arguments": "{}"},
            }],
        },
    ]

    kwargs = provider._build_kwargs(messages, tools=None, temperature=None, max_tokens=None)

    assert kwargs["messages"][0]["reasoning_content"] == "real reasoning"


def test_provider_does_not_replay_reasoning_when_effort_disabled():
    provider = _provider("deepseek-v4-pro")
    messages = [
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "c1",
                "type": "function",
                "function": {"name": "read_file", "arguments": "{}"},
            }],
        },
    ]

    kwargs = provider._build_kwargs(
        messages,
        tools=None,
        temperature=None,
        max_tokens=None,
        reasoning_effort="off",
    )

    assert "reasoning_content" not in kwargs["messages"][0]
    assert kwargs["extra_body"]["thinking"] == {"type": "disabled"}


def test_provider_maps_reasoning_effort_to_deepseek_beta_body():
    provider = _provider("deepseek-v4-pro")
    kwargs = provider._build_kwargs(
        [{"role": "user", "content": "think"}],
        tools=None,
        temperature=None,
        max_tokens=None,
        reasoning_effort="max",
    )

    assert kwargs["extra_body"] == {
        "reasoning_effort": "max",
        "thinking": {"type": "enabled"},
    }


def test_extract_reasoning_accepts_reasoning_alias():
    assert _extract_reasoning(SimpleNamespace(reasoning="proxy reasoning")) == "proxy reasoning"


def test_stream_open_timeout_defaults_when_unset(monkeypatch):
    monkeypatch.delenv("NANOCODEX_STREAM_OPEN_TIMEOUT_S", raising=False)
    assert _stream_open_timeout_s() == 45


def test_stream_open_timeout_honors_env_within_bounds(monkeypatch):
    monkeypatch.setenv("NANOCODEX_STREAM_OPEN_TIMEOUT_S", "90")
    assert _stream_open_timeout_s() == 90


def test_stream_open_timeout_clamps_and_tolerates_garbage(monkeypatch):
    monkeypatch.setenv("NANOCODEX_STREAM_OPEN_TIMEOUT_S", "9999")
    assert _stream_open_timeout_s() == 300
    monkeypatch.setenv("NANOCODEX_STREAM_OPEN_TIMEOUT_S", "0")
    assert _stream_open_timeout_s() == 5
    monkeypatch.setenv("NANOCODEX_STREAM_OPEN_TIMEOUT_S", "notanint")
    assert _stream_open_timeout_s() == 45


async def test_chat_stream_raises_provider_error_on_open_timeout(monkeypatch):
    """A stalled header wait fails fast as ProviderError, not a silent hang."""
    monkeypatch.setenv("NANOCODEX_STREAM_OPEN_TIMEOUT_S", "5")
    provider = _provider("deepseek-v4-pro")

    async def _never_returns(**_kwargs):
        await asyncio.sleep(3600)

    # Replace the real timeout with a tiny one so the test is instant, and make
    # create() hang so wait_for trips.
    monkeypatch.setattr(
        "nanocodex.provider.deepseek._stream_open_timeout_s", lambda: 0.01
    )
    monkeypatch.setattr(provider._client.chat.completions, "create", _never_returns)

    with pytest.raises(ProviderError) as excinfo:
        await provider.chat_stream([{"role": "user", "content": "hi"}])
    assert "no streaming response headers" in str(excinfo.value)
