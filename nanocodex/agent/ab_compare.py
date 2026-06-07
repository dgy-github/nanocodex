"""A/B configuration comparison: run one task under two configs, compare.

The user picks two configurations (each a set of ``_build_loop`` overrides —
model / reasoning_effort / sandbox / approval) and one task prompt. Each side
runs the SAME prompt in its OWN throwaway git worktree, so both start from an
identical clean tree and their file edits never collide. When both finish we
show each side's diff + cost + latency + stop_reason; the user adopts one
side's changes into the real workspace and we discard the other.

Design split (mirrors schedule.py / skills_store.py):

* **Pure logic here.** The dataclasses, the human-readable summaries, and the
  deterministic worktree naming are pure (data in, text out, no clocks) so they
  unit-test offline. The thin git I/O helpers (``create_ab_worktrees`` etc.)
  take an explicit workspace + injected ids so they test against a real tmp
  git repo without hidden state.
* **No threads, no GUI.** The GUI layer owns the daemon thread, the desktop
  lock, and the asyncio loop; it calls these helpers and feeds results back.

Hard requirement: the real workspace must be a CLEAN git repo. Worktrees need
git, and adopting a side's changes applies its diff back onto that clean base.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from nanocodex.agent.pricing import cost_usd

# Where throwaway A/B worktrees live: a sibling temp area OUTSIDE the workspace
# (a worktree inside the workspace would get scanned/added by its own runs).
_WORKTREE_PREFIX = "nanocodex-ab"


@dataclass
class ABConfig:
    """One side of the comparison: a label + a set of _build_loop overrides."""

    label: str
    overrides: dict[str, Any] = field(default_factory=dict)


@dataclass
class ABResult:
    """One side's outcome after running the task in its worktree."""

    label: str
    text: str = ""
    stop_reason: str = ""
    iterations: int = 0
    usage: dict[str, int] = field(default_factory=dict)
    cost: float | None = None          # USD, None when model price unknown
    elapsed_s: float = 0.0
    diff: str = ""                     # `git diff` of the worktree's changes
    worktree_path: str = ""
    error: str = ""                    # non-empty if the run raised


def build_result(
    config: ABConfig,
    turn_result: Any,
    *,
    elapsed_s: float,
    diff: str,
    worktree_path: str,
    error: str = "",
) -> ABResult:
    """Assemble an ABResult from a finished TurnResult (pure).

    Pulls the model out of the config's overrides to price the run's usage.
    A missing/unknown model price yields cost=None (shown as "unknown", never a
    misleading $0.00).
    """
    model = str(config.overrides.get("model") or "")
    usage = dict(getattr(turn_result, "usage", {}) or {})
    return ABResult(
        label=config.label,
        text=str(getattr(turn_result, "text", "") or ""),
        stop_reason=str(getattr(turn_result, "stop_reason", "") or ""),
        iterations=int(getattr(turn_result, "iterations", 0) or 0),
        usage=usage,
        cost=cost_usd(model, usage) if model else None,
        elapsed_s=round(float(elapsed_s), 2),
        diff=diff or "",
        worktree_path=worktree_path,
        error=error,
    )


def _fmt_cost(cost: float | None) -> str:
    if cost is None:
        return "unknown"
    if cost < 0.01:
        return f"${cost:.4f}"
    return f"${cost:.2f}"


def _diff_stat(diff: str) -> str:
    """Tiny +added/-removed summary of a unified diff (pure)."""
    if not diff.strip():
        return "no file changes"
    added = sum(
        1 for ln in diff.splitlines()
        if ln.startswith("+") and not ln.startswith("+++")
    )
    removed = sum(
        1 for ln in diff.splitlines()
        if ln.startswith("-") and not ln.startswith("---")
    )
    return f"+{added} -{removed} lines"


def summarize_result(r: ABResult) -> str:
    """One-side human-readable summary (pure)."""
    if r.error:
        return f"[{r.label}] ERROR: {r.error}"
    parts = [
        f"[{r.label}]",
        f"stop={r.stop_reason}",
        f"steps={r.iterations}",
        f"cost={_fmt_cost(r.cost)}",
        f"time={r.elapsed_s}s",
        _diff_stat(r.diff),
    ]
    return "  ".join(parts)


