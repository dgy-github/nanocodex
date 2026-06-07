"""Tests for web_search: network-policy gating + result formatting (offline)."""

from __future__ import annotations

from pathlib import Path

from nanocodex.sandbox.approval import NEVER, ON_REQUEST, Approver
from nanocodex.sandbox.executor import make_executor
from nanocodex.sandbox.policy import DANGER_FULL_ACCESS, WORKSPACE_WRITE, SandboxPolicy
from nanocodex.tools import ToolContext
from nanocodex.tools.web_search import WebSearchTool

_FAKE_RESULTS = [
    {"title": "First", "href": "https://a.example", "body": "first snippet"},
    {"title": "Second", "href": "https://b.example", "body": "second snippet"},
]


def _ctx(tmp_path: Path, *, network: bool, approver: Approver) -> ToolContext:
    policy = SandboxPolicy(
        mode=DANGER_FULL_ACCESS if network else WORKSPACE_WRITE,
        workspace=tmp_path,
        network_access=network,
    )
    return ToolContext(
        workspace=tmp_path, policy=policy, approver=approver,
        executor=make_executor(policy), plan=[],
    )


def _fake_search(query, max_results):
    return _FAKE_RESULTS[:max_results]


async def test_search_runs_when_network_enabled(tmp_path):
    async def cb(req):
        raise AssertionError("must not prompt when network is enabled")

    ctx = _ctx(tmp_path, network=True, approver=Approver(ON_REQUEST, cb))
    tool = WebSearchTool(ctx, search_fn=_fake_search)
    out = await tool.execute(query="python asyncio", max_results=2)
    assert "First" in out and "https://a.example" in out
    assert "Second" in out


async def test_search_asks_when_network_disabled_and_approved(tmp_path):
    asked = []

    async def cb(req):
        asked.append(req)
        return True

    ctx = _ctx(tmp_path, network=False, approver=Approver(ON_REQUEST, cb))
    tool = WebSearchTool(ctx, search_fn=_fake_search)
    out = await tool.execute(query="q")
    assert len(asked) == 1
    assert asked[0].escalated is True
    assert "First" in out


async def test_search_blocked_when_denied(tmp_path):
    async def cb(req):
        return False

    ctx = _ctx(tmp_path, network=False, approver=Approver(ON_REQUEST, cb))
    tool = WebSearchTool(ctx, search_fn=_fake_search)
    out = await tool.execute(query="q")
    assert "not approved" in out


async def test_search_auto_denied_under_never_policy(tmp_path):
    async def cb(req):
        raise AssertionError("never policy must not prompt")

    ctx = _ctx(tmp_path, network=False, approver=Approver(NEVER, cb))
    tool = WebSearchTool(ctx, search_fn=_fake_search)
    out = await tool.execute(query="q")
    assert "denied by approval policy 'never'" in out


async def test_search_handles_no_results(tmp_path):
    async def cb(req):
        return True

    ctx = _ctx(tmp_path, network=True, approver=Approver(ON_REQUEST, cb))
    tool = WebSearchTool(ctx, search_fn=lambda q, n: [])
    out = await tool.execute(query="nothing")
    assert "No results" in out


async def test_search_requires_query(tmp_path):
    async def cb(req):
        return True

    ctx = _ctx(tmp_path, network=True, approver=Approver(ON_REQUEST, cb))
    tool = WebSearchTool(ctx, search_fn=_fake_search)
    out = await tool.execute(query="")
    assert out.startswith("Error")
