"""Session snapshots: a browsable, replayable history of past conversations.

The GUI shows a left-hand list of past conversations; clicking one replays its
FULL transcript. Two layers make that work:

* a small global INDEX at ``~/.nanocodex/sessions.jsonl`` — one line per
  conversation (a lightweight summary: title, counts, timestamps), newest-first
  for the list, and
* a per-conversation SNAPSHOT at ``~/.nanocodex/snapshots/<session_id>.json``
  holding the frozen full message list, so the detail view shows the real
  conversation — not a digest, and not the live ``session.jsonl`` (which
  ``--resume`` keeps appending to and compaction rewrites).

Keying is by **session_id**, NOT workspace: each time the GUI opens a project it
mints a new id, so re-opening or resuming the same folder keeps a SEPARATE
history entry instead of overwriting the previous one. Within one conversation
the same id is reused, so each turn rewrites (grows) that one snapshot in place.

Design split (same spirit as schedule.py / memory_store.py): SUMMARY EXTRACTION
and the STORE are pure over data (+ an injected "now"), so they unit-test fully
offline. Legacy workspace-keyed rows (the pre-snapshot format) still load — they
get a synthetic ``legacy:<workspace>`` id and simply have no snapshot to replay.
"""

from __future__ import annotations

import json
import uuid
from dataclasses import asdict, dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

DEFAULT_INDEX_PATH = Path.home() / ".nanocodex" / "sessions.jsonl"

# Cap the stored title/snippet so one giant paste can't bloat the index file.
_TITLE_MAX = 120
_SNIPPET_MAX = 200


def new_session_id() -> str:
    """Mint a fresh conversation id (the GUI calls this when it builds a loop)."""
    return uuid.uuid4().hex[:12]


def _now_iso() -> str:
    return datetime.now().isoformat(timespec="seconds")


