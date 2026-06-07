"""Scheduler runtime: poll the ScheduleStore and run due tasks.

Kept deliberately thin and INJECTABLE so it tests offline:

* ``run_due_once`` fires every task due at a given ``now`` through a supplied
  ``run_task`` coroutine, then rolls each forward via the store. It does NOT
  sleep or touch a real clock — tests pass an explicit ``now``.
* ``run_forever`` is the only part that waits: it sleeps until the next task is
  due (bounded by ``poll_interval`` so newly-added tasks are noticed), then
  delegates to ``run_due_once``. The waiting/clock lives here alone.

A "task run" is just one agent turn with the task's prompt. The CLI wires
``run_task`` to a real AgentLoop; tests wire a fake that records calls.
"""

from __future__ import annotations

import asyncio
from datetime import datetime
from typing import Awaitable, Callable

from nanocodex.agent.schedule import ScheduledTask, ScheduleStore

# run_task(task) -> awaitable. Receives the whole ScheduledTask (not just the
# prompt) so the host can honor per-task settings like allow_desktop when it
# builds the run's approver. Return value is ignored; exceptions are caught so
# one failing task never kills the scheduler loop.
RunTask = Callable[[ScheduledTask], Awaitable[None]]
OnEvent = Callable[[str], None]


async def run_due_once(
    store: ScheduleStore,
    run_task: RunTask,
    *,
    now: datetime | None = None,
    on_event: OnEvent | None = None,
) -> list[str]:
    """Run every task due at *now*; return the ids that fired.

    Each task is marked ran (rolled forward / disabled) AFTER its turn, whether
    or not the turn raised — a task that errors should still advance, not spin.
    """
    now = now or datetime.now()
    fired: list[str] = []
    for task in store.due(now=now):
        if on_event:
            on_event(f"running scheduled task {task.id}: {task.prompt[:60]}")
        ok = True
        try:
            await run_task(task)
        except Exception as exc:  # noqa: BLE001 - one bad task mustn't stop the loop
            ok = False
            if on_event:
                on_event(f"task {task.id} failed: {exc}")
        finally:
            # Mark failures so a recurring task that keeps erroring/timing out is
            # eventually auto-disabled (see ScheduleStore.mark_ran) instead of
            # spinning every poll. A clean run resets the failure streak.
            store.mark_ran(task.id, now=now, ok=ok)
            fired.append(task.id)
    return fired


async def run_forever(
    store: ScheduleStore,
    run_task: RunTask,
    *,
    poll_interval: float = 30.0,
    on_event: OnEvent | None = None,
    stop_check: Callable[[], bool] | None = None,
) -> None:
    """Sleep until the next task is due, run it, repeat until stopped.

    ``poll_interval`` caps the sleep so tasks added while we wait are picked up
    within that window. ``stop_check`` lets a host request a clean shutdown.
    """
    while not (stop_check and stop_check()):
        secs = store.seconds_until_next()
        # Nothing pending -> wait a poll interval and re-check (tasks may be
        # added externally). Otherwise wait until the soonest, capped.
        wait = poll_interval if secs is None else min(secs, poll_interval)
        if wait > 0:
            await asyncio.sleep(wait)
        if stop_check and stop_check():
            break
        await run_due_once(store, run_task, on_event=on_event)
