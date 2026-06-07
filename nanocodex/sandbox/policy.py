"""Codex-style sandbox policy.

Mirrors Codex's three sandbox modes:

    read-only          read anywhere; no writes; no network
    workspace-write    read anywhere; write to workspace + writable roots + temp;
                       no network unless explicitly enabled
    danger-full-access no restrictions

This module makes *policy decisions* (is this path writable? is network allowed?).
Enforcement lives in :mod:`nanocodex.sandbox.executor`. On Windows the
enforcement is policy-level (path checks at the tool boundary), not kernel
isolation -- see the executor docstring.
"""

from __future__ import annotations

import tempfile
from dataclasses import dataclass, field
from pathlib import Path

READ_ONLY = "read-only"
WORKSPACE_WRITE = "workspace-write"
DANGER_FULL_ACCESS = "danger-full-access"


@dataclass
class SandboxPolicy:
    """Resolved filesystem/network permissions for a sandbox mode."""

    mode: str
    workspace: Path
    writable_roots: list[Path] = field(default_factory=list)
    network_access: bool = False
    # Tightened default: writes are confined to the workspace (+ explicit
    # writable_roots). The system temp dir is NO LONGER writable by default —
    # set allow_temp_write=True only if a tool genuinely needs scratch space.
    allow_temp_write: bool = False

    def __post_init__(self) -> None:
        self.workspace = Path(self.workspace).resolve()
        self.writable_roots = [Path(p).resolve() for p in self.writable_roots]

    @classmethod
    def from_config(cls, cfg) -> "SandboxPolicy":  # type: ignore[no-untyped-def]
        return cls(
            mode=cfg.sandbox_mode,
            workspace=cfg.workspace,
            writable_roots=list(cfg.writable_roots),
            network_access=cfg.network_access,
        )

    # --- decisions -------------------------------------------------------

    @property
    def writes_allowed(self) -> bool:
        return self.mode in (WORKSPACE_WRITE, DANGER_FULL_ACCESS)

    def _writable_dirs(self) -> list[Path]:
        roots = [self.workspace, *self.writable_roots]
        if self.allow_temp_write:
            roots.append(Path(tempfile.gettempdir()).resolve())
        return roots

    def can_read(self, path: str | Path) -> bool:
        # All three modes permit reads. (Codex also allows reads broadly;
        # secret-file protection is handled separately at the tool layer.)
        return True

    def can_write(self, path: str | Path) -> bool:
        if self.mode == DANGER_FULL_ACCESS:
            return True
        if self.mode == READ_ONLY:
            return False
        target = Path(path)
        if not target.is_absolute():
            target = (self.workspace / target)
        try:
            target = target.resolve()
        except OSError:
            return False
        for root in self._writable_dirs():
            if target == root or root in target.parents:
                return True
        return False

    def describe(self) -> str:
        net = "network on" if self.network_access else "network off"
        if self.mode == DANGER_FULL_ACCESS:
            return f"{self.mode} (no restrictions, {net})"
        if self.mode == READ_ONLY:
            return f"{self.mode} (no writes, {net})"
        roots = ", ".join(str(p) for p in self._writable_dirs())
        return f"{self.mode} ({net}; writable: {roots})"