def format_ab_comparison(a: ABResult, b: ABResult) -> str:
    """Side-by-side comparison text for the result dialog (pure)."""
    lines = [
        "A/B comparison",
        "",
        summarize_result(a),
        summarize_result(b),
    ]
    return "\n".join(lines)


def worktree_name(label: str, token: str) -> str:
    """Deterministic, filesystem-safe worktree dir name (pure, testable).

    *token* is an injected unique id (the GUI passes a timestamp/uuid) so the
    name is stable for a given (label, token) and collisions are the caller's
    concern, not a hidden clock here.
    """
    safe_label = "".join(c for c in label if c.isalnum() or c in "._-") or "x"
    safe_token = "".join(c for c in token if c.isalnum() or c in "._-") or "0"
    return f"{_WORKTREE_PREFIX}-{safe_label}-{safe_token}"


# --- thin git I/O (explicit workspace in, no hidden state) -----------------

class ABGitError(RuntimeError):
    """Raised when the workspace can't host an A/B run (not git / dirty / etc)."""


def _git(args: list[str], cwd: Path) -> str:
    """Run a git command, returning stdout. Raises ABGitError on failure."""
    try:
        proc = subprocess.run(
            ["git", *args], cwd=str(cwd),
            capture_output=True, text=True, encoding="utf-8",
        )
    except OSError as exc:
        raise ABGitError(f"git not available: {exc}") from exc
    if proc.returncode != 0:
        raise ABGitError(
            f"git {' '.join(args)} failed: {proc.stderr.strip() or proc.stdout.strip()}"
        )
    return proc.stdout


def ensure_clean_git_workspace(workspace: Path) -> str:
    """Verify *workspace* is a git repo with no uncommitted changes.

    Returns the current HEAD commit hash (the shared A/B base). Raises
    ABGitError with a user-facing message otherwise.
    """
    inside = _git(["rev-parse", "--is-inside-work-tree"], workspace).strip()
    if inside != "true":
        raise ABGitError("workspace is not a git repository")
    status = _git(["status", "--porcelain"], workspace).strip()
    if status:
        raise ABGitError(
            "workspace has uncommitted changes; commit or stash them before A/B"
        )
    return _git(["rev-parse", "HEAD"], workspace).strip()


def create_worktree(workspace: Path, base_commit: str, name: str, tmp_root: Path) -> Path:
    """Add a detached git worktree at *tmp_root/name* on *base_commit*."""
    path = tmp_root / name
    _git(["worktree", "add", "--detach", str(path), base_commit], workspace)
    return path


def collect_worktree_diff(worktree: Path) -> str:
    """Return a unified diff of all changes (staged+unstaged) in *worktree*.

    Stages everything first (so new/untracked files appear in the diff) but
    never commits — the diff is the artifact we adopt or discard.
    """
    _git(["add", "-A"], worktree)
    return _git(["diff", "--cached"], worktree)


def adopt_diff(workspace: Path, diff: str) -> None:
    """Apply a collected diff onto the real *workspace* (no commit).

    Leaves the changes in the working tree for the user to review/commit, the
    same as if the agent had edited the files directly.
    """
    if not diff.strip():
        return  # nothing to adopt
    try:
        proc = subprocess.run(
            ["git", "apply", "--whitespace=nowarn", "-"],
            cwd=str(workspace), input=diff, text=True,
            capture_output=True, encoding="utf-8",
        )
    except OSError as exc:
        raise ABGitError(f"git apply failed to start: {exc}") from exc
    if proc.returncode != 0:
        raise ABGitError(f"git apply failed: {proc.stderr.strip()}")


def cleanup_worktree(workspace: Path, worktree: Path) -> None:
    """Remove a worktree (force), best-effort — never raises."""
    try:
        _git(["worktree", "remove", "--force", str(worktree)], workspace)
    except ABGitError:
        pass
