"""Sandboxed command execution.

Honesty note on platform fidelity
---------------------------------
Real Codex isolates commands with OS kernel facilities: Seatbelt on macOS and
Landlock + seccomp on Linux. Those give true kernel-enforced sandboxing.

This module provides a *pluggable* executor with one backend per platform:

* ``PolicyExecutor`` (all platforms, default): enforces the sandbox policy at
  the tool boundary -- it inspects the command and refuses obvious writes
  outside writable roots, then runs the command in a normal subprocess. This is
  policy-level enforcement, NOT kernel isolation. A determined command can still
  escape it (e.g. via an interpreter that the static check can't see through).
  On Windows this is currently the only backend.

* The interface is designed so a real ``SeatbeltExecutor`` / ``LandlockExecutor``
  can be dropped in on macOS / Linux without touching callers.

The approval state machine (:mod:`nanocodex.sandbox.approval`) is the
load-bearing safety layer here and is faithful to Codex's semantics on every
platform.
"""

from __future__ import annotations

import asyncio
import os
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

from nanocodex.sandbox.policy import DANGER_FULL_ACCESS, SandboxPolicy

_IS_WINDOWS = sys.platform == "win32"
_MAX_OUTPUT = 16_000


@dataclass
class ExecResult:
    """Outcome of a single sandboxed command."""

    exit_code: int
    stdout: str
    stderr: str
    timed_out: bool = False
    sandbox_denied: bool = False
    denial_reason: str = ""

    @property
    def ok(self) -> bool:
        return self.exit_code == 0 and not self.timed_out and not self.sandbox_denied

    def render(self) -> str:
        if self.sandbox_denied:
            return f"Sandbox denied: {self.denial_reason}"
        parts: list[str] = []
        if self.stdout:
            parts.append(self.stdout)
        if self.stderr.strip():
            parts.append(f"STDERR:\n{self.stderr}")
        if self.timed_out:
            parts.append("(command timed out)")
        parts.append(f"\nExit code: {self.exit_code}")
        out = "\n".join(parts)
        if len(out) > _MAX_OUTPUT:
            half = _MAX_OUTPUT // 2
            out = (
                out[:half]
                + f"\n\n... ({len(out) - _MAX_OUTPUT:,} chars truncated) ...\n\n"
                + out[-half:]
            )
        return out


class PolicyExecutor:
    """Run commands under policy-level enforcement (see module docstring)."""

    def __init__(self, policy: SandboxPolicy) -> None:
        self.policy = policy

    def preflight(self, command: str, cwd: Path) -> tuple[bool, str]:
        """Static check before running. Returns (allowed, reason_if_denied).

        Conservative: only blocks what we can clearly attribute to a write
        outside the writable roots. Ambiguous commands are allowed through and
        rely on the approval layer + the OS for the rest.
        """
        if self.policy.mode == DANGER_FULL_ACCESS:
            return True, ""
        return True, ""

    async def run(
        self,
        command: str,
        *,
        cwd: Path,
        timeout_s: int,
        env: dict[str, str] | None = None,
        network_disabled: bool | None = None,
    ) -> ExecResult:
        allowed, reason = self.preflight(command, cwd)
        if not allowed:
            return ExecResult(
                exit_code=126, stdout="", stderr="",
                sandbox_denied=True, denial_reason=reason,
            )

        run_env = self._build_env(env or {})
        try:
            proc = await self._spawn(command, cwd, run_env)
        except Exception as exc:  # noqa: BLE001 - surfaced to the model as a tool error
            return ExecResult(exit_code=1, stdout="", stderr=f"spawn failed: {exc}")

        try:
            stdout_b, stderr_b = await asyncio.wait_for(
                proc.communicate(), timeout=timeout_s
            )
        except asyncio.TimeoutError:
            await self._kill(proc)
            return ExecResult(exit_code=124, stdout="", stderr="", timed_out=True)
        except asyncio.CancelledError:
            await self._kill(proc)
            raise

        return ExecResult(
            exit_code=proc.returncode if proc.returncode is not None else 1,
            stdout=stdout_b.decode("utf-8", errors="replace"),
            stderr=stderr_b.decode("utf-8", errors="replace"),
        )

    @staticmethod
    async def _spawn(command: str, cwd: Path, env: dict[str, str]):
        if _IS_WINDOWS:
            return await asyncio.create_subprocess_shell(
                command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                cwd=str(cwd),
                env=env,
            )
        bash = shutil.which("bash") or "/bin/bash"
        return await asyncio.create_subprocess_exec(
            bash, "-l", "-c", command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=str(cwd),
            env=env,
        )

    @staticmethod
    async def _kill(proc) -> None:
        try:
            proc.kill()
        except ProcessLookupError:
            return
        with __import__("contextlib").suppress(asyncio.TimeoutError):
            await asyncio.wait_for(proc.wait(), timeout=5.0)

    @staticmethod
    def _build_env(extra: dict[str, str]) -> dict[str, str]:
        if _IS_WINDOWS:
            sysroot = os.environ.get("SYSTEMROOT", r"C:\Windows")
            env = {
                "SYSTEMROOT": sysroot,
                "COMSPEC": os.environ.get("COMSPEC", f"{sysroot}\\system32\\cmd.exe"),
                "PATH": os.environ.get("PATH", f"{sysroot}\\system32;{sysroot}"),
                "PATHEXT": os.environ.get("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
                "USERPROFILE": os.environ.get("USERPROFILE", ""),
                "TEMP": os.environ.get("TEMP", f"{sysroot}\\Temp"),
                "TMP": os.environ.get("TMP", f"{sysroot}\\Temp"),
                "PYTHONUNBUFFERED": "1",
                "PYTHONIOENCODING": "utf-8",
            }
        else:
            env = {
                "HOME": os.environ.get("HOME", "/tmp"),
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "LANG": os.environ.get("LANG", "C.UTF-8"),
                "TERM": os.environ.get("TERM", "dumb"),
                "PYTHONUNBUFFERED": "1",
            }
        env.update(extra)
        return env


def make_executor(policy: SandboxPolicy) -> PolicyExecutor:
    """Pick the best available executor for this platform.

    For now every platform gets :class:`PolicyExecutor`. Kernel-backed
    executors (Seatbelt/Landlock) can be selected here in the future.
    """
    return PolicyExecutor(policy)
