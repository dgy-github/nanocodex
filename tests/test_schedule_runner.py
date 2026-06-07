"""Tests for the scheduler runtime (offline; fake run_task, explicit now)."""

from __future__ import annotations

from datetime import datetime

from nanocodex.agent.schedule import ScheduleStore
from nanocodex.agent.schedule_runner import run_due_once, run_forever


def _dt(s: str) -> datetime:
    return datetime.strptime(s, "%Y-%m-%dT%H:%M:%S")


def _store(tmp_path):
    return ScheduleStore(path=tmp_path / "schedule.json")


async def test_run_due_once_fires_and_rolls_forward(tmp_path):
    s = _store(tmp_path)
    s.add("build", kind="once", run_at="2026-06-01T09:00:00")
    s.add("later", kind="once", run_at="2026-06-01T18:00:00")

    ran: list[str] = []

    async def fake_run(task):
        ran.append(task.prompt)

    fired = await run_due_once(s, fake_run, now=_dt("2026-06-01T10:00:00"))
    assert ran == ["build"]                       # only the past-due one ran
    assert len(fired) == 1
    # The 'once' task is now spent/disabled.
    assert s.due(now=_dt("2026-06-01T11:00:00")) == []


async def test_run_due_once_interval_reschedules(tmp_path):
    s = _store(tmp_path)
    # Pass an explicit `now` BEFORE run_at so the interval start isn't rebased to
    # the real wall clock (which made this test pass only before 09:00 local).
    s.add("ping", kind="interval", run_at="2026-06-01T09:00:00", every_seconds=3600,
          now=_dt("2026-06-01T08:00:00"))

    ran: list[str] = []

    async def fake_run(task):
        ran.append(task.prompt)

    await run_due_once(s, fake_run, now=_dt("2026-06-01T09:00:00"))
    assert ran == ["ping"]
    # Rolled forward by one hour and still enabled.
    t = s.tasks[0]
    assert t.enabled and t.next_run == "2026-06-01T10:00:00"


async def test_failing_task_still_rolls_forward(tmp_path):
    s = _store(tmp_path)
    # Explicit `now` before run_at so the interval start isn't rebased to the
    # real clock (otherwise the task isn't due at the test's fixed 09:00).
    t = s.add("boom", kind="interval", run_at="2026-06-01T09:00:00", every_seconds=60,
              now=_dt("2026-06-01T08:00:00"))

    async def boom(task):
        raise RuntimeError("kaboom")

    events: list[str] = []
    fired = await run_due_once(s, boom, now=_dt("2026-06-01T09:00:00"),
                               on_event=events.append)
    assert fired == [t.id]                         # advanced despite the error
    assert s.get(t.id).runs == 1
    assert s.get(t.id).consecutive_failures == 1   # the raise was recorded as a failure
    assert any("failed" in e for e in events)


async def test_run_forever_stops_on_stop_check(tmp_path):
    # With stop_check immediately True, run_forever returns without sleeping/running.
    s = _store(tmp_path)
    s.add("x", kind="once", run_at="2026-06-01T09:00:00")

    async def fake_run(prompt):
        raise AssertionError("should not run when stopped immediately")

    await run_forever(s, fake_run, stop_check=lambda: True)
