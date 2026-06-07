"""Tests for the scheduled-task store and due-calculation (pure, offline).

Every test injects an explicit `now`, so there are no real clocks or sleeps —
the recurrence math and the store's roll-forward are verified deterministically.
"""

from __future__ import annotations

from datetime import datetime

from nanocodex.agent.schedule import (
    ScheduledTask,
    ScheduleStore,
    compute_next_run,
)


def _dt(s: str) -> datetime:
    return datetime.strptime(s, "%Y-%m-%dT%H:%M:%S")


def _store(tmp_path):
    return ScheduleStore(path=tmp_path / "schedule.json")


# --- add / validation -----------------------------------------------------

def test_add_once_persists_and_reloads(tmp_path):
    s = _store(tmp_path)
    s.add("run the build", kind="once", run_at="2026-06-01T09:00:00")
    reloaded = ScheduleStore(path=tmp_path / "schedule.json")
    assert len(reloaded.tasks) == 1
    assert reloaded.tasks[0].prompt == "run the build"
    assert reloaded.tasks[0].next_run == "2026-06-01T09:00:00"


def test_add_rejects_empty_prompt(tmp_path):
    s = _store(tmp_path)
    try:
        s.add("   ", kind="once")
        assert False, "expected ValueError"
    except ValueError:
        pass


def test_add_interval_requires_positive_period(tmp_path):
    s = _store(tmp_path)
    try:
        s.add("ping", kind="interval", every_seconds=0)
        assert False, "expected ValueError"
    except ValueError:
        pass


def test_add_daily_validates_time(tmp_path):
    s = _store(tmp_path)
    try:
        s.add("morning report", kind="daily", at_hour=25)
        assert False, "expected ValueError"
    except ValueError:
        pass


# --- due detection --------------------------------------------------------

def test_due_returns_tasks_at_or_before_now(tmp_path):
    s = _store(tmp_path)
    s.add("a", kind="once", run_at="2026-06-01T09:00:00")
    s.add("b", kind="once", run_at="2026-06-01T12:00:00")
    due = s.due(now=_dt("2026-06-01T10:00:00"))
    assert [t.prompt for t in due] == ["a"]            # only the past-due one


def test_disabled_task_not_due(tmp_path):
    s = _store(tmp_path)
    t = s.add("a", kind="once", run_at="2026-06-01T09:00:00")
    s.set_enabled(t.id, False)
    assert s.due(now=_dt("2026-06-01T10:00:00")) == []


# --- recurrence math ------------------------------------------------------

def test_once_has_no_next_run():
    t = ScheduledTask(id="x", prompt="p", kind="once", next_run="2026-06-01T09:00:00")
    assert compute_next_run(t, _dt("2026-06-01T09:00:00")) is None


def test_interval_advances_in_whole_steps():
    t = ScheduledTask(id="x", prompt="p", kind="interval",
                      next_run="2026-06-01T09:00:00", every_seconds=3600)
    # 2.5 hours later -> next firing is 12:00 (3 whole hours from 09:00).
    nxt = compute_next_run(t, _dt("2026-06-01T11:30:00"))
    assert nxt == "2026-06-01T12:00:00"


def test_daily_rolls_to_next_day_when_past():
    t = ScheduledTask(id="x", prompt="p", kind="daily", at_hour=9, at_minute=0)
    # It's 10:00 already -> next firing is tomorrow 09:00.
    nxt = compute_next_run(t, _dt("2026-06-01T10:00:00"))
    assert nxt == "2026-06-02T09:00:00"


def test_daily_same_day_when_before():
    t = ScheduledTask(id="x", prompt="p", kind="daily", at_hour=9, at_minute=0)
    nxt = compute_next_run(t, _dt("2026-06-01T07:00:00"))
    assert nxt == "2026-06-01T09:00:00"


# --- mark_ran roll-forward ------------------------------------------------

def test_mark_ran_disables_spent_once(tmp_path):
    s = _store(tmp_path)
    t = s.add("a", kind="once", run_at="2026-06-01T09:00:00")
    s.mark_ran(t.id, now=_dt("2026-06-01T09:00:01"))
    got = s.get(t.id)
    assert got.enabled is False
    assert got.runs == 1
    assert got.next_run == ""
    assert s.due(now=_dt("2026-06-01T10:00:00")) == []   # won't fire again


def test_mark_ran_rolls_interval_forward(tmp_path):
    s = _store(tmp_path)
    t = s.add("a", kind="interval", run_at="2026-06-01T09:00:00", every_seconds=3600,
              now=_dt("2026-06-01T08:00:00"))
    s.mark_ran(t.id, now=_dt("2026-06-01T09:00:00"))
    got = s.get(t.id)
    assert got.enabled is True
    assert got.runs == 1
    assert got.next_run == "2026-06-01T10:00:00"          # +1 hour


def test_mark_ran_rolls_daily_to_next_day(tmp_path):
    s = _store(tmp_path)
    t = s.add("report", kind="daily", at_hour=9, at_minute=0,
              now=_dt("2026-06-01T07:00:00"))
    assert t.next_run == "2026-06-01T09:00:00"
    s.mark_ran(t.id, now=_dt("2026-06-01T09:00:00"))
    assert s.get(t.id).next_run == "2026-06-02T09:00:00"


# --- consecutive-failure auto-disable -------------------------------------

def test_mark_ran_ok_keeps_failure_streak_at_zero(tmp_path):
    s = _store(tmp_path)
    t = s.add("a", kind="interval", run_at="2026-06-01T09:00:00", every_seconds=3600,
              now=_dt("2026-06-01T08:00:00"))
    s.mark_ran(t.id, now=_dt("2026-06-01T09:00:00"), ok=True)
    got = s.get(t.id)
    assert got.consecutive_failures == 0
    assert got.enabled is True


