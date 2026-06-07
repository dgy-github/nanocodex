"""Tests for the GUI settings-window pure helpers (no Tk needed).

The Codex-style settings window (left nav + right section) keeps its data
logic in small pure functions so the navigation order and the
"collect updates" rules can be unit-tested without a display.
"""

from __future__ import annotations

from nanocodex.gui import (
    _REASONING_CHOICES,
    _collect_schedule_add,
    _collect_settings_updates,
    _format_schedule_recurrence,
    _settings_sections,
)


def test_settings_sections_order_stable():
    secs = _settings_sections()
    assert secs == (
        "General", "Config", "MCP servers", "Marketplace", "Scheduled tasks",
        "Desktop",
    )
    # First section is what the window shows by default — keep it General.
    assert secs[0] == "General"


def test_reasoning_choices_match_provider_buckets():
    # These four map to distinct behaviors in provider/deepseek.py:
    #   auto -> defer, max -> reasoning_effort=max, high -> high, off -> disabled
    assert _REASONING_CHOICES == ("auto", "max", "high", "off")


def test_collect_updates_blank_key_is_omitted():
    # An empty new-key submit must mean "keep the existing key", never wipe it.
    updates = _collect_settings_updates(
        api_key="", base_url="https://x", model="m",
        sandbox_mode="read-only", approval_policy="on-request",
        reasoning_effort="auto",
    )
    assert "api_key" not in updates
    assert updates["base_url"] == "https://x"
    assert updates["model"] == "m"
    assert updates["sandbox_mode"] == "read-only"
    assert updates["approval_policy"] == "on-request"
    assert updates["reasoning_effort"] == "auto"


def test_collect_updates_includes_nonempty_key():
    updates = _collect_settings_updates(
        api_key="sk-new", base_url="", model="",
        sandbox_mode="", approval_policy="", reasoning_effort="",
    )
    assert updates == {"api_key": "sk-new"}


def test_collect_updates_all_blank_is_empty():
    # Nothing entered -> nothing to save (the dialog shows "Nothing to save").
    updates = _collect_settings_updates(
        api_key="", base_url="", model="",
        sandbox_mode="", approval_policy="", reasoning_effort="",
    )
    assert updates == {}


def test_collect_updates_dropdowns_flow_through():
    # The sandbox/approval/reasoning dropdowns (new in this window) must reach
    # write_nanocodex_config, not just the three original text fields.
    updates = _collect_settings_updates(
        api_key="", base_url="", model="",
        sandbox_mode="danger-full-access",
        approval_policy="never",
        reasoning_effort="max",
    )
    assert updates == {
        "sandbox_mode": "danger-full-access",
        "approval_policy": "never",
        "reasoning_effort": "max",
    }


# --- Scheduled-tasks section helpers --------------------------------------


def test_collect_schedule_add_coerces_numeric_strings():
    # Tk Entry widgets hand us strings; the store wants ints. The GUI form must
    # coerce exactly like the conversational manage_schedule tool does.
    kwargs = _collect_schedule_add(
        prompt="run tests", kind="interval", every_seconds="300",
    )
    assert kwargs["prompt"] == "run tests"
    assert kwargs["kind"] == "interval"
    assert kwargs["every_seconds"] == 300
    assert kwargs["at_hour"] == 9 and kwargs["at_minute"] == 0  # defaults
    assert kwargs["allow_desktop"] is False


def test_collect_schedule_add_daily_fields():
    kwargs = _collect_schedule_add(
        prompt="x", kind="daily", at_hour="14", at_minute="30",
        allow_desktop=True,
    )
    assert kwargs["at_hour"] == 14 and kwargs["at_minute"] == 30
    assert kwargs["allow_desktop"] is True


def test_collect_schedule_add_blank_numbers_use_defaults():
    # Blank / garbage numeric fields fall back to the store's own defaults
    # rather than raising here (the store does the real validation).
    kwargs = _collect_schedule_add(prompt="x", kind="once",
                                   every_seconds="", at_hour="oops")
    assert kwargs["every_seconds"] == 0
    assert kwargs["at_hour"] == 9


def test_collect_schedule_add_strips_prompt_and_run_at():
    kwargs = _collect_schedule_add(prompt="  hi  ", kind="once",
                                   run_at="  2026-06-07T10:00:00  ")
    assert kwargs["prompt"] == "hi"
    assert kwargs["run_at"] == "2026-06-07T10:00:00"


def test_format_schedule_recurrence():
    assert _format_schedule_recurrence(
        kind="interval", every_seconds=300, at_hour=9, at_minute=0,
    ) == "every 5m"
    assert _format_schedule_recurrence(
        kind="daily", every_seconds=0, at_hour=14, at_minute=5,
    ) == "daily 14:05"
    assert _format_schedule_recurrence(
        kind="once", every_seconds=0, at_hour=9, at_minute=0,
    ) == "once"


def test_collect_schedule_add_round_trips_through_store(tmp_path):
    # End-to-end: the coerced kwargs must satisfy ScheduleStore.add() — the same
    # store the model's manage_schedule tool writes to — and persist a task.
    from nanocodex.agent.schedule import ScheduleStore

    store = ScheduleStore(path=tmp_path / "schedule.json")
    kwargs = _collect_schedule_add(
        prompt="reply to wechat", kind="interval", every_seconds="600",
        allow_desktop=True,
    )
    task = store.add(**kwargs)
    assert task.kind == "interval"
    assert task.every_seconds == 600
    assert task.allow_desktop is True
    # Reload from disk: the manual page and the model see the same file.
    assert ScheduleStore(path=tmp_path / "schedule.json").get(task.id) is not None


def test_collect_schedule_add_bad_kind_rejected_by_store(tmp_path):
    # The store is the single validation authority; a bad kind raises there,
    # so the GUI surfaces the SAME error the conversational tool would.
    import pytest

    from nanocodex.agent.schedule import ScheduleStore

    store = ScheduleStore(path=tmp_path / "schedule.json")
    kwargs = _collect_schedule_add(prompt="x", kind="hourly")  # invalid kind
    with pytest.raises(ValueError):
        store.add(**kwargs)
