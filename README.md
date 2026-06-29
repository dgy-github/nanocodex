# nanocodex

English | [简体中文](README.zh-CN.md)

## Capability Page

[Open the live capability page](https://dgy-github.github.io/nanocodex/nanocodex.html) · [View the HTML in this repo](nanocodex.html)

[Design brief PDF](docs/ai-coding-agent-design-brief.pdf) · [Design brief HTML](docs/ai-coding-agent-design-brief.html)

📖 **[设计理念手册（中文）](docs/design-philosophy.zh-CN.md)** — why the tiered orchestrator, recursive decomposition, tool-less reasoning nodes, progressive disclosure, vision routing, and the benchmark methodology are built the way they are.

[![nanocodex GUI preview: sessions, tool calls, MCP, skills, cost, and tests](assets/nanocodex-ui-preview.svg)](https://dgy-github.github.io/nanocodex/nanocodex.html)

`nanocodex` is a compact but full-featured Codex-style coding agent. A
chat-completions model proposes tool calls, the agent runs sandboxed
file/shell tools, records the session, and loops until the task is done. It
runs against DeepSeek's hosted API or any OpenAI-compatible local model, and
ships with MCP integration, a skills system, a sandbox/approval state machine,
context compaction, token-cost accounting, a Windows GUI, a scheduler, and
git-worktree A/B comparison.

The project has two clear stages. The important shift is not just "same
features in another language"; it is an architectural split and a release
performance upgrade.

## Project Phases

### Stage 1: Python Baseline

The Python implementation under `nanocodex/` is the original feature-complete
agent line. It was optimized for fast product exploration: prove the agent
loop, tool UX, approval model, and desktop workflows before locking the system
into a stricter runtime.

**Architecture**

- A Python package centered on a compact async agent loop: model call -> tool
  execution -> session update -> next model call.
- Tooling, provider, sandbox, MCP, skills, memory, scheduler, compaction, and
  GUI modules are easy to extend independently while the product surface is
  still changing.
- Runtime contracts are intentionally dynamic, which made it cheap to add
  features such as MCP marketplace entries, prompt enhancement, image input,
  session resume/fork, and A/B worktree comparison.
- The Windows GUI uses Tkinter, keeping the first desktop version dependency
  light and simple to debug.

**Performance and delivery profile**

- Best for iteration speed: no compile step, quick experiments, and a large
  offline test suite around mocked providers.
- 420 offline tests validate behavior without API keys or network calls.
- Runtime delivery still depends on a Python install, package environment, and
  import-time startup cost; desktop distribution is therefore less clean than a
  native binary.
- Dynamic boundaries are productive during exploration, but become harder to
  reason about as sandboxing, tool execution, memory, MCP, and parallel agent
  flows grow.

### Stage 2: Rust Rewrite

The Rust implementation under `rust/` is the current release line. It keeps the
Python tree intact while rebuilding the core as small crates plus a Tauri
desktop shell. The rewrite keeps the proven Python feature map, but changes the
internal shape so the project can ship as a smaller, faster, more predictable
tool.

**Architecture**

- The workspace is split by responsibility: `ncx-sandbox`, `ncx-config`,
  `ncx-provider`, `ncx-tools`, `ncx-core`, and `ncx-cli`.
- Core contracts are typed: provider responses, tool calls, sandbox decisions,
  session messages, memory entries, and orchestrator results cross crate
  boundaries explicitly.
- Tool execution is centralized behind `ToolContext` and `ToolRegistry`, so
  sandbox policy, approval policy, timeouts, search, and memory are attached at
  the boundary where actions actually happen.
- The orchestration layer adds task classification, main/fast model routing,
  isolated worker workspaces, verifier selection, and promotion of the winning
  worker back into the real workspace.
- Project memory is local and auditable: verified notes live in
  `.ncx/memory/LEARNINGS.md`, while candidate learnings wait in
  `.ncx/memory/PROPOSALS.md` until CLI/Tauri review accepts them. Startup uses
  cheap heuristic deduplication, while `ncx --memory-merge` or the Tauri Memory
  panel run explicit heuristic/LLM-backed consolidation.
- The desktop line moves to Tauri v2 + Svelte 5, separating the native backend
  from the UI surface and preparing a smaller release bundle than the Python GUI
  path.

**Performance and delivery profile**

- The CLI builds to a native `ncx.exe`, so users do not need Python, virtual
  environments, or editable installs.
- Release builds use strip, LTO, size optimization, and a Windows GNU target;
  the current CLI zip is under 2 MB while still including README, license, and
  config example files.
- Startup avoids Python interpreter/import overhead and is suitable for short
  one-shot commands as well as interactive REPL use.
- Typed ownership makes parallel worker isolation and result promotion easier
  to reason about without shared mutable state leaks.
- 319 source-counted offline Rust test functions cover the current crate
  boundary, including memory consolidation, provider request/response parsing,
  sandbox policy, tools, orchestration, and context-editing regressions.

**Platform control-plane upgrades**

- **Task budget:** every model call receives a runtime budget note with current
  model-call, tool-call, and context limits; the loop stops cleanly when model
  or tool budgets are exhausted and backfills unanswered tool calls so the
  message history stays valid. The Rust REPL exposes `/budget` for per-task
  limits, session use, and last-turn remaining budget; completed turns append
  `.nanocodex/task-ledger.jsonl`, and `/budget report` / `--budget-report` show
  recent task records with wall time, approvals, stop reasons, token totals,
  average task time, budget-exhaustion rate, model/tool budget utilization, and
  visible-vs-called tool traces for tool-search evaluation. `/tools eval [N]`
  and `--tools-eval-report` turn the same ledger rows into offline
  schema-recall reports.
  The orchestrator shares the same parent budget across reasoning nodes and
  parallel workers so subagents cannot each spend a full independent task
  budget.
- **Context editing:** the full local session remains intact, but the provider
  sees a send-time edited view that compresses old tool results and drops older
  prefixes once the context budget is exceeded. Before an old prefix is
  dropped, Rust creates a deterministic assistant summary checkpoint so
  `/compact`, `--resume`, payload snapshots, and Usage telemetry can still audit
  what was omitted. The checkpoint also carries deterministic focus anchors:
  older messages whose lexical terms overlap the latest user request. The Rust
  REPL exposes `/context` for active policy, session size, last-turn telemetry,
  next-send preview, and recent provider payload snapshots via
  `/context payload [N]`. Telemetry now breaks the edited payload into
  context-pack buckets: system prompt, runtime notes, memory recall, history,
  and tool-result characters; configurable history and total tool-result bucket
  caps actively govern the send-time view. When these caps are omitted or set
  to `0`, the Rust config loader derives them from `context_token_budget` and
  `context_window`, so 1M-context model profiles are not forced through the old
  120k-character send-time ceiling. Regression tests now cover large tool
  outputs, long histories, runtime-note/memory competition, and tool-call/result
  pairing after compaction.
- **Tool search:** tools are registered into a catalog. Small registries expose
  all tools; larger registries expose core tools plus `tool_search`, and search
  hits are made visible in the next schema view. Ranking is namespace-aware for
  MCP tools (`mcp__server__tool`) and has 29-query gold-case regression
  coverage across core tools, MCP connectors, and release packaging. MCP tool
  descriptions also receive deterministic category/capability hints for sparse
  connector metadata. The Rust
  REPL exposes `/tools` to inspect the
  catalog, visible schema view, and active search hints. The Tauri Tools panel
  reads the same live catalog and shows MCP server status, registered tool
  counts, startup elapsed time, and the last connection error. Completed turns
  persist the visible tool schemas and actual called tools into the task ledger,
  and `/tools eval [N]` / `--tools-eval-report` report schema recall, missed
  calls, MCP recall, and recent miss samples from those traces.
- **Semantic memory:** every turn receives a query-scoped memory recall note at
  send time. Retrieval uses a hybrid lexical semantic ranker: keywords, tags,
  phrase matches, Jaccard similarity, recency, and a small domain synonym map
  for agent/runtime terms. The Rust REPL exposes `/memory` for the backing file,
  entry count, recent notes, tag summary, and query-scoped recall preview.
- **Deterministic hooks:** `[[hooks]]` can run project commands before or after
  matching tools and at turn lifecycle points. A failing `pre_tool` or
  `user_prompt` hook blocks the action; `post_tool` and `stop` output is
  appended for audit, formatting, and quality-gate workflows.
- **Checkpoint / restore:** the Rust CLI and Tauri GUI create file checkpoints
  before model turns. CLI exposes `/checkpoint`, `/checkpoints`, and
  `/restore <id>`; the GUI exposes a checkpoint panel for manual save, list, and
  restore.

### Why Rust For Stage 2

Rust was chosen because the second stage is about productizing the agent, not
only adding more features:

- **Architecture hardening:** the sandbox, approval engine, provider adapter,
  tool registry, memory store, and orchestrator now have explicit typed
  contracts instead of relying on Python's dynamic object boundaries.
- **Predictable action boundary:** file, shell, search, and memory operations
  all pass through one tool context, which keeps approval and sandbox checks
  close to execution.
- **Parallel orchestration:** isolated worker copies, verifier selection, and
  result promotion are safer when ownership is explicit and data movement is
  visible in the type system.
- **Runtime control plane:** task budgets, context editing, tool search,
  semantic memory, and sandboxed `code_exec` sit in the Rust runtime boundary
  rather than depending on model-side conventions alone.
- **Native release performance:** a small `ncx.exe` starts without interpreter
  setup, making one-shot CLI tasks feel immediate and making distribution much
  easier for Windows users.
- **Desktop packaging path:** Tauri provides a native shell with a web UI
  frontend, a better long-term packaging fit than growing the Tkinter prototype.

## Claude Code / Fable Gap Calibration

As of 2026-06-29, public Claude Code documentation describes a broader
Anthropic platform surface than a local open harness: terminal, IDE, desktop,
and browser entry points; MCP, hooks, skills, auto memory, agent teams, and
cloud/scheduled sessions; plus 1M-context variants for Fable 5, Opus 4.8, and
Sonnet 4.6. `nanocodex` should therefore be read as a compact Rust agent
runtime, not as a parity claim with the full Anthropic platform.
Reference surface: Claude Code
[overview](https://code.claude.com/docs/en/overview),
[context window](https://code.claude.com/docs/en/context-window),
[memory](https://code.claude.com/docs/en/memory),
[MCP](https://code.claude.com/docs/en/mcp), and
[hooks](https://code.claude.com/docs/en/hooks), plus Anthropic's
[models overview](https://docs.anthropic.com/en/docs/about-claude/models/overview).
Detailed current backlog and gap sizing live in
[`docs/claude-fable-gap-roadmap.zh-CN.md`](docs/claude-fable-gap-roadmap.zh-CN.md).

The rough current estimate is:

| Area | nanocodex Rust estimate | Main reason |
| --- | ---: | --- |
| Local CLI harness and tool loop, assuming a similar frontier model | 55-65% | The typed Rust loop, sandbox, approvals, tool registry, memory recall, context editing, sandboxed `code_exec`, checkpoints, MCP, skills, and Tauri GUI cover much of the local coding-agent harness. |
| End-to-end performance with the default DeepSeek-compatible model line versus Claude Code on Fable-class models | 35-45% | The harness is close enough for many workflows, but model reasoning, latency, tool discipline, and Anthropic-native integrations dominate hard tasks. |
| Release/distribution ergonomics for Windows local use | 60-70% | Native CLI zip and Tauri NSIS installer are in place, but the ecosystem and cross-surface product polish are not Anthropic-scale. |

The four platform controls requested for this stage are now present, but they
are local-runtime implementations:

| Capability | Current coverage | Remaining gap |
| --- | ---: | --- |
| Task budget | 82-90% | Runtime model/tool budgets are enforced and visible to the model; CLI and GUI write/read a task ledger with trend/utilization analytics; orchestrator workers now share the parent budget instead of each receiving a fresh full budget. Remaining gaps are cloud task quotas, remote queue governance, and hosted execution analytics. |
| Context editing | 72-80% | Send-time editing compresses tool results, derives 1M-friendly character and bucket caps from `context_token_budget`/`context_window`, and materializes deterministic summary checkpoints with focus anchors before old prefixes are dropped; provider payload snapshots, context-pack bucket telemetry, and regression coverage for large tool outputs/long histories make the actual model input more auditable. Still missing Anthropic-scale long-context model quality, model-guided focus compaction, platform automatic compact, and broader quality evaluation suites. |
| Tool search / connectors | 70-80% | Tool catalogs, namespace-aware `tool_search`, GUI MCP runtime status, 29-query cross-category gold-case ranking tests, deterministic MCP category/capability hints, visible-vs-called task-ledger traces, `/tools eval` / `--tools-eval-report` schema-recall reporting, and an auditable `connectors.toml` install/auth spec reduce schema and connector ambiguity. Missing complete OAuth login UX, remote transport startup, a managed registry, broader trace corpus, richer category ontologies, and large-scale dynamic tool ranking. |
| Semantic memory | 74-82% | Query-scoped lexical-semantic recall, local vector sidecar recall, `remember`, LLM merge, CLI/Tauri proposal review, proposal editing, batch accept/reject, handoff/release-document harvesting, and runtime correction/failure proposal harvesting exist. Remaining gaps are external embedding providers and platform-grade long-term memory. |

The largest remaining gaps are not ordinary Rust code gaps. They are
platform-level gaps: Anthropic's native tool/connectors ecosystem, managed
task budgets and cloud execution, model-integrated context management,
first-party memory behavior, and model-side code execution/analysis ability.
The new local `code_exec` tool narrows the harness gap for quick calculations
and parsing, but it still runs through the local shell sandbox rather than a
model-native hosted execution environment.

## Table of Contents

- [Project Phases](#project-phases)
- [Claude Code / Fable Gap Calibration](#claude-code--fable-gap-calibration)
- [Claude/Fable Gap Roadmap](docs/claude-fable-gap-roadmap.zh-CN.md)
- [Highlights](#highlights)
- [Architecture](#architecture)
- [Tools](#tools)
- [Install](#install)
- [Quick Start](#quick-start)
- [Configuration](#configuration)
- [Custom Slash Commands](#custom-slash-commands)
- [Local Model / OpenAI-Compatible Endpoint](#local-model--openai-compatible-endpoint)
- [Sandbox & Approval](#sandbox--approval)
- [MCP](#mcp)
- [Skills](#skills)
- [Memory & AGENTS.md](#memory--agentsmd)
- [Sessions, Resume & History](#sessions-resume--history)
- [Context Compaction](#context-compaction)
- [Token Usage & Cost](#token-usage--cost)
- [Scheduler](#scheduler)
- [A/B Worktree Comparison](#ab-worktree-comparison)
- [GUI](#gui)
- [Tests](#tests)
- [Release Packaging](#release-packaging)
- [Security Notes](#security-notes)

## Highlights

- **Codex-style agent loop** — streaming token output, multi-round tool calls,
  cancellation, and per-turn usage accounting.
- **DeepSeek + any OpenAI-compatible backend** — point `base_url` at the hosted
  API or a local server (vLLM, llama-server, LM Studio, …).
- **Sandbox & approval state machine** — three sandbox modes and four approval
  policies gate every file/shell/network action.
- **MCP integration** — opt in with `--mcp` to load `mcp.toml` servers; tools
  surface as `mcp__<server>__<tool>`. The legacy Python GUI also keeps a
  built-in / remote MCP marketplace installer.
- **Skills system** — user skills plus three built-in coding skills; only
  name + description are injected, bodies load on demand.
- **Custom slash commands** — prompt-backed project/user commands in
  `.nanocodex/commands`, with `.claude/commands` compatibility.
- **Persistent memory + AGENTS.md / CLAUDE.md** — durable notes plus layered
  project instructions injected each turn.
- **Browsable session history** — JSONL logs, full-transcript snapshots, resume,
  and fork.
- **Context compaction** — zero-cost deterministic digest or opt-in model
  summarizer, keyed to a token budget.
- **Cache-aware cost accounting** — real per-call usage priced against
  DeepSeek's hit/miss rates.
- **Adaptive reasoning effort** — the `auto` tier picks `max`/`high`/`low` from
  the request (multilingual keyword tables: EN / 中文 / 日本語).
- **Scheduler** — recurring/one-shot saved prompts with consecutive-failure
  auto-disable.
- **A/B worktree comparison** — run one prompt under two configs in isolated git
  worktrees, compare diff/cost/latency, adopt one side.
- **Prompt enhancement, image input, Chinese-first responses**, and a
  Tkinter GUI for Windows.

## Architecture

```text
nanocodex/
├── agent/
│   ├── loop.py            # the turn loop: call model → run tools → repeat
│   ├── prompt.py          # base system prompt (Chinese-first communication)
│   ├── session.py         # running message list + JSONL persistence
│   ├── session_index.py   # browsable history index + per-session snapshots
│   ├── compaction.py      # keep the prompt within a token budget
│   ├── pricing.py         # cache-aware USD cost from real usage
│   ├── auto_reasoning.py  # pick reasoning effort for the `auto` tier
│   ├── enhance_prompt.py  # ✨ rewrite raw input into a clearer prompt
│   ├── memory_store.py    # ~/.nanocodex/memory.md durable notes
│   ├── agents_md.py       # layered AGENTS.md project instructions
│   ├── images.py          # OpenAI multimodal image blocks
│   ├── skills_store.py    # user + built-in skills discovery
│   ├── schedule.py        # scheduled-task store (once / interval)
│   ├── schedule_runner.py # fires due tasks, tracks failures
│   └── ab_compare.py      # A/B worktree comparison (pure core)
├── provider/
│   ├── base.py            # Provider / ToolCall / ModelResponse contracts
│   └── deepseek.py        # OpenAI-compatible chat-completions + streaming
├── tools/                 # shell, code_exec, apply_patch, update_plan, read_file,
│                          # web_search, schedule, skills, remember,
│                          # mcp, mcp_store, marketplace, patch
├── sandbox/
│   ├── policy.py          # what's writable / is network allowed
│   ├── approval.py        # ASK / AUTO_APPROVE / AUTO_DENY state machine
│   └── executor.py        # policy-level enforcement at the tool boundary
├── builtin_skills/        # code-review, debug, write-tests
├── cli.py                 # CLI entry (typer)
├── gui.py                 # Tkinter GUI
└── config.py              # layered config resolution
```

## Tools

The model sees these tools each turn (order matters):

| Tool | Purpose |
| --- | --- |
| `shell` | Run a shell command, gated by the sandbox/approval policy. |
| `code_exec` | Run a small Python/Node snippet through the same sandbox/approval policy; not model-native hosted execution. |
| `apply_patch` | Apply a Codex-style patch to create/edit/delete files. |
| `update_plan` | Maintain a visible step plan for multi-step tasks. |
| `read_file` | Read a file (or a line range) from the workspace. |
| `web_search` | DuckDuckGo search, gated by the network policy. |
| `manage_schedule` | Create / list / cancel scheduled tasks in-chat. |
| `manage_skills` | Create / list / read / delete user skills in-chat. |
| `remember` | Append a durable note to user memory. |
| `mcp__<server>__<tool>` | Any tool exposed by a connected MCP server. |

## Install

```powershell
cd path\to\nanocodex
python -m pip install -e ".[dev]"
```

Requires Python ≥ 3.11.

## Quick Start

Rust CLI, current release line:

```powershell
cd rust
cargo run -p ncx-cli -- "summarize this repository"
cargo run -p ncx-cli
cargo run -p ncx-cli -- --resume
cargo run -p ncx-cli -- --history
cargo run -p ncx-cli -- --memory-merge
```

Inside the Rust REPL, `/config` shows the resolved config file path, current
model/sandbox/approval values, and writable keys. `/config key=value` persists a
setting to `~/.nanocodex/config.toml`; restart the REPL for provider, model,
sandbox, or budget changes to affect the active session. `/usage` (or `/cost`)
shows raw token usage for the last turn and current REPL session. `/budget`
shows task-budget limits and last-turn/session use. `/context` shows the active
context-edit policy, session size, last-turn telemetry, and a preview of the
next provider send; `/context payload [N]` lists recent redacted provider
payload snapshots from `.nanocodex/context-payloads/`.

Python CLI, original line:

```powershell
# one-shot task
nanocodex "add a --json flag to the CLI"

# interactive, in the current directory
nanocodex --cd .

# with MCP servers enabled
nanocodex --mcp

# the GUI
nanocodex-gui --cd .
```

On Windows you can also double-click `nanocodex-gui.cmd` after installation, or
generate a Start-menu shortcut with `scripts/make-shortcut.ps1`.

## Configuration

Settings resolve in priority order:

```text
CLI flags > environment > ~/.nanocodex/config.toml > ~/.deepseek/config.toml > ~/.codex/config.toml > defaults
```

The real API key should stay outside the repository:

```powershell
$env:DEEPSEEK_API_KEY = "sk-..."
$env:NANOCODEX_API_KEY = "sk-..."
```

Or create `~/.nanocodex/config.toml`:

```toml
api_key = "sk-..."
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"

sandbox_mode = "workspace-write"   # read-only | workspace-write | danger-full-access
approval_policy = "on-request"     # untrusted | on-failure | on-request | never
reasoning_effort = "auto"          # auto | low | high | max | off

# Optional
# context_token_budget = 512000
# context_window = 1048576
# max_iterations = 60
# max_tool_calls = 120
# context_edit_enabled = true
# context_edit_max_chars = 0                 # 0/omitted = derive from token budget
# context_edit_keep_recent_messages = 30
# context_edit_max_tool_result_chars = 0     # 0/omitted = derive from max chars
# context_edit_max_history_chars = 0         # 0/omitted = derive from max chars
# context_edit_max_tool_result_total_chars = 0
# available_models = ["deepseek-chat", "deepseek-reasoner", "deepseek-v4-pro"]

# [[hooks]]
# event = "pre_tool"          # pre_tool | post_tool | user_prompt | stop
# matcher = "shell|apply_patch"
# command = "echo checking %NCX_HOOK_TOOL%"
# timeout_s = 10
```

Runtime control-plane settings can also be set with environment variables:
`NANOCODEX_MAX_ITERATIONS`, `NANOCODEX_MAX_TOOL_CALLS`,
`NANOCODEX_CONTEXT_EDIT_ENABLED`, `NANOCODEX_CONTEXT_EDIT_MAX_CHARS`,
`NANOCODEX_CONTEXT_EDIT_KEEP_RECENT`,
`NANOCODEX_CONTEXT_EDIT_TOOL_RESULT_CHARS`,
`NANOCODEX_CONTEXT_EDIT_MAX_HISTORY_CHARS`, and
`NANOCODEX_CONTEXT_EDIT_TOOL_RESULT_TOTAL_CHARS`. The Rust CLI also accepts
`--max-iterations`, `--max-tool-calls`, `--context-edit-max-chars`,
`--context-edit-keep-recent`, `--context-edit-tool-result-chars`,
`--context-edit-history-chars`, `--context-edit-tool-result-total-chars`, and
`--disable-context-edit`.

A full example lives in `config.example.toml`.

Hooks receive `NCX_HOOK_EVENT`, `NCX_HOOK_TOOL`, `NCX_HOOK_ARGS`,
`NCX_HOOK_RESULT`, and `NCX_HOOK_WORKSPACE` in their environment. Use
`pre_tool` for deterministic guards such as blocking risky shell commands,
`post_tool` for audit and formatting, `user_prompt` to block or annotate a
prompt before the model sees it, and `stop` for end-of-turn quality gates or
notifications. Claude-style event names such as `UserPromptSubmit`, `Stop`,
`PreToolUse`, and `PostToolUse` are accepted and normalized. Hooks run as local
subprocesses, so configure only commands you trust.

## Custom Slash Commands

Rust REPL can turn Markdown prompt templates into slash commands. Put project
commands in `.nanocodex/commands/<name>.md`; for Claude Code compatibility,
`.claude/commands/<name>.md` is also read. User commands live in
`~/.nanocodex/commands/<name>.md`, with `~/.claude/commands/<name>.md` as a
compatibility fallback.

Built-in REPL slash commands also expose platform status surfaces: `/usage`
for raw token and context-edit telemetry, `/budget` for task-budget use,
`/context` for the active context-edit policy, next-send preview, and
provider payload snapshots, `/tools` for the tool catalog and visible schema
view, `/memory` for project memory status and recall preview, `/skills` for
the discovered skill catalog, `/history` for saved sessions, and `/mcp` for
enabled MCP servers plus currently registered MCP tools.

The Tauri GUI exposes the same project/user command catalog from the `/` header
button. You can run a command from the panel with arguments, or type the custom
slash command directly in the chat box; the GUI expands it with the same core
template engine the CLI uses.

```markdown
---
description: Review one file
---
Review `$ARGUMENTS[0]` for bugs, regressions, and missing tests.
```

In the REPL:

```text
/review rust/crates/ncx-core/src/session.rs
/project:review rust/crates/ncx-core/src/session.rs
/user:review rust/crates/ncx-core/src/session.rs
```

`/name` resolves project commands before user commands. Templates support
`$ARGUMENTS` for the raw argument string plus `$0`..`$9` and
`$ARGUMENTS[0]`..`$ARGUMENTS[9]` for simple positional arguments. If a command
template has no placeholders, the raw arguments are appended under an
`Arguments:` block. These commands expand to a normal user prompt; they do not
run local shell or `code_exec` snippets by themselves.

## Local Model / OpenAI-Compatible Endpoint

nanocodex talks plain `/v1/chat/completions`, so any OpenAI-compatible server
works — vLLM, llama-server, LM Studio, Ollama's OpenAI shim, etc. Point
`base_url` at the server's `/v1` root. Most local servers ignore the API key,
but a non-empty placeholder is still required because the OpenAI SDK expects
one.

```toml
api_key = "local-dev-key"
base_url = "http://127.0.0.1:8005/v1"
model = "Qwen3.6-27B-Q4_K_M"
```

Quick connectivity check:

```powershell
curl http://127.0.0.1:8005/v1/models
```

Streaming has a bounded "response-header" timeout (default 45s, override with
`NANOCODEX_STREAM_OPEN_TIMEOUT_S`) so a stalled local server fails fast with a
clear hint instead of hanging the UI.

## Sandbox & Approval

Two orthogonal axes gate every action, mirroring Codex:

**Sandbox mode** — what's physically allowed:

| Mode | Reads | Writes | Network |
| --- | --- | --- | --- |
| `read-only` | anywhere | none | off |
| `workspace-write` | anywhere | workspace + writable roots + temp | off unless enabled |
| `danger-full-access` | anywhere | anywhere | on |

**Approval policy** — what to do when an action exceeds the sandbox:
`untrusted`, `on-failure`, `on-request`, `never`. The approval engine resolves
each escalation to `ASK` / `AUTO_APPROVE` / `AUTO_DENY`.

On Windows enforcement is **policy-level**: path checks and writable-root gating
happen at the tool boundary. It is not kernel isolation.

## MCP

MCP servers are opt-in and run **outside** the sandbox (they launch external
subprocesses). The low-level server file remains `~/.nanocodex/mcp.toml`:

```toml
[mcp_servers.fetch]
command = "uvx"
args = ["mcp-server-fetch"]

[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "D:\\projects"]
```

Then start with MCP enabled:

```powershell
ncx --mcp
```

Each server's tools surface to the model as `mcp__<server>__<tool>`. Set
`enabled = false` on a server table to keep it installed but disconnected.

For a more auditable connector install layer, `~/.nanocodex/connectors.toml`
also supports `[connectors.<name>]` specs with `transport`, `source`, `trusted`,
`permission`, `allowed_tools`, `auth`, `headers`, `headers_helper`, and
`[connectors.<name>.oauth]`. Stdio connectors are converted into MCP servers
when `ncx --mcp` starts, and `allowed_tools` plus `permission` (`ask`,
`trusted`, `read-only`, `deny`) are enforced while registering tools.
`allowed_tools` accepts raw MCP tool names or fully qualified
`mcp__<server>__<tool>` names. Remote `sse`/`http` specs are parsed and shown
for audit with redacted auth metadata plus an explicit `launch=audit-only`
status and `launch_gap=remote_transport_not_implemented`; they are not launched
until first-class OAuth login and remote transports land. See
`mcp.example.toml` and `connectors.example.toml`.

Inside the Rust REPL, `/mcp` shows enabled server entries, connector install
specs, connector launch status, permission/trust/source/auth metadata, and MCP
tools registered in the active session. In the Tauri GUI, the Tools panel also
shows each configured MCP server's connected/error state, registered tool
count, startup elapsed time, command, and last error.

## Skills

Skills are reusable instruction documents, one folder each:

```text
~/.nanocodex/skills/<skill-name>/SKILL.md
```

Only each skill's **name and description** are injected into the system prompt;
the full body is read on demand, so a large library doesn't eat the context
window. The model can also create/read/delete user skills in-chat via the
`manage_skills` tool.

Minimal skill:

```markdown
---
name: code-review
description: Review code changes and focus on bugs, regressions, and missing tests.
---

# Code Review

Look for behavior regressions first, then missing tests, then maintainability.
```

The package ships three **read-only built-in skills** under
`nanocodex/builtin_skills/`:

- **code-review** — two-pass review (correctness, then cleanup), ranked by impact.
- **debug** — reproduce → localize → fix → verify; resist patching the first
  plausible line.
- **write-tests** — test observable behavior, one behavior per test, prefer pure
  functions over mocks.

A user skill of the same name shadows the built-in.

## Memory & AGENTS.md

Three complementary layers of persistent context:

- **Project memory** (`.ncx/memory/LEARNINGS.md`) — verified project notes
  retrieved as query-scoped recall leads for each turn. Written by the
  `remember` tool or the Tauri Memory panel; maintained with
  `ncx --memory-merge`, heuristic Merge, or LLM merge. Rust `/memory [query]`
  shows the file path, vector index path, entry count, recent notes, tags, and
  a recall preview. Verified notes also build `.ncx/memory/INDEX.json`, a local
  deterministic vector sidecar; `/memory index` or the Tauri Memory panel can
  rebuild it after manual edits.
- **Memory proposals** (`.ncx/memory/PROPOSALS.md`) — candidate learnings that
  are useful but not trusted yet. `remember` can set `propose=true`, the CLI can
  run `/memory propose <note>`, `/memory edit <id> <note>`, `/memory accept
  <id>`, `/memory reject <id>`, `/memory accept-all`, `/memory reject-all`, or
  `/memory harvest [path]` to extract candidates from handoff/release docs. The
  Tauri Memory panel can harvest documents, edit proposal text/tags, and expose
  single/batch accept/reject controls. Proposals are not recalled until accepted
  into `LEARNINGS.md`. During normal turns, explicit user corrections, tool
  failures, and assistant resolution notes are also harvested into proposals for
  review.
- **User memory** (`~/.nanocodex/memory.md`) — durable personal facts and
  preferences. Written by the `remember` tool, by typing `# something` in the
  legacy Python GUI composer (quick-capture), or by hand. Wrapped in a
  `<user_memory>` block on the Python line.
- **AGENTS.md / CLAUDE.md** — project instructions layered from
  `~/.codex/AGENTS.md` and `~/.claude/CLAUDE.md`, then every `AGENTS.md`,
  `CLAUDE.md`, and `.claude/CLAUDE.md` from the repo root down to the workspace,
  so nested directories refine their parents. Total size is capped so a huge
  file can't blow the context. Rust CLI, orchestrator workers, and the Tauri GUI
  all inject this project-instruction block at session startup; project memory is
  recalled separately at send time for the current prompt.

Memory is "who/what" (preferences, facts); skills are "how to do X";
AGENTS.md / CLAUDE.md are project-scoped guidance.

## Sessions, Resume & History

- Every conversation is appended to a **JSONL session log** (base64 image data
  is redacted from the log to keep it small).
- A **global index** (`~/.nanocodex/sessions.jsonl`) holds one summary line per
  conversation, newest-first, for the GUI's history list.
- A **per-session snapshot** (`~/.nanocodex/snapshots/<id>.json`) freezes the
  full transcript so the detail view replays the real conversation, not a digest.
- The Rust CLI supports `--resume` to reload the workspace
  `.nanocodex/session.jsonl` before starting and `--history` to list recent
  global session summaries. The Tauri backend records the same snapshots after
  each GUI turn.
- The Tauri GUI's `S` panel reads the same global index, opens JSONL logs and
  frozen snapshots, and can resume a snapshot when it belongs to the current
  workspace.
- Rust CLI and Tauri GUI save a workspace file checkpoint before each model
  turn. Use `/checkpoints` to list recent checkpoints, `/checkpoint <label>` to
  create one manually, and `/restore <id>` to restore files; the GUI has the
  same save/list/restore flow in its checkpoint panel. Restore first creates a
  safety checkpoint of the current state.
- The original Python GUI can **fork** a saved snapshot to branch a past
  conversation without mutating the source session.

## Context Compaction

Long conversations are folded to stay within a token budget while preserving the
system message and a recent tail (the tail always starts at a `user` message, so
no tool-call/result pair is split). Two strategies share one interface:

- **deterministic** (default, zero API cost) — the folded middle becomes a
  factual, rule-based digest.
- **summarizer** (opt-in, costs tokens) — a model call turns the middle into prose.

The trigger estimate uses a Chinese-leaning chars/token ratio so zh-heavy chats
don't compact too late.

In the Rust CLI, `/compact` materializes the active context-edit policy into the
live session and rewrites the workspace session log, so future turns and
`--resume` continue from the compacted history. Rust `/context` previews that
same send-time edit without mutating the session, including policy knobs,
message counts, last-turn telemetry, and the next-send compression/drop stats.
Rust `/usage` and the Tauri GUI's `U` panel also surface send-time context
editing telemetry: original chars, edited chars, saved chars, compressed tool
results, dropped messages, summary checkpoints, and context-pack buckets. The
summary checkpoint is inserted as an assistant message before truncation when it
is smaller than the omitted prefix, so `/compact` and later `--resume` keep an
auditable bridge over older history. The summary also includes deterministic
focus anchors from older messages that overlap the latest user request, which
keeps a few task-relevant facts visible after long-history trimming. The
send-time policy derives `context_edit_max_chars`,
`context_edit_max_history_chars`, `context_edit_max_tool_result_chars`, and
`context_edit_max_tool_result_total_chars` from `context_token_budget` and
`context_window` when those caps are omitted or set to `0`, then enforces
`context_edit_max_history_chars` and
`context_edit_max_tool_result_total_chars` so long history and old tool output
cannot crowd out system notes and memory recall. Each provider call also writes
a redacted JSON snapshot under `.nanocodex/context-payloads/`, so
`/context payload [N]` and the GUI Usage panel can inspect exactly which edited
messages and tool schemas were sent.

## Token Usage & Cost

The provider returns real `usage` per call, including DeepSeek's
cache-hit/miss split. In the Rust REPL, `/usage` and `/cost` show the last turn
and session total model calls, tool calls, prompt tokens, completion tokens, and
cache hit/miss tokens. The Tauri GUI's `U` panel shows the same raw last-turn
and session totals from the desktop event stream. The Rust surfaces
intentionally report raw usage only; `pricing.py` turns usage into a USD cost
for the Python line:

- **Cache-aware** — a cache-hit input token is ~120× cheaper than a miss; each is
  billed at its own rate. When the split is absent, the whole prompt is billed at
  the miss rate so cost is never understated.
- **Honest about staleness** — prices are a hardcoded snapshot carrying a source
  and as-of date; an unknown model returns "cost unknown" rather than a wrong
  number.

## Scheduler

Save a prompt to run automatically — once at a future time or on an interval:

```powershell
nanocodex schedule add "run the tests" --at 2026-06-08T09:00:00
nanocodex schedule add "summarize new issues" --every 3600
nanocodex schedule list
nanocodex schedule run        # keep this running for tasks to fire
```

A task that fails repeatedly **auto-disables** after 5 consecutive failures
(success resets the counter; re-enabling clears it), so a broken task can't loop
forever. The agent can also manage tasks in-chat via `manage_schedule`.

## A/B Worktree Comparison

Run the **same prompt under two configurations** and compare the results without
risking your working tree. Each side runs in its own isolated **git worktree**,
so real `shell`/`apply_patch` edits never collide:

1. Pick two configs (model / reasoning effort / sandbox / approval).
2. nanocodex creates two worktrees from clean `HEAD` and runs the prompt in each,
   serially, with auto-approve scoped to the worktree.
3. You get a side-by-side comparison: diff, token cost, latency, iterations,
   stop reason.
4. **Adopt** one side (its diff is applied to the real workspace) or discard
   both; the worktrees are always cleaned up.

Requires a clean git workspace (no uncommitted changes); the entry is disabled
otherwise.

## GUI

The current desktop line is a Tauri v2 + Svelte GUI (`rust/gui`):

- Streaming chat, tool-call display, and approval modals.
- Settings panel for model, sandbox, approval, budgets, context editing, base
  URL, and API key, plus direct open buttons for
  `~/.nanocodex/config.toml`, `mcp.toml`, and `connectors.toml`. On a fresh
  install with no API key, the GUI opens this panel directly so the agent can be
  configured before the first turn.
- Sessions panel for global history, log/snapshot open actions, and
  same-workspace resume.
- Checkpoint panel for manual save/list/restore.
- Custom command panel backed by the same core `.nanocodex/.claude` template
  engine the CLI uses.
- Tools panel for the live runtime catalog, including core/MCP grouping,
  read-only versus effectful tool classification, and MCP server runtime
  health with startup timing and last-error visibility.
- Usage panel for last-turn and session model calls, tool calls, prompt tokens,
  completion tokens, cache hit/miss tokens, estimated cost, context-edit
  telemetry, provider payload snapshots, and task-ledger wall time / approval /
  budget report visibility.
- Memory panel for viewing project notes, adding verified notes, rebuilding the
  local vector index, harvesting handoff/release docs into pending proposals,
  editing proposal text/tags, reviewing proposals with single/batch
  accept/reject, opening `LEARNINGS.md`, heuristic deduplication, and
  LLM-backed memory merge.

The original Tkinter GUI remains in the Python tree as a legacy prototype.
Note: the desktop GUI does not hot-reload — code changes require closing and
reopening it.

## Tests

Rust release line:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\verify-rust.ps1
```

The verification script checks `cargo fmt`, runs the Rust workspace tests for
`x86_64-pc-windows-gnu`, and checks the Tauri backend. If `cargo` is not on
`PATH`, it probes common Windows Rust locations and prints the `rustup` commands
needed to install the target. Use `-SkipTauri` for CLI-only validation, or
`-Cargo C:\path\to\cargo.exe` when Rust is installed outside `PATH`.

Manual equivalent:

```powershell
cd rust
cargo fmt --all --check
cargo test --workspace --target x86_64-pc-windows-gnu
cd gui\src-tauri
cargo check --target x86_64-pc-windows-gnu
```

Python line:

```powershell
python -m pytest -q
```

Both suites are fully offline: mocked providers, injectable I/O, no real API
key or network call required.

GitHub Actions also runs the same Windows Rust verification path plus the
Python offline test suite on pushes, pull requests, and manual dispatches. The
Windows job installs the MinGW linker before testing the
`x86_64-pc-windows-gnu` target. Use that CI result as the release gate when a
local Windows machine is missing the Rust toolchain.

## Release Packaging

Use [docs/release-checklist.md](docs/release-checklist.md) before cutting a
release or merging a release branch.

Recommended Windows release entry point:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-rust-release.ps1
```

The script runs the Rust workspace tests, builds the Windows GNU release binary,
creates `releases\nanocodex-<version>-x86_64-pc-windows-gnu.zip`, builds the
Tauri NSIS installer, then writes `releases\SHA256SUMS.txt` and
`releases\release-manifest.json`. The installer build receives the same version
through a temporary Tauri config overlay, so its NSIS metadata follows the Rust
workspace release version. The script uses the same cargo discovery path as
`scripts\verify-rust.ps1`; pass `-Cargo C:\path\to\cargo.exe` if Rust is
installed outside `PATH`. Use `-SkipTauri` for a CLI-only package or
`-SkipTests` only after the same target has already passed in CI/local release
validation.
For release or benchmark audits, the packaged CLI can also print local
tool-search trace health without entering the REPL:

```powershell
.\releases\<unzipped>\ncx.exe --tools-eval-report
```

Manual Windows GNU CLI release:

```powershell
cd rust
cargo build --release --workspace --target x86_64-pc-windows-gnu
```

Manual Tauri desktop installer:

```powershell
cd rust\gui
npm.cmd ci
npm.cmd run tauri:installer
```

The desktop build now targets the Windows NSIS installer explicitly. The
installer is emitted under
`rust\gui\src-tauri\target\x86_64-pc-windows-gnu\release\bundle\nsis\`.
The GUI Settings dialog also exposes the resolved `~/.nanocodex/config.toml`,
`mcp.toml`, and `connectors.toml` paths with buttons to open each file or the
config directory. Missing MCP/connector files are created with commented
templates; saving Settings reloads the Rust agent in place.

The Tauri crate deliberately keeps `crate-type = ["lib"]`; changing it to
`cdylib` or `staticlib` overflows the Windows GNU linker's export table.

## Security Notes

- **Never commit real API keys.** `.env`, `*.key`, `*.pem`, token files, and
  local handoff files are git-ignored; `config.toml`, `mcp.toml`, and
  `connectors.toml` live in `~/.nanocodex/`, outside the repo.
- The sandbox is **policy-level on Windows** — it gates tool actions and writable
  roots, but is not kernel isolation.
- **MCP tools run outside the sandbox** as external subprocesses. Only enable
  servers you trust; the legacy marketplace validates names but does not vet
  behavior.
- **Hooks run local commands** around tool execution. Treat hook configuration
  like code and review it before enabling it in a project.
- External content (file contents, command output, web/MCP results) is treated
  as untrusted data, not as instructions.

## License

MIT — see [LICENSE](LICENSE).
