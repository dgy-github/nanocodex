"""Codex-style sandbox: policy + approval + executor."""

from nanocodex.sandbox.approval import (
    Approver,
    ApprovalRequest,
    Decision,
    NEVER,
    ON_FAILURE,
    ON_REQUEST,
    UNTRUSTED,
)
from nanocodex.sandbox.executor import ExecResult, PolicyExecutor, make_executor
from nanocodex.sandbox.policy import (
    DANGER_FULL_ACCESS,
    READ_ONLY,
    WORKSPACE_WRITE,
    SandboxPolicy,
)

__all__ = [
    "Approver",
    "ApprovalRequest",
    "Decision",
    "NEVER",
    "ON_FAILURE",
    "ON_REQUEST",
    "UNTRUSTED",
    "ExecResult",
    "PolicyExecutor",
    "make_executor",
    "SandboxPolicy",
    "READ_ONLY",
    "WORKSPACE_WRITE",
    "DANGER_FULL_ACCESS",
]
