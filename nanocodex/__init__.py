"""nanocodex: a minimal Codex-style coding agent on a DeepSeek backend.

Architecture (independent rewrite, inspired by nanobot's agent loop):

    config       -> resolve model / sandbox / approval settings
    provider     -> talk to the LLM (DeepSeek, OpenAI-compatible)
    sandbox      -> Codex policy + approval state machine + executor
    tools        -> Codex tool layer: shell, apply_patch (V4A), update_plan, read_file
    agent        -> prompt + session + turn loop
    cli          -> interactive REPL wiring approval to the console
"""

__version__ = "0.1.0"
