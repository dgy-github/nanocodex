"""Session: the message history for one conversation, persisted as JSONL."""

from __future__ import annotations

import json
from datetime import datetime
from pathlib import Path
from typing import Any


def _redact_for_log(msg: dict[str, Any]) -> dict[str, Any]:
    """Replace base64 image data with a placeholder before logging.

    Keeps the JSONL log small and avoids persisting huge data URLs (which would
    also bloat any later resume). The in-memory message the model sees is
    untouched — only the logged copy is redacted.
    """
    content = msg.get("content")
    if not isinstance(content, list):
        return msg
    redacted_blocks: list[Any] = []
    changed = False
    for block in content:
        if (
            isinstance(block, dict)
            and block.get("type") == "image_url"
            and isinstance(block.get("image_url"), dict)
            and str(block["image_url"].get("url", "")).startswith("data:")
        ):
            redacted_blocks.append({"type": "text", "text": "[image omitted from log]"})
            changed = True
        else:
            redacted_blocks.append(block)
    if not changed:
        return msg
    return {**msg, "content": redacted_blocks}


class Session:
    """Holds the running message list and appends each turn to a JSONL log."""

    def __init__(self, system_prompt: str, *, log_path: Path | None = None) -> None:
        self.messages: list[dict[str, Any]] = [
            {"role": "system", "content": system_prompt}
        ]
        self._log_path = log_path
        # Number of non-system messages restored from a prior log (0 for fresh).
        self.restored_count = 0
        if log_path is not None:
            log_path.parent.mkdir(parents=True, exist_ok=True)

    @classmethod
    def resume(cls, system_prompt: str, *, log_path: Path | None) -> "Session":
        """Rebuild a session from a prior JSONL log, if one exists.

        The system prompt is taken fresh (sandbox/AGENTS.md may have changed)
        rather than from the log. The restored tail is sanitized so every
        assistant tool_call has a matching tool result — otherwise the backend
        rejects the next request. New messages continue appending to the same
        log file.
        """
        session = cls(system_prompt, log_path=log_path)
        restored = cls._read_log(log_path)
        if restored:
            body = [m for m in restored if m.get("role") != "system"]
            body = cls._backfill_tool_results(body)
            session.messages.extend(body)
            session.restored_count = len(body)
        return session

    @classmethod
    def fork(
        cls,
        system_prompt: str,
        seed_messages: list[dict[str, Any]],
        *,
        log_path: Path | None,
    ) -> "Session":
        """Start a NEW conversation seeded from another's frozen messages.

        Used to "continue from" a past conversation without mutating it: the
        caller passes a snapshot's message list; we drop its system prompt (a
        fresh one is taken — sandbox / AGENTS.md / skills may have changed since),
        sanitize any dangling tool_call (so the backend's "tool_calls must be
        followed by tool messages" contract holds), and seed the new session with
        the rest. New messages append to *log_path*, which must be a NEW file for
        the forked conversation, so the original log/snapshot is never touched.
        """
        session = cls(system_prompt, log_path=log_path)
        body = [m for m in (seed_messages or []) if m.get("role") != "system"]
        body = cls._backfill_tool_results(body)
        session.messages.extend(body)
        session.restored_count = len(body)
        return session

    @staticmethod
    def _read_log(log_path: Path | None) -> list[dict[str, Any]]:
        if log_path is None or not log_path.is_file():
            return []
        out: list[dict[str, Any]] = []
        try:
            text = log_path.read_text(encoding="utf-8")
        except OSError:
            return []
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue  # tolerate a partially-written trailing line
            if isinstance(rec, dict) and rec.get("role"):
                rec.pop("_ts", None)
                out.append(rec)
        return out

    @staticmethod
    def _backfill_tool_results(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """Insert synthetic results for any tool_call left unanswered.

        Mirrors the contract the backend enforces: an assistant message with
        tool_calls must be followed by a tool message per call id.
        """
        fulfilled = {
            m.get("tool_call_id")
            for m in messages
            if m.get("role") == "tool" and m.get("tool_call_id")
        }
        result: list[dict[str, Any]] = []
        for m in messages:
            result.append(m)
            if m.get("role") == "assistant" and m.get("tool_calls"):
                for tc in m["tool_calls"]:
                    cid = tc.get("id")
                    if cid and cid not in fulfilled:
                        name = (tc.get("function") or {}).get("name", "tool")
                        result.append({
                            "role": "tool",
                            "tool_call_id": cid,
                            "name": name,
                            "content": "[interrupted: tool result not recorded]",
                        })
        return result

    def add_user(self, content: "str | list[dict[str, Any]]") -> None:
        self._append({"role": "user", "content": content})

    def add_assistant(
        self,
        content: str,
        tool_calls: list[dict[str, Any]] | None = None,
        reasoning: str | None = None,
    ) -> None:
        msg: dict[str, Any] = {"role": "assistant", "content": content or ""}
        if reasoning:
            msg["reasoning_content"] = reasoning
        if tool_calls:
            msg["tool_calls"] = tool_calls
        self._append(msg)

    def add_tool_result(self, tool_call_id: str, name: str, content: str) -> None:
        self._append({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "name": name,
            "content": content,
        })

    def backfill_unanswered_tool_calls(
        self, placeholder: str = "[interrupted: tool not run]"
    ) -> int:
        """Append synthetic tool results for any tool_call left unanswered.

        The backend contract: an assistant message carrying ``tool_calls`` must
        be followed by a ``tool`` message for *every* call id, or the next
        request is rejected (400: "tool_calls must be followed by tool
        messages"). When a turn is interrupted (user Stop) after the assistant
        tool-call message was recorded but before its tools ran, the live
        session is left with a dangling tool_calls message.

        Call this BEFORE appending any further assistant message so the
        synthetic results stay contiguous with the tool-call message they
        answer. Returns the number of results added (0 when already complete).
        """
        fulfilled = {
            m.get("tool_call_id")
            for m in self.messages
            if m.get("role") == "tool" and m.get("tool_call_id")
        }
        added = 0
        for m in self.messages:
            if m.get("role") == "assistant" and m.get("tool_calls"):
                for tc in m["tool_calls"]:
                    cid = tc.get("id")
                    if cid and cid not in fulfilled:
                        name = (tc.get("function") or {}).get("name", "tool")
                        self.add_tool_result(cid, name, placeholder)
                        fulfilled.add(cid)
                        added += 1
        return added

    def _append(self, msg: dict[str, Any]) -> None:
        self.messages.append(msg)
        if self._log_path is not None:
            record = {**_redact_for_log(msg), "_ts": datetime.now().isoformat()}
            try:
                with self._log_path.open("a", encoding="utf-8") as fh:
                    fh.write(json.dumps(record, ensure_ascii=False) + "\n")
            except OSError:
                pass

    def for_model(self) -> list[dict[str, Any]]:
        """The message list to send to the provider (a shallow copy)."""
        return list(self.messages)
