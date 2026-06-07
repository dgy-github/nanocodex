"""manage_schedule: let the agent create/list/cancel scheduled tasks in-chat.

So the model can act on "every day at 9, run the tests" without the user
needing to know the CLI. It wraps the SAME ScheduleStore the CLI and the
`schedule run` runner use (default ~/.nanocodex/schedule.json), so a task added
here is picked up by a running scheduler.

This tool only MANAGES tasks (data); it does not run them. The user still needs
`nanocodex schedule run` going for tasks to actually fire — the tool says so in
its output, so the agent can relay that.
"""

from __future__ import annotations

from typing import Any

from nanocodex.tools.base import Tool


class ManageScheduleTool(Tool):
    @property
    def name(self) -> str:
        return "manage_schedule"

    @property
    def description(self) -> str:
        return (
            "Create, list, or cancel SCHEDULED TASKS — a saved prompt that runs "
            "automatically at a future time. Use this when the user asks to do "
            "something on a schedule ('every day at 9am run the tests', 'in an "
            "hour, summarize the logs', 'every 30 minutes check the build'). "
            "Actions: 'add' (needs prompt + kind), 'list', 'remove' (needs id), "
            "'enable'/'disable' (needs id). kind is 'once' (with run_at, an ISO "
            "time), 'interval' (with every_seconds), or 'daily' (with at_hour + "
            "at_minute, local time). Tasks fire automatically while the desktop "
            "GUI is open (its 'Scheduler' toggle is ON by default), or while "
            "'nanocodex schedule run' is running in a terminal. SECURITY / "
            "allow_desktop: defaults false and never widens shell/file access. "
            "It controls whether an UNATTENDED run may drive the DESKTOP "
            "(click/type into apps). Two-sided rule: (1) set allow_desktop=true "
            "when the task's JOB is to act on the desktop unattended — e.g. the "
            "user asks it to auto-reply / take over WeChat, send a message, click "
            "something on a schedule. WITHOUT it the task is given NO desktop "
            "tools at all and will just spin uselessly to the step limit. (2) "
            "Leave it false for everything that doesn't touch the desktop "
            "(running tests, summarizing logs). When you do set it true the tool "
            "echoes a [SECURITY] notice so the user sees the tradeoff."
        )

    @property
    def parameters(self) -> dict[str, Any]:
        return {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "list", "remove", "enable", "disable"],
                },
                "prompt": {
                    "type": "string",
                    "description": "The task prompt to run on schedule (for 'add').",
                },
                "kind": {
                    "type": "string",
                    "enum": ["once", "interval", "daily"],
                    "description": "Recurrence kind (for 'add').",
                },
                "run_at": {
                    "type": "string",
                    "description": "ISO time for kind='once', e.g. 2026-06-01T09:00:00.",
                },
                "every_seconds": {
                    "type": "integer",
                    "description": "Seconds between runs for kind='interval'.",
                },
                "at_hour": {
                    "type": "integer",
                    "description": "Hour 0-23 for kind='daily'.",
                },
                "at_minute": {
                    "type": "integer",
                    "description": "Minute 0-59 for kind='daily'.",
                },
                "task_id": {
                    "type": "string",
                    "description": "Task id for remove/enable/disable.",
                },
                "allow_desktop": {
                    "type": "boolean",
                    "description": (
                        "Default false. When true, this task's UNATTENDED runs "
                        "may drive the DESKTOP (MCP) — clicking/typing into real "
                        "apps (e.g. sending a WeChat message) with no human "
                        "watching. Set true when the task's PURPOSE is an "
                        "unattended desktop action the user asked for (auto-reply "
                        "to WeChat, send a message on a schedule, take over an "
                        "app): without it the task gets NO desktop tools and just "
                        "spins to the step limit. Leave false for tasks that don't "
                        "touch the desktop (tests, log summaries). It does NOT "
                        "widen shell or out-of-sandbox file access either way. The "
                        "user sees a [SECURITY] notice when you set it true."
                    ),
                },
            },
            "required": ["action"],
        }

    async def execute(self, **kwargs: Any) -> str:
        from nanocodex.agent.schedule import ScheduleStore

        action = str(kwargs.get("action", "")).strip()
        store = ScheduleStore()

        if action == "list":
            return self._render_list(store)

        if action == "add":
            return self._add(store, kwargs)

        if action in ("remove", "enable", "disable"):
            task_id = str(kwargs.get("task_id", "")).strip()
            if not task_id:
                return f"Error: '{action}' needs a task_id. Call action='list' to see ids."
            if action == "remove":
                ok = store.remove(task_id)
            else:
                ok = store.set_enabled(task_id, action == "enable")
            if not ok:
                return f"Error: no task with id '{task_id}'."
            return f"Task {task_id} {action}d."

        return f"Error: unknown action {action!r}. Use add/list/remove/enable/disable."

    def _add(self, store, kwargs: dict[str, Any]) -> str:
        prompt = str(kwargs.get("prompt", "")).strip()
        kind = str(kwargs.get("kind", "once")).strip()
        if not prompt:
            return "Error: 'add' needs a 'prompt'."
        allow_desktop = bool(kwargs.get("allow_desktop", False))
        try:
            task = store.add(
                prompt,
                kind=kind,
                run_at=str(kwargs.get("run_at", "") or ""),
                every_seconds=int(kwargs.get("every_seconds", 0) or 0),
                at_hour=int(kwargs.get("at_hour", 9) or 0),
                at_minute=int(kwargs.get("at_minute", 0) or 0),
                allow_desktop=allow_desktop,
            )
        except (ValueError, TypeError) as exc:
            return f"Error: {exc}"
        msg = (
            f"Scheduled task '{task.id}' added ({task.kind}); next run: "
            f"{task.next_run or '(immediate)'}.\n"
            "Note: tasks only fire while 'nanocodex schedule run' is running — "
            "tell the user to start it if it isn't already."
        )
        if allow_desktop:
            # Make the security tradeoff impossible to miss in the relayed output.
            # Plain ASCII only (the HANDOFF notes consoles can choke on non-ASCII).
            msg += (
                "\n\n[SECURITY] this task is marked allow_desktop=TRUE: when it "
                "fires, it can drive the DESKTOP (click/type into real apps) with "
                "nobody watching. Confirm the user truly wants an unattended "
                "desktop action and that the prompt is trusted. It does NOT widen "
                "shell or out-of-sandbox file access."
            )
        return msg

    def _render_list(self, store) -> str:
        if not store.tasks:
            return "No scheduled tasks."
        lines = []
        for t in store.tasks:
            state = "on" if t.enabled else "off"
            # Surface the desktop authorization so a 'list' makes the security
            # tradeoff visible, not just settable.
            desktop = " desktop=ON" if getattr(t, "allow_desktop", False) else ""
            lines.append(
                f"{t.id} [{state}]{desktop} {t.kind} next={t.next_run or '-'} "
                f"runs={t.runs}: {t.prompt[:80]}"
            )
        return "\n".join(lines)