def _first_text(content: "str | list[dict[str, Any]] | None") -> str:
    """Flatten a message ``content`` (str or block list) to plain text."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: list[str] = []
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                parts.append(str(block.get("text", "")))
        return " ".join(p for p in parts if p)
    return ""


def _clip(text: str, limit: int) -> str:
    text = " ".join(text.split())  # collapse whitespace/newlines
    if len(text) <= limit:
        return text
    return text[: limit - 1].rstrip() + "…"


@dataclass
class SessionSummary:
    """A browsable digest of one conversation.

    Pure data — built by :func:`summarize`, persisted by :class:`SessionIndex`.
    ``session_id`` is the stable key (one entry per conversation; re-opening a
    project mints a new id and a new entry). The rest is a deterministic readout
    of the transcript for the directory list + detail view. ``has_snapshot``
    tells the GUI whether a full-transcript replay is available (legacy rows
    have none).
    """

    session_id: str
    workspace: str
    title: str = ""              # first user line, clipped — the list label
    snippet: str = ""            # last assistant line, clipped — a preview
    user_messages: int = 0
    assistant_messages: int = 0
    tool_calls: int = 0
    recent_tools: list[str] = field(default_factory=list)
    created_at: str = ""         # ISO timestamp of the first turn in this session
    updated_at: str = ""         # ISO timestamp of the last turn
    log_path: str = ""           # the workspace's session.jsonl, for reopening
    has_snapshot: bool = False   # whether a frozen full transcript exists

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def summarize(
    session_id: str,
    workspace: str,
    messages: list[dict[str, Any]],
    *,
    log_path: str = "",
    now_iso: str | None = None,
    created_at: str | None = None,
    has_snapshot: bool = False,
) -> SessionSummary:
    """Build a DETERMINISTIC summary of *messages* for one conversation.

    Zero-cost: title = first user message (clipped), snippet = last assistant
    message (clipped), plus counts and the most-recent tool names. No model
    call, no network — just a readout of the list. ``system`` messages are
    ignored (they're prompt scaffolding, not conversation).
    """
    title = ""
    snippet = ""
    users = assistants = tools = 0
    recent_tools: list[str] = []

    for m in messages:
        role = m.get("role")
        if role == "user":
            users += 1
            if not title:
                text = _first_text(m.get("content"))
                # Skip a compaction/summary marker injected as a user message.
                if text and not text.startswith("[Earlier conversation"):
                    title = _clip(text, _TITLE_MAX)
        elif role == "assistant":
            assistants += 1
            text = _first_text(m.get("content"))
            if text:
                snippet = _clip(text, _SNIPPET_MAX)
            for tc in m.get("tool_calls") or []:
                name = (tc.get("function") or {}).get("name")
                if name:
                    tools += 1
                    recent_tools.append(str(name))

    now = now_iso or _now_iso()
    return SessionSummary(
        session_id=session_id,
        workspace=workspace,
        title=title or "(no prompt yet)",
        snippet=snippet,
        user_messages=users,
        assistant_messages=assistants,
        tool_calls=tools,
        recent_tools=recent_tools[-8:],
        created_at=created_at or now,
        updated_at=now,
        log_path=log_path,
        has_snapshot=has_snapshot,
    )


def _redact_messages(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Strip base64 image data from a message list before freezing a snapshot.

    Mirrors session.py's log redaction: a full transcript may carry huge
    ``data:`` image URLs that would bloat the snapshot file (and aren't useful
    to replay as text). Replace each with a placeholder; everything else is kept
    verbatim so the conversation reads back faithfully.
    """
    out: list[dict[str, Any]] = []
    for msg in messages:
        content = msg.get("content")
        if not isinstance(content, list):
            out.append(msg)
            continue
        blocks: list[Any] = []
        changed = False
        for block in content:
            if (
                isinstance(block, dict)
                and block.get("type") == "image_url"
                and isinstance(block.get("image_url"), dict)
                and str(block["image_url"].get("url", "")).startswith("data:")
            ):
                blocks.append({"type": "text", "text": "[image omitted from snapshot]"})
                changed = True
            else:
                blocks.append(block)
        out.append({**msg, "content": blocks} if changed else msg)
    return out


class SessionIndex:
    """Load/save the global conversation directory as JSONL, keyed by session_id.

    Each conversation gets one row; :meth:`record` UPSERTs by ``session_id`` so a
    turn updates its own row, while a NEW conversation (new id) adds a separate
    entry — re-opening a project no longer overwrites its past history.
    :meth:`entries` returns them newest-activity-first for the directory list.

    Full transcripts are frozen as per-conversation snapshot files under
    ``<index dir>/snapshots/<session_id>.json`` so the detail view can replay the
    real conversation rather than a digest.
    """

    def __init__(self, path: Path | None = None) -> None:
        self.path = Path(path) if path is not None else DEFAULT_INDEX_PATH
        self.snapshots_dir = self.path.parent / "snapshots"
        self._by_id: dict[str, SessionSummary] = {}
        self._load()

    def _load(self) -> None:
        try:
            text = self.path.read_text(encoding="utf-8")
        except (OSError, ValueError):
            return
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError:
                continue  # tolerate a partially-written trailing line
            if not isinstance(d, dict):
                continue
            sid = d.get("session_id")
            ws = str(d.get("workspace", ""))
            # Legacy rows (pre-snapshot format) had no session_id, only a
            # workspace. Give them a stable synthetic id so they still list (they
            # simply have no snapshot to replay).
            if not sid:
                if not ws:
                    continue
                sid = f"legacy:{ws}"
            sid = str(sid)
            # Last write for an id wins (the file is append-style on rewrite).
            self._by_id[sid] = SessionSummary(
                session_id=sid,
                workspace=ws,
                title=str(d.get("title", "")),
                snippet=str(d.get("snippet", "")),
                user_messages=int(d.get("user_messages", 0) or 0),
                assistant_messages=int(d.get("assistant_messages", 0) or 0),
                tool_calls=int(d.get("tool_calls", 0) or 0),
                recent_tools=[str(t) for t in (d.get("recent_tools") or [])],
                created_at=str(d.get("created_at", "") or d.get("updated_at", "")),
                updated_at=str(d.get("updated_at", "")),
                log_path=str(d.get("log_path", "")),
                has_snapshot=bool(d.get("has_snapshot", False)),
            )

    def _save(self) -> None:
        """Rewrite the whole index file from the folded map (dedup on disk too)."""
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            lines = [
                json.dumps(s.to_dict(), ensure_ascii=False)
                for s in self._sorted()
            ]
            self.path.write_text("\n".join(lines) + ("\n" if lines else ""),
                                 encoding="utf-8")
        except OSError:
            pass  # best-effort; a directory listing must never crash a turn

    def _sorted(self) -> list[SessionSummary]:
        """Newest activity first; blank timestamps sort last."""
        return sorted(
            self._by_id.values(),
            key=lambda s: s.updated_at or "",
            reverse=True,
        )

    # --- full-transcript snapshots ---------------------------------------

    def snapshot_path(self, session_id: str) -> Path:
        return self.snapshots_dir / f"{session_id}.json"

    def save_snapshot(self, session_id: str, messages: list[dict[str, Any]]) -> bool:
        """Freeze the full message list for one conversation. Best-effort.

        Returns True if the snapshot was written. Images are redacted to keep the
        file small; the snapshot is rewritten in full each turn (the transcript
        only grows), so the latest write always holds the whole conversation.
        """
        try:
            self.snapshots_dir.mkdir(parents=True, exist_ok=True)
            payload = {
                "session_id": session_id,
                "messages": _redact_messages(messages),
            }
            self.snapshot_path(session_id).write_text(
                json.dumps(payload, ensure_ascii=False), encoding="utf-8",
            )
            return True
        except OSError:
            return False

    def load_snapshot(self, session_id: str) -> list[dict[str, Any]] | None:
        """Read back a frozen transcript, or None if there is no snapshot."""
        try:
            text = self.snapshot_path(session_id).read_text(encoding="utf-8")
        except (OSError, ValueError):
            return None
        try:
            d = json.loads(text)
        except json.JSONDecodeError:
            return None
        msgs = d.get("messages") if isinstance(d, dict) else None
        return msgs if isinstance(msgs, list) else None

    # --- index records ---------------------------------------------------

    def record(self, summary: SessionSummary) -> None:
        """UPSERT one conversation's summary (by session_id) and persist."""
        self._by_id[summary.session_id] = summary
        self._save()

    def record_turn(
        self,
        session_id: str,
        workspace: str,
        messages: list[dict[str, Any]],
        *,
        log_path: str = "",
        now_iso: str | None = None,
    ) -> SessionSummary:
        """Freeze the transcript + upsert the row — the GUI's one call per turn.

        ``created_at`` is preserved from the conversation's first turn so the
        history keeps a stable "started at" even as later turns roll the
        ``updated_at`` forward.
        """
        prior = self._by_id.get(session_id)
        created_at = prior.created_at if prior else None
        saved = self.save_snapshot(session_id, messages)
        summary = summarize(
            session_id, workspace, messages,
            log_path=log_path, now_iso=now_iso,
            created_at=created_at, has_snapshot=saved,
        )
        self.record(summary)
        return summary

    def entries(self) -> list[SessionSummary]:
        """All conversation summaries, newest activity first (directory list)."""
        return self._sorted()

    def get(self, session_id: str) -> SessionSummary | None:
        return self._by_id.get(session_id)
