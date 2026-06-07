# nanocodex

`nanocodex` is a small Codex-style coding agent for local experiments. It keeps
the core loop intentionally simple: a chat-completions model proposes tool calls,
the agent executes safe file/shell tools, records the session, and asks the model
to continue until the task is done.

The project is mainly useful for testing how a coding agent behaves with
DeepSeek, OpenAI-compatible local models, MCP tools, skills, sandbox policies,
context compaction, and a lightweight Windows GUI.

## Features

- Codex-style tools: `apply_patch`, `shell`, `update_plan`, `read_file`,
  `web_search`, memory, and scheduled tasks.
- DeepSeek / OpenAI-compatible backend via `/v1/chat/completions`.
- Local model support by changing `base_url`, for example vLLM, llama-server,
  LM Studio, or any OpenAI-compatible server.
- Sandbox and approval policy state machine: `read-only`, `workspace-write`,
  `danger-full-access`; `untrusted`, `on-failure`, `on-request`, `never`.
- MCP integration from `~/.nanocodex/mcp.toml`, exposed as
  `mcp__<server>__<tool>` tools.
- Skills system from `~/.nanocodex/skills/<name>/SKILL.md`, plus built-in
  `code-review`, `debug`, and `write-tests` skills.
- Session JSONL, context compaction, prompt enhancement, token usage/cost
  accounting, GUI, scheduler, and A/B worktree comparison.

## Install

```powershell
cd path\to\nanocodex
python -m pip install -e ".[dev]"
```

Run the CLI:

```powershell
nanocodex --cd .
nanocodex "add a --json flag to the CLI"
```

Run the GUI:

```powershell
nanocodex-gui --cd .
```

On Windows you can also double-click `nanocodex-gui.cmd` after installation.

## Configuration

Settings resolve in this order:

```text
CLI flags > environment > ~/.nanocodex/config.toml > ~/.deepseek/config.toml > ~/.codex/config.toml > defaults
```

The real API key should stay outside the repository. Use one of these:

```powershell
$env:DEEPSEEK_API_KEY = "sk-..."
$env:NANOCODEX_API_KEY = "sk-..."
```

Or create `~/.nanocodex/config.toml`:

```toml
api_key = "sk-..."
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
```

A full example is in `config.example.toml`.

## Local Model / OpenAI-Compatible Endpoint

For a local model server, point `base_url` at the server's `/v1` root. Many
local servers ignore the API key, but `nanocodex` still requires a non-empty
placeholder because the OpenAI SDK expects one.

Example:

```toml
api_key = "local-dev-key"
base_url = "http://127.0.0.1:8005/v1"
model = "Qwen3.6-27B-Q4_K_M"
```

Quick connectivity check:

```powershell
curl http://127.0.0.1:8005/v1/models
```

## MCP

MCP servers are opt-in and run outside the sandbox. Configure them in:

```text
~/.nanocodex/mcp.toml
```

Example:

```toml
[mcp_servers.fetch]
command = "uvx"
args = ["mcp-server-fetch"]
```

Then start nanocodex with MCP enabled:

```powershell
nanocodex --mcp
```

See `mcp.example.toml` for more examples.

## Skills

Skills are reusable instruction documents:

```text
~/.nanocodex/skills/<skill-name>/SKILL.md
```

Only each skill's name and description are injected into the system prompt. The
full body is read on demand, so a large skills library does not automatically
consume the full context window.

Minimal skill:

```markdown
---
name: code-review
description: Review code changes and focus on bugs, regressions, and missing tests.
---

# Code Review

Look for behavior regressions first, then missing tests, then maintainability.
```

The package also ships built-in read-only skills under
`nanocodex/builtin_skills/`.

## Tests

```powershell
python -m pytest -q
```

The test suite is offline and uses mocked providers; no real API key or network
call is required.

## Security Notes

- Do not commit real API keys. `.env`, `*.key`, token files, and local handoff
  files are ignored.
- The sandbox is policy-level on Windows. It gates tool actions and writable
  roots, but it is not kernel isolation.
- MCP tools execute external subprocesses outside the sandbox. Only enable MCP
  servers you trust.
