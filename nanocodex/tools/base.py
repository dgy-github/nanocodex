"""Tool base classes and the execution context shared across tools."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from nanocodex.sandbox.approval import Approver
from nanocodex.sandbox.executor import PolicyExecutor
from nanocodex.sandbox.policy import SandboxPolicy


@dataclass
class ToolContext:
    """Everything a tool needs to do its job, injected at registration."""

    workspace: Path
    policy: SandboxPolicy
    approver: Approver
    executor: PolicyExecutor
    timeout_s: int = 120
    # Shared mutable plan state for update_plan / the CLI to read.
    plan: list[dict[str, str]] | None = None
    # When True, write actions (shell / apply_patch) prompt for approval on
    # EVERY step — even inside the sandbox. This is the "confirm each step"
    # mode the GUI's auto-approve toggle flips (auto-approve OFF -> True). It's
    # a plain bool the worker thread can read/flip safely (atomic in CPython).
    require_step_approval: bool = False
    # Running total of Seedance video spend (CNY) for this session. The
    # StoryboardTool adds each render's cost here so the GUI can show it in the
    # status bar. Kept separate from the USD turn cost (no FX rate is invented):
    # Seedance bills in CNY on a different axis than the text models.
    seedance_cost_cny: float = 0.0


class Tool(ABC):
    """An agent capability exposed to the model as an OpenAI function tool."""

    def __init__(self, ctx: ToolContext) -> None:
        self.ctx = ctx

    @property
    @abstractmethod
    def name(self) -> str: ...

    @property
    @abstractmethod
    def description(self) -> str: ...

    @property
    @abstractmethod
    def parameters(self) -> dict[str, Any]:
        """JSON Schema for the arguments object."""

    @abstractmethod
    async def execute(self, **kwargs: Any) -> str:
        """Run the tool and return a string result for the model."""

    def to_schema(self) -> dict[str, Any]:
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        }
