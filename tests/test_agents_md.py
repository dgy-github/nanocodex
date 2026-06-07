"""Tests for AGENTS.md discovery and system-prompt injection."""

from __future__ import annotations

from pathlib import Path

from nanocodex.agent.agents_md import discover_agents
from nanocodex.agent.prompt import build_system_prompt
from nanocodex.sandbox.policy import WORKSPACE_WRITE, SandboxPolicy


def _no_global(tmp_path: Path) -> Path:
    return tmp_path / "no_global_AGENTS.md"


def test_no_agents_returns_empty(tmp_path):
    ws = tmp_path / "ws"
    ws.mkdir()
    result = discover_agents(ws, global_path=_no_global(tmp_path))
    assert result.is_empty
    assert result.render() == ""


def test_discovers_workspace_agents(tmp_path):
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "AGENTS.md").write_text("Use tabs, not spaces.\n")
    result = discover_agents(ws, global_path=_no_global(tmp_path))
    assert not result.is_empty
    assert "Use tabs, not spaces." in result.render()


def test_layers_global_then_local(tmp_path):
    gp = tmp_path / "global_AGENTS.md"
    gp.write_text("GLOBAL RULE\n")
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "AGENTS.md").write_text("LOCAL RULE\n")
    result = discover_agents(ws, global_path=gp)
    rendered = result.render()
    # Global appears before local (outermost first).
    assert rendered.index("GLOBAL RULE") < rendered.index("LOCAL RULE")


def test_layers_parent_dirs_up_to_git_root(tmp_path):
    root = tmp_path / "repo"
    root.mkdir()
    (root / ".git").mkdir()
    (root / "AGENTS.md").write_text("ROOT RULE\n")
    sub = root / "pkg" / "sub"
    sub.mkdir(parents=True)
    (sub / "AGENTS.md").write_text("SUB RULE\n")

    result = discover_agents(sub, global_path=_no_global(tmp_path))
    rendered = result.render()
    assert "ROOT RULE" in rendered
    assert "SUB RULE" in rendered
    # Root (outermost) comes before the nested one.
    assert rendered.index("ROOT RULE") < rendered.index("SUB RULE")


def test_blank_agents_file_ignored(tmp_path):
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "AGENTS.md").write_text("   \n\n")
    result = discover_agents(ws, global_path=_no_global(tmp_path))
    assert result.is_empty


def test_system_prompt_includes_agents(tmp_path):
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "AGENTS.md").write_text("ALWAYS run ruff before committing.\n")
    agents = discover_agents(ws, global_path=_no_global(tmp_path))
    policy = SandboxPolicy(mode=WORKSPACE_WRITE, workspace=ws)
    prompt = build_system_prompt(policy, "on-request", agents)
    assert "Project instructions (AGENTS.md)" in prompt
    assert "ALWAYS run ruff before committing." in prompt


def test_system_prompt_without_agents_has_no_section(tmp_path):
    ws = tmp_path / "ws"
    ws.mkdir()
    agents = discover_agents(ws, global_path=_no_global(tmp_path))
    policy = SandboxPolicy(mode=WORKSPACE_WRITE, workspace=ws)
    prompt = build_system_prompt(policy, "on-request", agents)
    assert "Project instructions (AGENTS.md)" not in prompt
