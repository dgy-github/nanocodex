"""Tests for session resume from JSONL history."""

from __future__ import annotations

import json
from pathlib import Path

from nanocodex.agent.session import Session


def _write_log(path: Path, records: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as fh:
        for rec in records:
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")


def test_resume_with_no_log_starts_fresh(tmp_path):
    log = tmp_path / "session.jsonl"
    session = Session.resume("SYS", log_path=log)
    assert session.restored_count == 0
    assert session.messages == [{"role": "system", "content": "SYS"}]


def test_resume_restores_prior_messages(tmp_path):
    log = tmp_path / "session.jsonl"
    _write_log(log, [
        {"role": "system", "content": "OLD SYS", "_ts": "t0"},
        {"role": "user", "content": "hello", "_ts": "t1"},
        {"role": "assistant", "content": "hi there", "_ts": "t2"},
    ])
    session = Session.resume("NEW SYS", log_path=log)
    # System prompt is the fresh one, not the logged "OLD SYS".
    assert session.messages[0] == {"role": "system", "content": "NEW SYS"}
    # The two non-system messages are restored, _ts stripped.
    assert session.messages[1] == {"role": "user", "content": "hello"}
    assert session.messages[2] == {"role": "assistant", "content": "hi there"}
    assert session.restored_count == 2


def test_resume_backfills_unanswered_tool_call(tmp_path):
    log = tmp_path / "session.jsonl"
    _write_log(log, [
        {"role": "user", "content": "do it"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {"id": "c1", "type": "function",
                 "function": {"name": "shell", "arguments": "{}"}}
            ],
        },
        # NOTE: no tool result for c1 — interrupted before it was recorded.
    ])
    session = Session.resume("SYS", log_path=log)
    # A synthetic tool result must follow the assistant tool_call so the
    # backend doesn't reject the next request.
    tool_msgs = [m for m in session.messages if m.get("role") == "tool"]
    assert len(tool_msgs) == 1
    assert tool_msgs[0]["tool_call_id"] == "c1"
    assert tool_msgs[0]["name"] == "shell"
    assert "interrupted" in tool_msgs[0]["content"]


def test_resume_does_not_backfill_answered_tool_call(tmp_path):
    log = tmp_path / "session.jsonl"
    _write_log(log, [
        {"role": "user", "content": "do it"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {"id": "c1", "type": "function",
                 "function": {"name": "shell", "arguments": "{}"}}
            ],
        },
        {"role": "tool", "tool_call_id": "c1", "name": "shell", "content": "done"},
    ])
    session = Session.resume("SYS", log_path=log)
    tool_msgs = [m for m in session.messages if m.get("role") == "tool"]
    assert len(tool_msgs) == 1
    assert tool_msgs[0]["content"] == "done"  # real result kept, no synthetic added


def test_resume_tolerates_partial_trailing_line(tmp_path):
    log = tmp_path / "session.jsonl"
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("w", encoding="utf-8") as fh:
        fh.write(json.dumps({"role": "user", "content": "ok"}) + "\n")
        fh.write('{"role": "assistant", "content": "half')  # truncated, no newline
    session = Session.resume("SYS", log_path=log)
    # The valid line is restored; the corrupt trailing line is skipped.
    assert session.restored_count == 1
    assert session.messages[1] == {"role": "user", "content": "ok"}


def test_resume_then_append_continues_same_log(tmp_path):
    log = tmp_path / "session.jsonl"
    _write_log(log, [{"role": "user", "content": "first"}])
    session = Session.resume("SYS", log_path=log)
    session.add_user("second")
    # The new message is appended to the existing log, not overwriting it.
    lines = [json.loads(ln) for ln in log.read_text(encoding="utf-8").splitlines() if ln.strip()]
    contents = [r.get("content") for r in lines]
    assert contents == ["first", "second"]
