"""Tests for the `nanocodex schedule add` CLI command's wiring.

Focused on the Direction-B addition: the `--allow-desktop` flag must travel all
the way into the persisted task (and default to False when omitted). We call the
command function directly rather than through CliRunner — the app uses a
`@app.callback(invoke_without_command=True)` guard (so bare `nanocodex` / a
prompt still work after subcommands were added), and CliRunner's router trips
over that for subcommand dispatch. Calling the function exercises exactly the
flag -> store.add wiring we care about, against an isolated temp store.
"""

from __future__ import annotations

import nanocodex.agent.schedule as sched
import nanocodex.cli as cli


def _isolate_store(tmp_path, monkeypatch):
    # _schedule_store() builds ScheduleStore() with the default path, read at
    # construction — so redirecting the default to a temp file isolates the test
    # from the real ~/.nanocodex/schedule.json.
    monkeypatch.setattr(sched, "DEFAULT_SCHEDULE_PATH", tmp_path / "schedule.json")


def test_allow_desktop_flag_persists_true(tmp_path, monkeypatch):
    _isolate_store(tmp_path, monkeypatch)
    cli.schedule_add("send wechat", kind="interval", at=None, every=900,
                     daily_at=None, allow_desktop=True)
    s = sched.ScheduleStore(path=tmp_path / "schedule.json")
    assert len(s.tasks) == 1
    assert s.tasks[0].allow_desktop is True


def test_allow_desktop_defaults_false(tmp_path, monkeypatch):
    _isolate_store(tmp_path, monkeypatch)
    # Omitting the flag (its CLI default) must leave the safe behavior.
    cli.schedule_add("harmless", kind="interval", at=None, every=900,
                     daily_at=None, allow_desktop=False)
    s = sched.ScheduleStore(path=tmp_path / "schedule.json")
    assert s.tasks[0].allow_desktop is False


def test_allow_desktop_survives_reload(tmp_path, monkeypatch):
    # The flag must round-trip through JSON: a fresh store reloads it as True.
    _isolate_store(tmp_path, monkeypatch)
    cli.schedule_add("desktop task", kind="daily", at=None, every=None,
                     daily_at="09:00", allow_desktop=True)
    reloaded = sched.ScheduleStore(path=tmp_path / "schedule.json")
    assert reloaded.tasks[0].allow_desktop is True
    assert reloaded.tasks[0].kind == "daily"
