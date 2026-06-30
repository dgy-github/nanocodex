# nanocodex

[English](README.md) | 简体中文

## 能力说明页

[打开在线能力说明页](https://dgy-github.github.io/nanocodex/nanocodex.html) · [查看仓库内 HTML](nanocodex.html)

[设计说明 PDF](docs/ai-coding-agent-design-brief.pdf) · [设计说明 HTML](docs/ai-coding-agent-design-brief.html)

📖 **[设计理念手册](docs/design-philosophy.zh-CN.md)** —— 系统讲解分层编排、递归子任务分解、无工具推理节点、渐进披露、视觉分流、记忆自进化与基准方法论“为什么这样设计”。

[![nanocodex GUI 预览：会话、工具调用、MCP、Skills、成本统计与测试状态](assets/nanocodex-ui-preview.svg)](https://dgy-github.github.io/nanocodex/nanocodex.html)

`nanocodex` 是一个小而完整的 Codex 风格编码 agent。一个 chat-completions 模型
提出工具调用，agent 在沙箱内执行文件/shell 工具，记录会话，并循环直到任务完成。
它可以对接 DeepSeek 托管 API，也可以对接任意 OpenAI 兼容的本地模型，并自带
MCP 集成、skills 系统、沙箱/审批状态机、上下文压缩、token 成本统计、Windows
GUI、定时器，以及 git worktree 的 A/B 对比。

项目分为两个清晰阶段。重点不是“把同一套功能换一种语言写”，而是架构边界和发布性能的
升级。

## 项目阶段

### 第一阶段：Python 基础版

`nanocodex/` 下的 Python 实现是最早的完整功能线，目标是快速验证产品形态：先把
agent 循环、工具体验、审批模型和桌面流程跑通，再决定哪些部分需要更强的工程边界。

**架构层面**

- 以一个紧凑的 async agent loop 为中心：调用模型 -> 执行工具 -> 更新 session ->
  进入下一轮模型调用。
- 工具、provider、sandbox、MCP、skills、memory、scheduler、compaction、GUI 都是
  可独立扩展的 Python 模块，适合在产品形态还不稳定时快速迭代。
- 运行时契约偏动态，因此新增 MCP marketplace、prompt 增强、图片输入、session
  resume/fork、A/B worktree 对比等功能成本很低。
- Windows GUI 使用 Tkinter，第一版桌面体验依赖少、调试直接。

**性能与交付层面**

- 优势是迭代速度：没有编译步骤，实验快，mock provider 的离线测试也容易铺开。
- 420 个离线测试覆盖 Python 功能线，不需要真实 API key，也不依赖网络。
- 交付仍依赖 Python 解释器、包环境和导入启动成本；对普通 Windows 用户来说，分发
  体验不如原生二进制直接。
- 动态边界适合探索期，但当沙箱、工具执行、记忆、MCP、并行 agent flow 变多时，跨模块
  状态和行为边界会越来越难推理。

### 第二阶段：Rust 重构版

`rust/` 下的 Rust 实现是当前 release 线。它不删除 Python 树，而是把已经验证过的
能力重建成更清晰的 crate 边界，并配套 Tauri 桌面壳，让项目进入可分发工具阶段。

**架构层面**

- 工作区按责任拆分为 `ncx-sandbox`、`ncx-config`、`ncx-provider`、`ncx-tools`、
  `ncx-core`、`ncx-cli`。
- provider 响应、工具调用、沙箱决策、session 消息、记忆条目、编排结果都有显式类型，
  跨 crate 边界时不再靠松散 dict/object 传递。
- 工具执行集中到 `ToolContext` 和 `ToolRegistry` 后面，沙箱策略、审批策略、超时、
  搜索和记忆都挂在真正执行动作的边界上。
- 编排层加入任务分类、main/fast 模型路由、隔离 worker 工作区、verifier 选择，以及
  把胜出 worker promote 回真实工作区。
- 项目记忆是本地可审计的：已验证笔记保存在 `.ncx/memory/LEARNINGS.md`，候选经验先进入
  `.ncx/memory/PROPOSALS.md`，经 CLI/Tauri review 接受后才会进入 recall；启动时只做便宜的
  启发式去重，显式运行 `ncx --memory-merge`，或在 Tauri Memory 面板里操作，才会执行显式的
  启发式/LLM 记忆合并。
- 桌面线切到 Tauri v2 + Svelte 5，把原生后端和 UI 表层分离，为比 Python GUI 更小的
  release bundle 做准备。

**性能与交付层面**

- CLI 构建成原生 `ncx.exe`，用户不需要准备 Python、虚拟环境或 editable install。
- release 构建启用 strip、LTO、体积优化，并面向 Windows GNU target；当前 CLI zip
  小于 2 MB，且已经包含 README、LICENSE 和配置样例。
- 启动路径避开 Python 解释器和 import 开销，适合短的一次性命令，也适合交互 REPL。
- 显式所有权让并行 worker 隔离、结果选择和 promote 更容易推理，不容易出现共享可变状态
  泄漏。
- 源码计 319 个 Rust 离线测试函数覆盖当前 crate 边界，包括记忆合并、provider 请求/响应解析、
  沙箱策略、工具、编排器和 context-editing 回归。

**平台控制面补齐**

- **Task budget：** 每次模型调用都会收到当前运行预算，包括模型调用次数、工具调用次数
  和上下文限制；模型调用或工具调用超预算时，loop 会干净停止，并补齐未执行工具调用的
  tool result，保证消息历史仍然有效。Rust REPL 提供 `/budget` 查看每任务限额、session
  用量和上一轮剩余额度；每个完成任务会追加 `.nanocodex/task-ledger.jsonl`，`/budget report`
  和 `--budget-report` 可查看最近任务的 wall time、审批数、停止原因、token 汇总、平均耗时、
  预算耗尽率，以及模型/工具预算利用率。编排器会在
  reasoning 节点和并行 worker/subagent 之间共享同一份父任务预算，避免每个 worker 都拿到一份
  独立满额预算。
- **Context editing：** 本地完整 session 不会被删；发给 provider 的是发送时编辑视图，
  会压缩旧 tool result，并在超过上下文预算时丢弃更早的前缀。丢弃旧前缀前，
  Rust 会先生成确定性的 assistant 摘要 checkpoint，方便 `/compact`、`--resume`、
  payload snapshot 和 Usage telemetry 审计被省略的历史。摘要里还会带确定性的
  focus anchors：从旧历史中挑出与最新用户请求词面重合的消息片段；如果用户执行
  `/compact <focus>`，这个显式 focus 也会参与 anchor 排名。Rust REPL 提供
  `/context` 查看当前策略、session 大小、上一轮 telemetry、下一次发送预览，并可通过
  `/context payload [N]` 查看最近 provider payload 快照。telemetry 会把发送 payload
  拆成 context-pack 桶：system prompt、运行注记、memory recall、历史和 tool result 字符数；
  history 与 tool-result 总桶上限也会实际参与发送时治理。这些上限留空或设为 `0` 时，
  Rust loader 会按 `context_token_budget` 和 `context_window` 自动推导，1M context 模型配置
  不再被旧的 120k 字符发送上限过早裁剪。回归测试已覆盖大工具输出、长历史、
  runtime note / memory 竞争预算，以及压缩后的 tool_call/tool_result 配对合法性。
- **Tool search：** 工具注册时会进入 catalog。小工具集仍全量暴露；工具变多时只暴露核心
  工具和 `tool_search`，搜索命中的工具会在下一轮 schema 里出现。排序会识别 MCP
  工具命名空间（`mcp__server__tool`），并用 29 条覆盖 core tools、MCP connectors
  和 release packaging 的 tool-selection gold cases 做回归覆盖；MCP tool description
  也会补上确定性的 category/capability hints，弥补稀疏 connector metadata。Rust REPL 提供
  `/tools` 检查工具 catalog、当前可见 schema 视图和 active search hints；
  Tauri Tools 面板读取同一份 live catalog，并展示 MCP server 连接状态、注册工具数、启动耗时和最近错误。
  每轮完成后会把 visible schemas / called tools 写入 task ledger，`/tools eval [N]`
  和 `--tools-eval-report` 可输出 schema recall、missed calls、MCP recall 和最近 miss 样本。
- **Semantic memory：** 每轮都会按当前 prompt 做 query-scoped 记忆召回，并以发送时
  system note 注入；排序使用关键词、标签、短语、Jaccard 相似度、时间新近度，以及一小组
  agent/runtime 领域同义词。Rust REPL 提供 `/memory` 查看记忆文件、条目数、最近记录、
  tag 摘要、query-scoped recall 预览，以及当前 embedding backend/gap。`~/.nanocodex/config.toml`
  现在可声明外部 embedding provider/model/base URL/API-key env var 作为审计入口；真正召回仍使用
  本地确定性 vector sidecar，直到 runtime 外部 embedding 调用落地。

### 第二阶段为什么改用 Rust

切到 Rust 是因为第二阶段的目标是产品化，而不只是继续加功能：

- **架构硬化：** 沙箱、审批引擎、provider adapter、工具注册表、记忆存储和编排器都有
  显式类型契约，不再主要依赖 Python 的动态对象边界。
- **动作边界更清楚：** 文件、shell/`code_exec`、搜索、记忆操作都经过同一个 tool context，审批和
  沙箱检查贴近真实执行点。
- **并行编排更稳：** 隔离 worker 副本、verifier 选择、结果 promote 回真实工作区这些
  流程，在显式所有权和类型系统下更容易证明不会互相踩写。
- **运行时控制面：** task budget、context editing、tool search、semantic memory 和沙箱化
  `code_exec` 放在 Rust runtime 边界里；memory 召回按每轮 prompt 发送时注入，而不是只靠启动 prompt 约定。
- **Checkpoint / restore：** Rust CLI 和 Tauri GUI 都会在模型轮次前创建文件
  checkpoint。CLI 提供 `/checkpoint`、`/checkpoints`、`/restore <id>`；GUI 提供
  checkpoint 面板用于手动保存、列表查看和恢复。
- **原生发布性能：** 小体积 `ncx.exe` 不需要解释器启动和环境配置，一次性 CLI 任务响应
  更直接，Windows 用户拿到包即可运行。
- **桌面打包路径：** Tauri 提供原生 shell + web UI 前端，比继续扩大 Tkinter 原型更适合
  长期发布。

## 与 Claude Code / Fable 的能力校准

截至 2026-06-29，公开 Claude Code 文档描述的是比本地开源 harness 更大的 Anthropic
平台面：终端、IDE、桌面和浏览器入口；MCP、hooks、skills、auto memory、agent teams、
cloud/scheduled sessions；以及 Fable 5、Opus 4.8、Sonnet 4.6 的 1M context 变体。
所以 `nanocodex` 应该被理解成一个紧凑的 Rust agent runtime，而不是对 Anthropic
完整平台的等价声明。
参考面来自 Claude Code 官方
[overview](https://code.claude.com/docs/en/overview)、
[context window](https://code.claude.com/docs/en/context-window)、
[memory](https://code.claude.com/docs/en/memory)、
[MCP](https://code.claude.com/docs/en/mcp) 和
[hooks](https://code.claude.com/docs/en/hooks)，以及 Anthropic 官方
[models overview](https://docs.anthropic.com/en/docs/about-claude/models/overview)。
更细的缺口拆解和近期 backlog 见
[`docs/claude-fable-gap-roadmap.zh-CN.md`](docs/claude-fable-gap-roadmap.zh-CN.md)。

当前粗略估算如下：

| 范围 | nanocodex Rust 估算 | 主要原因 |
| --- | ---: | --- |
| 本地 CLI harness 与工具循环，假设接入同等级前沿模型 | 55-65% | Rust typed loop、sandbox、审批、工具注册表、memory recall、context editing、沙箱化 `code_exec`、checkpoints、MCP、skills 和 Tauri GUI 已覆盖较多本地 coding-agent harness。 |
| 默认 DeepSeek 兼容模型线，对比 Claude Code + Fable 级模型的端到端表现 | 35-45% | harness 已能支撑不少工作流，但硬任务主要受模型推理、延迟、工具纪律和 Anthropic 原生集成影响。 |
| Windows 本地发布和分发体验 | 60-70% | 原生 CLI zip 和 Tauri NSIS installer 已具备，但生态规模、跨入口体验和产品抛光还不是 Anthropic 平台级。 |

本阶段要求补齐的四个控制面已经落到本地 runtime，但仍是本地实现：

| 能力 | 当前覆盖 | 剩余差距 |
| --- | ---: | --- |
| Task budget | 82-90% | 模型/工具预算已执行，并对模型可见；CLI 和 GUI 会写入/读取带趋势/利用率分析的 task ledger；orchestrator worker 已共享父任务预算，而不是每个 subagent 拿一份独立满额预算。还缺云端任务额度、远端队列治理和托管执行分析面。 |
| Context editing | 72-80% | 发送时编辑会压缩旧 tool result，按 `context_token_budget`/`context_window` 自动推导适配 1M context 的字符与分桶上限，并在丢弃旧前缀前物化带 focus anchors 的确定性摘要 checkpoint；`/compact <focus>` 现在可显式写入 focus instruction 并用于旧历史 anchor 排名；provider payload snapshot、context-pack 分桶 telemetry，以及大工具输出/长历史回归覆盖已让真实模型输入更可审计。还缺 Anthropic 级长上下文质量、模型引导的自动 focus compaction、平台自动 compact 和更完整的质量评估套件。 |
| Tool search / connectors | 70-80% | 工具 catalog、namespace-aware `tool_search`、GUI MCP runtime 状态、29 条跨类别 gold-case 排名测试、确定性 MCP category/capability hints、visible-vs-called task-ledger trace、`/tools eval` / `--tools-eval-report` schema-recall 报告，以及可审计的 `connectors.toml` install/auth spec 已降低 schema 和 connector 歧义；还缺完整 OAuth login UX、远程 transport 启动、托管 registry、更大的真实 trace 样本、更丰富的类别体系和大规模动态工具排序。 |
| Semantic memory | 74-82% | query-scoped lexical-semantic recall、本地 vector sidecar recall、`remember`、LLM merge、CLI/Tauri proposal review、提议编辑、批量接受/拒绝、handoff/release 文档提炼、运行时纠正/失败 proposal 提炼，以及外部 embedding 配置/状态审计已有；还缺真正执行外部 embedding 调用和平台级长期 memory。 |

最大的剩余差距不是普通 Rust 代码量，而是平台能力：Anthropic 原生工具/connector
生态、托管 task budget 与云端执行、模型集成的 context 管理、一方 memory 行为，以及
模型侧代码执行/分析能力。
新增的本地 `code_exec` 能覆盖快速计算、解析和小片段分析，但它仍通过本地 shell 沙箱执行，
不是模型/平台内建的托管代码执行环境。

## 目录

- [项目阶段](#项目阶段)
- [与 Claude Code / Fable 的能力校准](#与-claude-code--fable-的能力校准)
- [Claude/Fable 差距路线图](docs/claude-fable-gap-roadmap.zh-CN.md)
- [亮点](#亮点)
- [架构](#架构)
- [工具](#工具)
- [安装](#安装)
- [快速开始](#快速开始)
- [配置](#配置)
- [自定义 Slash Commands](#自定义-slash-commands)
- [本地模型 / OpenAI 兼容接口](#本地模型--openai-兼容接口)
- [沙箱与审批](#沙箱与审批)
- [MCP](#mcp)
- [Skills](#skills)
- [记忆与 AGENTS.md](#记忆与-agentsmd)
- [会话、恢复与历史](#会话恢复与历史)
- [上下文压缩](#上下文压缩)
- [Token 用量与成本](#token-用量与成本)
- [定时器](#定时器)
- [A/B worktree 对比](#ab-worktree-对比)
- [GUI](#gui)
- [测试](#测试)
- [Release 打包](#release-打包)
- [安全说明](#安全说明)

## 亮点

- **Codex 风格 agent 循环** —— 流式 token 输出、多轮工具调用、可取消、按轮统计
  用量。
- **DeepSeek + 任意 OpenAI 兼容后端** —— 把 `base_url` 指向托管 API 或本地服务
  （vLLM、llama-server、LM Studio……）。
- **沙箱与审批状态机** —— 三种沙箱模式、四种审批策略，拦截每一次文件/shell/网络
  动作。
- **MCP 集成** —— Rust CLI 通过 `--mcp` 显式加载 `mcp.toml` 服务，工具以
  `mcp__<server>__<tool>` 形式暴露；legacy Python GUI 仍保留内置 / 远程 MCP 市场安装器。
- **Skills 系统** —— 用户 skill 加三个内置编码 skill；只注入名称 + 描述，正文
  按需加载。
- **自定义 slash commands** —— 项目/用户级 Markdown prompt 模板放在
  `.nanocodex/commands`，并兼容 `.claude/commands`。
- **持久记忆 + AGENTS.md / CLAUDE.md** —— 每轮注入的持久笔记和分层项目指令。
- **可浏览的会话历史** —— JSONL 日志、完整对话快照、恢复（resume）和分叉（fork）。
- **上下文压缩** —— 零成本的确定性摘要，或可选的模型摘要，按 token 预算触发。
- **缓存感知的成本统计** —— 用真实的按调用用量，按 DeepSeek 的命中/未命中费率
  计价。
- **自适应推理强度** —— `auto` 档根据请求选 `max`/`high`/`low`（多语言关键词表：
  英文 / 中文 / 日本語）。
- **定时器** —— 一次性/周期性保存的 prompt，连续失败自动禁用。
- **A/B worktree 对比** —— 同一 prompt 在两个隔离的 git worktree 里各跑一套配置，
  对比 diff/成本/延迟，采纳其中一侧。
- **prompt 增强、图片输入、中文优先回复**，以及一个面向 Windows 的 Tkinter GUI。

## 架构

```text
nanocodex/
├── agent/
│   ├── loop.py            # 轮次循环：调模型 → 跑工具 → 重复
│   ├── prompt.py          # 基础系统提示（中文优先沟通）
│   ├── session.py         # 运行中的消息列表 + JSONL 持久化
│   ├── session_index.py   # 可浏览的历史索引 + 单会话快照
│   ├── compaction.py      # 把 prompt 压在 token 预算内
│   ├── pricing.py         # 从真实用量算缓存感知的美元成本
│   ├── auto_reasoning.py  # 为 `auto` 档选推理强度
│   ├── enhance_prompt.py  # ✨ 把原始输入改写成更清晰的 prompt
│   ├── memory_store.py    # ~/.nanocodex/memory.md 持久笔记
│   ├── agents_md.py       # 分层的 AGENTS.md 项目指令
│   ├── images.py          # OpenAI 多模态图片块
│   ├── skills_store.py    # 用户 + 内置 skills 发现
│   ├── schedule.py        # 定时任务存储（一次性 / 周期）
│   ├── schedule_runner.py # 触发到期任务，跟踪失败
│   └── ab_compare.py      # A/B worktree 对比（纯逻辑核心）
├── provider/
│   ├── base.py            # Provider / ToolCall / ModelResponse 契约
│   └── deepseek.py        # OpenAI 兼容的 chat-completions + 流式
├── tools/                 # shell、code_exec、apply_patch、update_plan、read_file、
│                          # web_search、schedule、skills、remember、
│                          # mcp、mcp_store、marketplace、patch
├── sandbox/
│   ├── policy.py          # 什么可写 / 是否允许网络
│   ├── approval.py        # ASK / AUTO_APPROVE / AUTO_DENY 状态机
│   └── executor.py        # 工具边界上的策略级强制
├── builtin_skills/        # code-review、debug、write-tests
├── cli.py                 # CLI 入口（typer）
├── gui.py                 # Tkinter GUI
└── config.py              # 分层配置解析
```

## 工具

模型每轮看到这些工具（顺序有意义）：

| 工具 | 用途 |
| --- | --- |
| `shell` | 执行 shell 命令，受沙箱/审批策略约束。 |
| `code_exec` | 通过同一套沙箱/审批策略执行小段 Python/Node 代码；不等价于模型内建代码执行。 |
| `apply_patch` | 应用 Codex 风格补丁，创建/编辑/删除文件。 |
| `update_plan` | 为多步任务维护一个可见的步骤计划。 |
| `read_file` | 读取工作区里的文件（或某个行区间）。 |
| `web_search` | DuckDuckGo 搜索，受网络策略约束。 |
| `manage_schedule` | 在对话里创建 / 列出 / 取消定时任务。 |
| `manage_skills` | 在对话里创建 / 列出 / 读取 / 删除用户 skill。 |
| `remember` | 往用户记忆里追加一条持久笔记。 |
| `mcp__<server>__<tool>` | 已连接 MCP 服务暴露的任意工具。 |

## 安装

```powershell
cd path\to\nanocodex
python -m pip install -e ".[dev]"
```

需要 Python ≥ 3.11。

## 快速开始

Rust CLI，当前 release 线：

```powershell
cd rust
cargo run -p ncx-cli -- "summarize this repository"
cargo run -p ncx-cli
cargo run -p ncx-cli -- --resume
cargo run -p ncx-cli -- --history
cargo run -p ncx-cli -- --memory-merge
```

Rust REPL 里可以用 `/config` 查看解析后的配置文件路径、当前 model/sandbox/approval
值和可写 key。`/config key=value` 会把设置持久写入 `~/.nanocodex/config.toml`；
provider、model、sandbox 或预算类变更需要重启 REPL 后影响当前会话。`/usage`（或
`/cost`）会显示上一轮和当前 REPL session 的原始 token 用量；`/budget` 会显示任务预算
限额和上一轮/session 用量；`/context` 会显示当前 context-edit 策略、session 大小、上一轮
telemetry 和下一次 provider 发送预览；`/context payload [N]` 会列出
`.nanocodex/context-payloads/` 下最近的脱敏 provider payload 快照。

Python CLI，原始功能线：

```powershell
# 一次性任务
nanocodex "add a --json flag to the CLI"

# 在当前目录交互
nanocodex --cd .

# 启用 MCP 服务
nanocodex --mcp

# 启动 GUI
nanocodex-gui --cd .
```

在 Windows 上，安装后也可以直接双击 `nanocodex-gui.cmd`，或用
`scripts/make-shortcut.ps1` 生成开始菜单快捷方式。

## 配置

配置项按优先级解析：

```text
CLI 参数 > 环境变量 > ~/.nanocodex/config.toml > ~/.deepseek/config.toml > ~/.codex/config.toml > 默认值
```

真实 API key 应当留在仓库之外：

```powershell
$env:DEEPSEEK_API_KEY = "sk-..."
$env:NANOCODEX_API_KEY = "sk-..."
```

或创建 `~/.nanocodex/config.toml`：

```toml
api_key = "sk-..."
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"

sandbox_mode = "workspace-write"   # read-only | workspace-write | danger-full-access
approval_policy = "on-request"     # untrusted | on-failure | on-request | never
reasoning_effort = "auto"          # auto | low | high | max | off

# 可选
# context_token_budget = 512000
# context_window = 1048576
# max_iterations = 60
# max_tool_calls = 120
# context_edit_enabled = true
# context_edit_max_chars = 0                 # 0/留空 = 按 token budget 推导
# context_edit_keep_recent_messages = 30
# context_edit_max_tool_result_chars = 0     # 0/留空 = 按 max chars 推导
# context_edit_max_history_chars = 0         # 0/留空 = 按 max chars 推导
# context_edit_max_tool_result_total_chars = 0
# memory_embedding_provider = "local"        # local | openai-compatible | ollama | custom
# memory_embedding_model = ""
# memory_embedding_base_url = ""
# memory_embedding_api_key_env = ""          # 环境变量名；这里不存密钥明文
# available_models = ["deepseek-chat", "deepseek-reasoner", "deepseek-v4-pro"]

# [[hooks]]
# event = "pre_tool"          # pre_tool | post_tool | user_prompt | stop
# matcher = "shell|apply_patch"
# command = "echo checking %NCX_HOOK_TOOL%"
# timeout_s = 10
```

完整示例见 `config.example.toml`。

Hooks 会在环境变量中收到 `NCX_HOOK_EVENT`、`NCX_HOOK_TOOL`、
`NCX_HOOK_ARGS`、`NCX_HOOK_RESULT` 和 `NCX_HOOK_WORKSPACE`。`pre_tool`
适合做确定性的风险拦截，例如阻止危险 shell 命令；`post_tool` 适合做审计和格式化；
`user_prompt` 可以在模型看到 prompt 前阻断或追加系统说明；`stop` 适合做轮次结束质量门
或通知。`UserPromptSubmit`、`Stop`、`PreToolUse`、`PostToolUse` 这类 Claude 风格
事件名会自动归一化。Hooks 会作为本地子进程运行，只配置你信任的命令。

## 自定义 Slash Commands

Rust REPL 可以把 Markdown prompt 模板变成 slash command。项目级命令放在
`.nanocodex/commands/<name>.md`；为了兼容 Claude Code，也会读取
`.claude/commands/<name>.md`。用户级命令放在 `~/.nanocodex/commands/<name>.md`，
并兼容 `~/.claude/commands/<name>.md`。

内置 REPL slash command 也暴露平台状态面：`/usage` 查看原始 token 和 context-edit
telemetry，`/budget` 查看 task-budget 用量，`/context` 查看当前 context-edit 策略和下一次
发送预览以及 provider payload 快照，`/tools` 查看工具 catalog 和当前可见 schema 视图，
`/memory` 查看项目记忆状态和 recall 预览，`/skills` 查看已发现 skill catalog，
`/history` 查看保存的会话，`/mcp` 查看 enabled MCP server 以及当前会话已注册的 MCP 工具。

Tauri GUI 也通过标题栏的 `/` 按钮暴露同一套 project/user command catalog。可以在面板里
填写参数后直接运行，也可以在聊天输入框里直接输入 custom slash command；GUI 会用和 CLI
相同的 core 模板引擎展开 prompt。

```markdown
---
description: Review one file
---
Review `$ARGUMENTS[0]` for bugs, regressions, and missing tests.
```

REPL 中可以这样调用：

```text
/review rust/crates/ncx-core/src/session.rs
/project:review rust/crates/ncx-core/src/session.rs
/user:review rust/crates/ncx-core/src/session.rs
```

`/name` 会优先解析项目级命令，再解析用户级命令。模板支持 `$ARGUMENTS` 表示原始参数串，
也支持 `$0`..`$9` 和 `$ARGUMENTS[0]`..`$ARGUMENTS[9]` 作为简单位置参数。模板里没有
占位符时，原始参数会自动追加到 `Arguments:` 块下。这类命令只会展开成普通用户 prompt，
不会自己运行本地 shell 代码。

## 本地模型 / OpenAI 兼容接口

nanocodex 走标准的 `/v1/chat/completions`，所以任何 OpenAI 兼容服务都能用——
vLLM、llama-server、LM Studio、Ollama 的 OpenAI 兼容层等等。把 `base_url`
指向该服务的 `/v1` 根路径即可。多数本地服务会忽略 API key，但仍需一个非空占位
值，因为 OpenAI SDK 需要它。

```toml
api_key = "local-dev-key"
base_url = "http://127.0.0.1:8005/v1"
model = "Qwen3.6-27B-Q4_K_M"
```

快速连通性检查：

```powershell
curl http://127.0.0.1:8005/v1/models
```

流式有一个有界的「响应头」超时（默认 45 秒，可用
`NANOCODEX_STREAM_OPEN_TIMEOUT_S` 覆盖），这样卡住的本地服务会带清晰提示快速
失败，而不是把 UI 一直挂住。

## 沙箱与审批

两个正交维度拦截每一次动作，对齐 Codex：

**沙箱模式** —— 物理上允许什么：

| 模式 | 读 | 写 | 网络 |
| --- | --- | --- | --- |
| `read-only` | 任意 | 无 | 关 |
| `workspace-write` | 任意 | 工作区 + 可写根 + 临时区 | 默认关，可显式开 |
| `danger-full-access` | 任意 | 任意 | 开 |

**审批策略** —— 动作超出沙箱时怎么办：`untrusted`、`on-failure`、`on-request`、
`never`。审批引擎把每次越权解析为 `ASK` / `AUTO_APPROVE` / `AUTO_DENY`。

在 Windows 上强制是**策略级**的：路径检查和可写根拦截发生在工具边界，不是内核级
隔离。

## MCP

MCP 服务是可选的（opt-in），并且运行在沙箱**之外**（它们会启动外部子进程）。底层
server 文件仍是 `~/.nanocodex/mcp.toml`：

```toml
[mcp_servers.fetch]
command = "uvx"
args = ["mcp-server-fetch"]

[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "D:\\projects"]
```

然后以启用 MCP 的方式启动：

```powershell
ncx --mcp
```

每个服务的工具以 `mcp__<server>__<tool>` 暴露给模型。服务表里可以设置
`enabled = false`，保留配置但不连接。

更可审计的 connector install 层放在 `~/.nanocodex/connectors.toml`。其中
`[connectors.<name>]` 可以声明 `transport`、`source`、`trusted`、`permission`、
`allowed_tools`、`auth`、`headers`、`headers_helper` 和 `[connectors.<name>.oauth]`。
stdio connector 会在 `ncx --mcp` 启动时转换成 MCP server；远程 `sse`/`http`
spec 目前只解析并在 `/mcp` 里展示脱敏 auth 元数据、`launch=audit-only`
和 `launch_gap=remote_transport_not_implemented`，等后续补完整 OAuth login
和远程 transport 后再真正启动。
stdio connector/server 注册时会执行 `allowed_tools` 和 `permission`
（`ask`、`trusted`、`read-only`、`deny`）；`allowed_tools` 可写原始 MCP 工具名，
也可写完整的 `mcp__<server>__<tool>` 名。
更多见 `mcp.example.toml` 和 `connectors.example.toml`。

Rust REPL 内可用 `/mcp` 查看 enabled server、connector install/auth spec、connector launch 状态、权限/信任/source/auth
元数据，以及当前会话已经注册的 MCP 工具。Tauri GUI 的 Tools 面板也会展示每个 MCP server
的 connected/error 状态、注册工具数、启动耗时、启动命令和最近错误。

## Skills

Skills 是可复用的指令文档，每个一个目录：

```text
~/.nanocodex/skills/<skill-name>/SKILL.md
```

只有每个 skill 的**名称和描述**会被注入系统提示；完整正文按需读取，所以一个庞大
的库不会吃掉上下文窗口。模型也可以通过 `manage_skills` 工具在对话里创建/读取/
删除用户 skill。

最小 skill：

```markdown
---
name: code-review
description: Review code changes and focus on bugs, regressions, and missing tests.
---

# Code Review

Look for behavior regressions first, then missing tests, then maintainability.
```

包内自带三个**只读内置 skill**，位于 `nanocodex/builtin_skills/`：

- **code-review** —— 两遍审查（先正确性，再清理），按影响排序。
- **debug** —— 复现 → 定位 → 修复 → 验证；克制住「补第一处看似合理的行」的冲动。
- **write-tests** —— 测可观察行为，一个测试一个行为，优先纯函数而非 mock。

同名的用户 skill 会遮蔽内置的。

## 记忆与 AGENTS.md

三层互补的持久上下文：

- **项目记忆**（`.ncx/memory/LEARNINGS.md`）—— 经过验证的项目笔记，每轮按当前 prompt
  检索成 recall 线索。由 `remember` 工具或 Tauri Memory 面板写入；可用
  `ncx --memory-merge`、启发式 Merge 或 LLM merge 维护。Rust `/memory [query]`
  会显示文件路径、向量索引路径、条目数、最近记录、tags 和 recall 预览。已验证笔记也会生成
  `.ncx/memory/INDEX.json` 本地确定性 vector sidecar；手工改 `LEARNINGS.md` 后可用
  `/memory index` 或 Tauri Memory 面板重建。`/memory` 还会显示 `embedding_backend`、
  `embedding_provider` 和 `embedding_gap`；GUI Settings 可保存 `memory_embedding_provider`、
  `memory_embedding_model`、`memory_embedding_base_url`、`memory_embedding_api_key_env`，
  作为后续外部 embedding recall 的审计配置入口，密钥本身仍只放在环境变量里。
- **记忆提议**（`.ncx/memory/PROPOSALS.md`）—— 有用但尚未可信的候选经验。`remember`
  可传 `propose=true`，CLI 可用 `/memory propose <note>`、`/memory edit <id> <note>`、
  `/memory accept <id>`、`/memory reject <id>`、`/memory accept-all`、`/memory reject-all`，
  或 `/memory harvest [path]` 从 handoff/release 文档提炼候选；Tauri Memory 面板也能从文档
  提炼、编辑文本/tags，并提供单条/批量接受拒绝。正常 turn 结束后，明确的用户纠正、工具失败、
  assistant 修复/验证说明也会被启发式提炼成待审提议。提议被接受前不会进入 `LEARNINGS.md`，也不会被召回。
- **用户记忆**（`~/.nanocodex/memory.md`）—— 持久的个人事实和偏好。由 `remember`
  工具写入、在 legacy Python GUI 输入框里打 `# 内容` 快速捕获、或手工编辑。Python 线会包在
  `<user_memory>` 块里。
- **AGENTS.md / CLAUDE.md** —— 项目指令，从 `~/.codex/AGENTS.md` 和
  `~/.claude/CLAUDE.md` 开始，再分层读取从仓库根到工作区的每个 `AGENTS.md`、
  `CLAUDE.md` 和 `.claude/CLAUDE.md`，所以嵌套目录可以细化父级。总大小有上限，避免
  一个超大文件撑爆上下文。Rust CLI、orchestrator worker 和 Tauri GUI 都会在会话启动时注入
  项目指令；项目记忆则按当前 prompt 在发送时单独召回。

记忆讲「谁/什么」（偏好、事实）；skills 讲「怎么做 X」；AGENTS.md / CLAUDE.md 是项目级指引。

## 会话、恢复与历史

- 每个对话都追加进一个 **JSONL 会话日志**（base64 图片数据会从日志里抹掉以保持
  精简）。
- 一个**全局索引**（`~/.nanocodex/sessions.jsonl`）每个对话存一行摘要，最新在前，
  供 GUI 的历史列表使用。
- 一个**单会话快照**（`~/.nanocodex/snapshots/<id>.json`）冻结完整对话，所以详情
  视图回放的是真实对话，而非摘要。
- Rust CLI 支持 `--resume`，启动前读回工作区 `.nanocodex/session.jsonl`；
  也支持 `--history` 列出最近的全局会话摘要。Tauri 后端每轮 GUI 对话结束后会记录
  同一套 snapshot。
- Tauri GUI 的 `S` 面板复用同一个全局索引，可以打开 JSONL 日志和冻结 snapshot；
  snapshot 属于当前 workspace 时，也可以直接恢复继续。
- Rust CLI 和 Tauri GUI 还会在每个模型轮次前保存工作区文件 checkpoint。CLI 用
  `/checkpoints` 查看，`/checkpoint <label>` 手动创建，`/restore <id>` 恢复文件；
  GUI 则在 checkpoint 面板里提供同一套保存 / 列表 / 恢复流程。恢复前会先给当前
  状态创建一个 safety checkpoint。
- 原 Python GUI 可以从保存的 snapshot **分叉（fork）**一条历史会话，且不会修改源会话。

## 上下文压缩

长对话会被折叠以保持在 token 预算内，同时保留系统消息和最近一段尾部（尾部总是从
一条 `user` 消息开始，所以不会切断 tool-call/result 对）。两种策略共用一个接口：

- **deterministic（默认，零 API 成本）** —— 被折叠的中段变成事实性的、基于规则的
  摘要。
- **summarizer（可选，消耗 token）** —— 一次模型调用把中段写成散文。

触发估算用偏中文的 chars/token 比例，所以中文为主的对话不会压缩得太晚。

Rust CLI 里的 `/compact` 会把当前 context-edit 策略物化到 live session，并重写工作区
session 日志；后续对话和 `--resume` 都会从压缩后的历史继续。`/compact <focus>` 会把
focus 指令写入摘要 checkpoint，并用它给旧历史 anchors 排名，所以最新可见尾部已经切到别的
主题时，也能保留任务相关事实。Rust `/context` 会用同一套
发送时编辑策略做无变异预览，展示策略旋钮、消息数、上一轮 telemetry，以及下一次发送会
压缩/丢弃多少。Rust `/usage` 和 Tauri GUI 的 `U` 面板也会展示 send-time context editing
telemetry：原始字符数、编辑后字符数、节省字符数、压缩工具结果数、丢弃消息数、摘要
checkpoint 数和 context-pack 桶。摘要 checkpoint 会以 assistant 消息插入到截断点前，
且只有在它小于被省略前缀时才物化，所以 `/compact` 与后续 `--resume` 能保留可审计桥梁。
摘要还会包含与最新用户请求或显式 compact focus 相关的旧消息 focus anchors，让长历史裁剪后仍保留少量任务相关事实。
`context_edit_max_chars`、`context_edit_max_history_chars`、`context_edit_max_tool_result_chars`
和 `context_edit_max_tool_result_total_chars` 留空或设为 `0` 时会按 `context_token_budget` 与
`context_window` 自动推导；发送时策略随后执行 `context_edit_max_history_chars` 和
`context_edit_max_tool_result_total_chars` 两个分桶上限，避免长历史或旧工具输出挤掉
system notes 与 memory recall。每次 provider 调用还会把脱敏 JSON 快照写到 `.nanocodex/context-payloads/`，所以
`/context payload [N]` 和 GUI Usage 面板可以检查真实发给模型的编辑后消息与工具 schema。

## Token 用量与成本

provider 每次调用返回真实 `usage`，包含 DeepSeek 的缓存命中/未命中拆分。Rust REPL
里的 `/usage` 和 `/cost` 会显示上一轮和 session 累计的模型调用数、工具调用数、输入
token、输出 token、缓存命中/未命中 token。Tauri GUI 的 `U` 面板也展示同一套来自桌面
事件流的上一轮和当前 session 原始用量。Rust 侧刻意只展示原始用量；Python 线的
`pricing.py` 会把 usage 折算成美元成本：

- **缓存感知** —— 一个缓存命中的输入 token 比未命中便宜约 120×；各按自己的费率
  计价。拆分缺失时，整段 prompt 按未命中费率计，所以成本永不低估。
- **对陈旧诚实** —— 价格是一份硬编码快照，带来源和「截至日期」；未知模型返回
  「成本未知」而不是一个错误数字。

## 定时器

把一个 prompt 存起来自动运行——一次性在某个未来时间，或按间隔周期运行：

```powershell
nanocodex schedule add "run the tests" --at 2026-06-08T09:00:00
nanocodex schedule add "summarize new issues" --every 3600
nanocodex schedule list
nanocodex schedule run        # 让它一直跑，任务才会触发
```

连续失败 5 次的任务会**自动禁用**（成功会重置计数器；重新启用会清零），所以一个
坏掉的任务不会永远循环。模型也可以通过 `manage_schedule` 在对话里管理任务。

## A/B worktree 对比

用**两套配置跑同一个 prompt**并对比结果，且不冒着破坏工作树的风险。每一侧都在自己
隔离的 **git worktree** 里跑，所以真实的 `shell`/`apply_patch` 改动永不冲突：

1. 选两套配置（model / 推理强度 / 沙箱 / 审批）。
2. nanocodex 从干净的 `HEAD` 建两个 worktree，串行地在每个里跑这个 prompt，
   审批 auto 放行但范围锁在 worktree 内。
3. 你拿到并排对比：diff、token 成本、延迟、迭代步数、停止原因。
4. **采纳**其中一侧（它的 diff 被应用到真实工作区），或两侧都丢弃；worktree 总会
   被清理。

要求工作区是干净的 git 仓库（无未提交改动），否则入口禁用。

## GUI

当前桌面线是 Tauri v2 + Svelte GUI（`rust/gui`）：

- 流式对话、工具调用展示和审批弹窗。
- Settings 面板可配置 model、sandbox、approval、预算、context editing、base URL 和 API key，
  并可直接打开 `~/.nanocodex/config.toml`、`mcp.toml` 和 `connectors.toml`；
  新安装且未配置 API key 时，GUI 会直接打开这个面板，先完成配置再启动首轮 agent。
- Sessions 面板提供全局历史、日志 / snapshot 打开入口，以及同 workspace snapshot 恢复。
- Checkpoint 面板提供手动保存 / 列表 / 恢复。
- Custom command 面板复用 CLI 同一套 `.nanocodex/.claude` 模板展开器。
- Tools 面板展示当前运行时真实注册的 core/MCP 工具目录、read-only / effectful 分类，以及
  MCP server runtime health、启动耗时和最近错误。
- Usage 面板展示上一轮和当前 session 的模型调用数、工具调用数、输入/输出 token、
  缓存命中/未命中 token、费用估算、context editing telemetry、provider payload 快照，
  以及 task-ledger 耗时/审批/预算报告。
- Memory 面板可查看项目笔记、新增 verified note、重建本地 vector index、从 handoff/release
  文档提炼待审提议、编辑提议文本/tags，单条或批量接受/拒绝、打开 `LEARNINGS.md`、启发式去重和 LLM 记忆合并。

原 Tkinter GUI 仍作为 Python 树里的 legacy 原型保留。注意：桌面 GUI 不热加载——改代码需要关掉再重开。

## 测试

Rust release 线：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\verify-rust.ps1
```

验证脚本会检查 `cargo fmt`，运行 `x86_64-pc-windows-gnu` target 的 Rust workspace
测试，并检查 Tauri backend。若 `cargo` 不在 `PATH`，脚本会探测常见 Windows Rust
安装位置，并打印安装 target 所需的 `rustup` 命令。只验证 CLI 时可加 `-SkipTauri`；
Rust 安装在非 PATH 位置时可传 `-Cargo C:\path\to\cargo.exe`。

等价手动命令：

```powershell
cd rust
cargo fmt --all --check
cargo test --workspace --target x86_64-pc-windows-gnu
cd gui\src-tauri
cargo check --target x86_64-pc-windows-gnu
```

Python 线：

```powershell
python -m pytest -q
```

两套测试都完全离线：mock 过的 provider、可注入的 I/O，不需要真实 API key 或网络
请求。

GitHub Actions 也会在 push、pull request 和手动触发时运行同一条 Windows Rust 验证链，
并跑 Python 离线测试。Windows job 会先安装 MinGW linker，再测试
`x86_64-pc-windows-gnu` target。本地 Windows 缺 Rust 工具链时，可以把 CI 结果作为
release gate。

## Release 打包

切 release 或合并 release 分支前，先按
[docs/release-checklist.md](docs/release-checklist.md) 逐项检查。

推荐的 Windows release 入口：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-rust-release.ps1
```

脚本会先跑 Rust workspace 测试，再构建 Windows GNU release 二进制，生成
`releases\nanocodex-<version>-x86_64-pc-windows-gnu.zip`，构建 Tauri NSIS
installer，并写出 `releases\SHA256SUMS.txt` 和 `releases\release-manifest.json`。
installer 构建会通过临时 Tauri config overlay 注入同一个版本号，因此 NSIS 元数据会跟随
Rust workspace release 版本。脚本使用和 `scripts\verify-rust.ps1` 相同的 cargo
探测路径；Rust 安装在非 PATH 位置时可传 `-Cargo C:\path\to\cargo.exe`。只需要 CLI 包时
可加 `-SkipTauri`；只有同 target 已在 CI 或本地 release 验证通过时，才建议加
`-SkipTests`。

release/benchmark 审计时，打包出的 CLI 也可以非交互输出 tool-search trace 健康度：

```powershell
.\releases\<unzipped>\ncx.exe --tools-eval-report
```

手动 Windows GNU CLI release：

```powershell
cd rust
cargo build --release --workspace --target x86_64-pc-windows-gnu
```

手动 Tauri 桌面 installer：

```powershell
cd rust\gui
npm.cmd ci
npm.cmd run tauri:installer
```

桌面构建现在明确产出 Windows NSIS installer，安装包位于
`rust\gui\src-tauri\target\x86_64-pc-windows-gnu\release\bundle\nsis\`。
GUI 的 Settings 弹窗也会展示解析后的 `~/.nanocodex/config.toml`、`mcp.toml` 和
`connectors.toml` 路径，并提供打开每个文件和配置目录的入口；缺失的 MCP/connector
文件会用注释模板创建，保存 Settings 后会原地重载 Rust agent。

Tauri crate 特意保留 `crate-type = ["lib"]`；改成 `cdylib` 或 `staticlib` 会让
Windows GNU 链接器的 export ordinal 表溢出。

## 安全说明

- **绝不提交真实 API key。** `.env`、`*.key`、`*.pem`、token 文件以及本地交接文件
  都被 git 忽略；`config.toml`、`mcp.toml` 和 `connectors.toml` 放在
  `~/.nanocodex/`，在仓库之外。
- 在 Windows 上沙箱是**策略级**的——它拦截工具行为和可写根，但不是内核级隔离。
- **MCP 工具运行在沙箱之外**，作为外部子进程。只启用你信任的服务；legacy 市场会校验
  名称但不审查行为。
- **Hooks 会在工具执行前后运行本地命令**。把 hook 配置当作代码审查后再启用。
- 外部内容（文件内容、命令输出、web/MCP 结果）被当作不可信数据，而非指令。

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
