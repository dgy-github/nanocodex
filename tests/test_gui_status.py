"""Tests for the GUI status-bar text builder (pure, no Tk needed)."""

from __future__ import annotations

from nanocodex.gui import (
    _approval_short_circuit,
    _build_status,
    _fmt_tok,
    _fmt_usd,
    _is_mcp_command,
)


def test_fmt_tok():
    assert _fmt_tok(666) == "666"
    assert _fmt_tok(12345) == "12.3k"
    assert _fmt_tok(1_000_000) == "1.0M"


def test_status_shows_model_and_context():
    s = _build_status(busy=False, auto_on=False, model="deepseek-v4-pro",
                      tokens=666, window=65536, budget=0)
    assert "ready" in s
    assert "deepseek-v4-pro" in s          # model name shows
    assert "context: 666 / 65.5k (1%)" in s  # used / window (pct)


def test_status_working_state():
    s = _build_status(busy=True, auto_on=False, model="m", tokens=10, window=1000)
    assert s.startswith("working…")


def test_status_auto_approve_flag():
    s = _build_status(busy=False, auto_on=True, model="m", tokens=10, window=1000)
    assert "auto-approve: ON" in s


def test_status_shows_compaction_budget():
    s = _build_status(busy=False, auto_on=False, model="m",
                      tokens=10, window=1000, budget=6000)
    assert "compact @ 6.0k" in s


def test_status_error_instead_of_blank():
    # The bug we fixed: a failed loop must show the reason, never blank.
    s = _build_status(busy=False, auto_on=False, error="No API key found")
    assert "error: No API key found" in s
    assert "ready" in s


def test_status_no_window_omits_percentage():
    s = _build_status(busy=False, auto_on=False, model="m", tokens=42, window=0)
    assert "context: 42" in s
    assert "%" not in s


# --- cost readout ----------------------------------------------------------


def test_fmt_usd_sub_cent_keeps_precision():
    # A flat $0.00 would hide every cheap (cache-hit) turn — sub-$1 shows 4dp.
    assert _fmt_usd(0.0123) == "$0.0123"
    assert _fmt_usd(0.00005) == "$0.0001"   # rounds, still non-zero looking
    assert _fmt_usd(0) == "$0.00"
    assert _fmt_usd(2.5) == "$2.50"
    assert _fmt_usd(1234.5) == "$1,234.50"


def test_status_shows_session_cost_when_positive():
    s = _build_status(busy=False, auto_on=False, model="m",
                      tokens=10, window=1000, session_cost=0.0123)
    assert "cost: $0.0123" in s


def test_status_omits_cost_when_zero_or_none():
    # A fresh session with no priced turns shows nothing, not "cost: $0.00".
    s_none = _build_status(busy=False, auto_on=False, model="m",
                           tokens=10, window=1000, session_cost=None)
    s_zero = _build_status(busy=False, auto_on=False, model="m",
                           tokens=10, window=1000, session_cost=0.0)
    assert "cost:" not in s_none
    assert "cost:" not in s_zero


# --- approval short-circuit (Codex "approve for session") -----------------

def test_is_mcp_command():
    assert _is_mcp_command("mcp__windows_computer_use__send_wechat_message")
    assert _is_mcp_command("mcp__fs__write_file")
    assert not _is_mcp_command("echo hello")
    assert not _is_mcp_command("git commit")


def test_short_circuit_auto_approve_runs_everything():
    # Global auto-approve ON: even a shell command skips the dialog.
    assert _approval_short_circuit(
        "rm -rf build", auto_approve_on=True,
        allow_all_mcp=False, always_allow=set(),
    )


def test_short_circuit_allow_all_mcp_covers_any_desktop_action():
    # The fix: one "allow all desktop (session)" click frees EVERY later MCP
    # action — focus, click, type, press — even though each has a different
    # tool name. This is what stops the "click Allow, it prompts again" loop.
    for tool in ("focus_window", "click_xy", "type_text", "press_keys"):
        cmd = f"mcp__windows_computer_use__{tool}"
        assert _approval_short_circuit(
            cmd, auto_approve_on=False,
            allow_all_mcp=True, always_allow=set(),
        ), f"{tool} should be auto-approved once session-allow is on"


def test_short_circuit_allow_all_mcp_does_not_free_shell():
    # "Allow all desktop" is scoped to MCP only; a shell command still prompts.
    assert not _approval_short_circuit(
        "echo hi", auto_approve_on=False,
        allow_all_mcp=True, always_allow=set(),
    )


def test_short_circuit_always_allow_is_exact_command():
    # The narrower "always allow THIS command" memory (used for shell).
    assert _approval_short_circuit(
        "echo hi", auto_approve_on=False,
        allow_all_mcp=False, always_allow={"echo hi"},
    )
    assert not _approval_short_circuit(
        "echo bye", auto_approve_on=False,
        allow_all_mcp=False, always_allow={"echo hi"},
    )


def test_short_circuit_default_prompts():
    # Nothing enabled: the request must go to the dialog (returns False).
    assert not _approval_short_circuit(
        "mcp__windows_computer_use__click_xy", auto_approve_on=False,
        allow_all_mcp=False, always_allow=set(),
    )