def test_mark_ran_disables_after_max_consecutive_failures(tmp_path):
    from nanocodex.agent.schedule import MAX_CONSECUTIVE_FAILURES
    s = _store(tmp_path)
    t = s.add("needs desktop", kind="interval", run_at="2026-06-01T09:00:00",
              every_seconds=3600, now=_dt("2026-06-01T08:00:00"))
    # Fail one short of the threshold: still enabled, still rolling forward.
    for i in range(MAX_CONSECUTIVE_FAILURES - 1):
        s.mark_ran(t.id, now=_dt("2026-06-01T09:00:00"), ok=False)
    got = s.get(t.id)
    assert got.consecutive_failures == MAX_CONSECUTIVE_FAILURES - 1
    assert got.enabled is True
    # The failure that hits the threshold disables it.
    s.mark_ran(t.id, now=_dt("2026-06-01T09:00:00"), ok=False)
    got = s.get(t.id)
    assert got.consecutive_failures == MAX_CONSECUTIVE_FAILURES
    assert got.enabled is False
    assert s.due(now=_dt("2026-06-01T23:00:00")) == []   # no longer fires


def test_mark_ran_success_resets_failure_streak(tmp_path):
    s = _store(tmp_path)
    t = s.add("a", kind="interval", run_at="2026-06-01T09:00:00", every_seconds=3600,
              now=_dt("2026-06-01T08:00:00"))
    s.mark_ran(t.id, now=_dt("2026-06-01T09:00:00"), ok=False)
    s.mark_ran(t.id, now=_dt("2026-06-01T10:00:00"), ok=False)
    assert s.get(t.id).consecutive_failures == 2
    s.mark_ran(t.id, now=_dt("2026-06-01T11:00:00"), ok=True)
    got = s.get(t.id)
    assert got.consecutive_failures == 0   # one clean run clears the streak
    assert got.enabled is True


def test_reenable_clears_failure_streak(tmp_path):
    # A task auto-disabled by failures, then re-enabled by the user, must start
    # its streak fresh — otherwise it'd carry a full count and get re-disabled on
    # the very next failure.
    from nanocodex.agent.schedule import MAX_CONSECUTIVE_FAILURES
    s = _store(tmp_path)
    t = s.add("a", kind="interval", run_at="2026-06-01T09:00:00", every_seconds=3600,
              now=_dt("2026-06-01T08:00:00"))
    for _ in range(MAX_CONSECUTIVE_FAILURES):
        s.mark_ran(t.id, now=_dt("2026-06-01T09:00:00"), ok=False)
    assert s.get(t.id).enabled is False
    s.set_enabled(t.id, True)
    got = s.get(t.id)
    assert got.enabled is True
    assert got.consecutive_failures == 0


def test_consecutive_failures_persists_and_reloads(tmp_path):
    s = _store(tmp_path)
    t = s.add("a", kind="interval", run_at="2026-06-01T09:00:00", every_seconds=3600,
              now=_dt("2026-06-01T08:00:00"))
    s.mark_ran(t.id, now=_dt("2026-06-01T09:00:00"), ok=False)
    reloaded = ScheduleStore(path=tmp_path / "schedule.json")
    assert reloaded.get(t.id).consecutive_failures == 1


# --- remove / enable ------------------------------------------------------

def test_remove(tmp_path):
    s = _store(tmp_path)
    t = s.add("a", kind="once", run_at="2026-06-01T09:00:00")
    assert s.remove(t.id) is True
    assert s.get(t.id) is None
    assert s.remove("nope") is False


def test_seconds_until_next(tmp_path):
    s = _store(tmp_path)
    s.add("a", kind="once", run_at="2026-06-01T09:00:00")
    s.add("b", kind="once", run_at="2026-06-01T09:00:30")
    secs = s.seconds_until_next(now=_dt("2026-06-01T08:59:00"))
    assert secs == 60.0                                   # soonest is 09:00:00


def test_seconds_until_next_none_when_empty(tmp_path):
    s = _store(tmp_path)
    assert s.seconds_until_next(now=_dt("2026-06-01T09:00:00")) is None


# --- allow_desktop (Direction B: per-task desktop authorization) ----------

def test_allow_desktop_defaults_false(tmp_path):
    # A task is safe by default: nothing may drive the desktop unattended unless
    # the user explicitly opts in.
    s = _store(tmp_path)
    t = s.add("send a wechat message", kind="once", run_at="2026-06-01T09:00:00")
    assert t.allow_desktop is False


def test_allow_desktop_persists_and_reloads(tmp_path):
    # The flag must survive a save/reload round-trip, or a reopened scheduler
    # would silently drop the authorization.
    s = _store(tmp_path)
    s.add("reply on wechat", kind="interval", run_at="2026-06-01T09:00:00",
          every_seconds=900, allow_desktop=True, now=_dt("2026-06-01T08:00:00"))
    reloaded = ScheduleStore(path=s.path)
    assert reloaded.tasks[0].allow_desktop is True


def test_legacy_task_without_flag_loads_as_false(tmp_path):
    # A schedule.json written before this field existed has no allow_desktop key.
    # It must load as False (safe default), never crash or default to True.
    path = tmp_path / "schedule.json"
    path.write_text(
        '[{"id": "abc123", "prompt": "old task", "kind": "once", '
        '"next_run": "2026-06-01T09:00:00", "enabled": true}]',
        encoding="utf-8",
    )
    store = ScheduleStore(path=path)
    assert len(store.tasks) == 1
    assert store.tasks[0].allow_desktop is False
