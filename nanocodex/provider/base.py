"""Provider protocol and shared response types."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol


@dataclass
class ToolCall:
    """A single tool invocation requested by the model."""

    id: str
    name: str
    arguments: dict[str, Any]


@dataclass
class ModelResponse:
    """Normalized result of one model call."""

    content: str
    tool_calls: list[ToolCall] = field(default_factory=list)
    finish_reason: str = "stop"
    reasoning: str = ""
    usage: dict[str, int] = field(default_factory=dict)

    @property
    def has_tool_calls(self) -> bool:
        return bool(self.tool_calls)


class Provider(Protocol):
    """Minimal async chat interface every backend must implement."""

    model: str

    async def chat(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        *,
        temperature: float | None = None,
        max_tokens: int | None = None,
        reasoning_effort: str | None = None,
    ) -> ModelResponse:
        ...
