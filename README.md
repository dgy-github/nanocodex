# nanocodex

A minimal **Codex-style coding agent** with a **DeepSeek** backend, built as an
independent rewrite inspired by [nanobot](../nanobot)'s agent loop.

It reproduces Codex's signature **tool layer** — `apply_patch` (the V4A patch
format), `shell`, `update_plan`, and `read_file` — on top of a small async turn
loop, and faithfully replicates Codex's **sandbox + approval** semantics.

## What it is (and isn't)

- **Is:** a working coding agent that plans, edits files via patches, runs
  commands, and verifies its work, driven by DeepSeek (or any OpenAI-compatible
  endpoint).
- **Sandbox honesty:** the *policy + approval state machine* is faithful to
  Codex on every platform. The *enforcement* is policy-level (path checks at the
  tool boundary) — **not kernel isolation**. Real Codex uses Seatbelt (macOS) /
  Landlock+seccomp (Linux); those backends can be dropped into
  `nanocodex/sandbox/executor.py` later. On Windows, policy-level is the only
  backend today.

## Layout

```
nanocodex/
  config.py            resolve model / sandbox / approval (reads ~/.deepseek/config.toml)
  provider/            LLM backends (DeepSeek, OpenAI-compatible)
  sandbox/             policy + approval state machine + executor
  tools/               shell, apply_patch (V4A), update_plan, read_file
  agent/               system prompt, session (jsonl), turn loop
  cli.py               interactive REPL
```

## Configuration

Settings resolve in this order (highest wins):

```
CLI flags  >  environment  >  ~/.deepseek/config.toml  >  ~/.codex/config.toml  >  defaults
```

The DeepSeek API key is read from `~/.deepseek/config.toml` (`api_key` or
`providers.deepseek.api_key`) or `$DEEPSEEK_API_KEY`. It is never printed or
logged.

- **Sandbox modes:** `read-only`, `workspace-write` (default), `danger-full-access`
- **Approval policies:** `untrusted`, `on-failure`, `on-request` (default), `never`

## Usage

```bash
pip install -e .

nanocodex                                  # REPL in the current directory
nanocodex "add a --json flag to the CLI"   # one-shot: run a task and exit
nanocodex --sandbox read-only --approval untrusted
nanocodex --cd path/to/project -m deepseek-chat
```

In the REPL: `/plan` shows the current plan, `/exit` quits.

## Tests

```bash
pip install -e ".[dev]"
pytest
```

Tests are offline (mocked provider); no network or API key required.
