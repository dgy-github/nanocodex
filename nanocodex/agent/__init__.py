"""Agent layer: prompt, session, and the turn loop."""

from nanocodex.agent.agents_md import AgentsInstructions, discover_agents
from nanocodex.agent.compaction import CompactionConfig, compact, estimate_tokens
from nanocodex.agent.loop import AgentLoop, LoopHooks, TurnResult
from nanocodex.agent.prompt import build_system_prompt
from nanocodex.agent.session import Session

__all__ = [
    "AgentLoop",
    "LoopHooks",
    "TurnResult",
    "build_system_prompt",
    "Session",
    "AgentsInstructions",
    "discover_agents",
    "CompactionConfig",
    "compact",
    "estimate_tokens",
]
