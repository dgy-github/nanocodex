# Claude Code / Fable 差距路线图

更新日期：2026-06-29

这份文档把 `nanocodex` 当前 Rust/Tauri 架构，与公开 Claude Code / Fable 级平台能力做工程层面对比。它不是模型 benchmark，也不声称能复刻 Anthropic 的模型内建能力；它用于决定本项目接下来最值得补的工程面。

参考来源：

- Claude Code overview: https://code.claude.com/docs/en/overview
- Claude Code context window: https://code.claude.com/docs/en/context-window
- Claude Code memory: https://code.claude.com/docs/en/memory
- Claude Code MCP: https://code.claude.com/docs/en/mcp
- Claude Code subagents: https://code.claude.com/docs/en/sub-agents
- Claude Code hooks: https://code.claude.com/docs/en/hooks
- Claude Agent SDK overview: https://code.claude.com/docs/en/agent-sdk/overview
- Claude models overview: https://docs.anthropic.com/en/docs/about-claude/models/overview

## 当前结论

`nanocodex` 已经从 Python 原型进入 Rust 本地 agent runtime 阶段：typed loop、sandbox、approval、tool registry、MCP、skills、project memory、context editing、checkpoints、session snapshots、Tauri GUI 和 release scripts 都在。但与 Claude Code + Fable 级模型相比，差距主要不在“有没有一个工具循环”，而在平台级闭环：

| 对比范围 | 当前估算 | 结论 |
| --- | ---: | --- |
| 只看本地 coding-agent harness，并假设接入同等级前沿模型 | 55-65% | 核心 loop、工具、安全、memory、context、GUI 控制面已具备；生态与自动治理还弱。 |
| 默认 DeepSeek/OpenAI-compatible 模型线，对比 Claude Code + Fable 级模型端到端表现 | 35-45% | 硬任务会被模型推理、长上下文质量、工具纪律和原生平台集成拉开。 |
| Windows 本地桌面分发体验 | 60-70% | CLI zip、Tauri NSIS installer、配置入口已有；产品抛光和跨入口体验还不到平台级。 |

## 四个已补控制面

| 能力 | 当前 nanocodex 状态 | 下一步缺口 |
| --- | --- | --- |
| Task budget | Rust loop 强制 per-turn model/tool call budget；CLI `/budget` 可见；Tauri GUI 已恢复每轮模型/工具调用累计面板；`TaskLedger` 写入 `.nanocodex/task-ledger.jsonl` 并报告平均耗时、预算耗尽率和模型/工具预算利用率；orchestrator reason/worker 节点共享父任务预算，worker 会按并行度预留预算并退回未用额度。 | 云端/队列额度、预算超限后的续跑策略、更完整的 CI/端到端预算回归测试。 |
| Context editing | `Session::for_model_edited` 发送时压缩旧 tool result、执行可配置 history/tool-result 分桶上限、按预算丢旧前缀，并在截断前生成确定性 assistant 摘要 checkpoint；CLI `/context` 与 Tauri Usage 面板展示 telemetry；每次 provider 调用会写脱敏 payload snapshot 供 `/context payload [N]` 和 GUI Usage 面板审计；telemetry 已按 system/runtime notes/memory/history/tool result 拆出 context-pack 桶。 | focus compaction、对 1M context 模型的适配策略、更强 context regression suites。 |
| Tool search / connectors | Tool catalog 有 read-only/effectful 标记，`schemas_for_query`/`tool_search` 降低 schema 过载；CLI `/tools` 与 Tauri Tools 面板都可检查 runtime catalog；GUI 也显示 MCP server 连接状态、工具数、启动耗时和最近错误；新增 `connectors.toml` install spec 可审计 transport/source/trusted/permission/allowed_tools，并在 stdio MCP 注册时执行 allow-list 与 permission 策略；tool_search 已加入 MCP namespace-aware 评分和 tool-selection gold-case 回归测试。 | 扩大 ranking 评测集、远程 auth/OAuth、connector registry 治理、大规模动态工具排序。 |
| Semantic memory | `.ncx/memory/LEARNINGS.md`、query-scoped recall、本地 `.ncx/memory/INDEX.json` vector sidecar、`remember`、启发式/LLM merge、`.ncx/memory/PROPOSALS.md` review queue、CLI `/memory edit/accept/reject/accept-all/reject-all/harvest/index`、Tauri Memory 面板编辑/批量 review/文档提炼/重建索引，以及运行时纠正/工具失败/修复说明提炼已有。 | 外部 embedding provider、平台级长期 memory。 |

## 平台级差距

1. Anthropic 原生工具生态和 connector 平台

当前差距：约 65-75% 仍未补齐。

`nanocodex` 有 MCP stdio 客户端、工具注册表、审批和 GUI 连接状态可见性，也开始有本地 connector install spec。但它还不是“平台 connector”：缺少 OAuth/remote auth UX、托管 connector registry、集中工具权限审计和大规模工具排序。

已完成：

- Tauri GUI 已有 Tools 面板，可展示 runtime tool catalog、core/MCP 来源、read-only/effectful 分类，以及 MCP server connected/error、最近失败原因和启动耗时。
- 新增 `~/.nanocodex/connectors.toml`：stdio connector 可转换为 MCP server；`allowed_tools` 与 `permission` 会在注册时过滤/约束 MCP tools；remote `sse`/`http` spec 会被解析并在 CLI `/mcp` 中展示 transport/source/trusted/permission/allowed_tools，作为 auth/OAuth 落地前的审计面。

下一步：

- 增加 connector auth/OAuth、远程 transport 启动、托管 registry 和更细的工具权限审计。

2. Managed task budget / cloud execution

当前差距：约 60-70% 仍未补齐。

本地 task budget 已有，但 Claude Code/Agent SDK 的平台面更像托管执行环境：远端 runner、隔离 VM、队列、任务额度、日志、权限和监控联动。

已完成：

- 增加 `TaskLedger`：记录每个 session 的 model calls、tool calls、wall time、approval count、stop reason。
- 让 CLI/Tauri 都可导出 budget report，给 release/benchmark 用；报告包含平均任务耗时、预算耗尽率和模型/工具预算利用率。
- 给 live orchestrator 增加共享预算池：reason 节点只消耗模型预算，parallel worker/subagent 按并行度预留父任务预算，结束后退回未使用额度，避免 best-of-N 或递归 subtask 绕开主任务预算。
- 增加预算池单元测试，覆盖 reason 预留、worker 分摊、未用额度退回和预算耗尽拒绝新节点。

下一步：

- 云端 runner/队列额度、预算超限后的续跑策略，以及更完整的端到端预算回归。

3. Context editing / context management

当前差距：约 32-40% 仍未补齐。

本项目现在做的是发送时编辑视图，优点是本地完整 session 不丢；新增分桶预算和确定性摘要 checkpoint 后，旧工具输出和长历史不再能无边挤压 system notes/memory，被截断的旧前缀也会留下可审计摘要。剩余差距主要是模型侧长上下文、上下文缓存、平台自动 compact、focus compaction 和长任务 memory 的组合。

已完成：

- 对每轮 provider payload 写脱敏 snapshot：记录实际发送的编辑后 messages、工具 schema 名称、role 统计和 context-edit 压缩/丢弃数据，方便调试“为什么这轮模型没看到某信息”。
- CLI `/context payload [N]` 与 Tauri Usage 面板可读取 `.nanocodex/context-payloads/` 的最近快照。
- CLI `/context`、payload snapshot 和 Tauri Usage 面板已展示 context-pack 分桶 telemetry：system prompt、runtime notes、memory recall、history、tool result 字符数。
- 新增 context bucket budget：`context_edit_max_history_chars` 和 `context_edit_max_tool_result_total_chars` 会在发送时主动压缩旧工具结果、丢弃旧历史前缀；CLI flag、config/env 和 Tauri Settings 均可调。
- 新增长期任务摘要 checkpoint：旧前缀被截断前会物化为确定性的 assistant 摘要消息，`/compact`、`--resume`、payload snapshot、CLI `/usage` 和 Tauri Usage 面板都能审计 `summary_checkpoints`。

下一步：

- 增加 focus compaction / context regression tests，覆盖大工具输出、长会话、memory recall 竞争预算和 1M context 模型适配。

4. Tool search

当前差距：约 30-40% 仍未补齐。

已有 schema gating、MCP namespace-aware 排序和基础 gold-case 回归，但评测集规模仍小。还需要更多真实任务覆盖，确认 tool search 是在减少干扰，而不是错过必要工具。

下一步：

- 把 tool-selection gold cases 扩到 20-50 条。
- 记录每轮 visible tools 与实际 called tools。
- 对 MCP tool 增加更细的 category/capability hints。

5. Semantic memory

当前差距：约 18-26% 仍未补齐。

当前 memory 是本地 project memory，适合仓库级经验；已具备可审计的 verified/pending 两层文件、CLI/Tauri review queue、提议编辑/批量处理、从 handoff/release/roadmap 文档启发式提炼候选、每轮结束后从用户纠正/工具失败/assistant 修复验证说明里生成待审 proposals，以及已验证 memory 的本地 deterministic vector sidecar recall。但它仍不是模型/平台级的长期 memory，外部 embedding provider 也还没有接入。

已完成：

- 新增 `.ncx/memory/PROPOSALS.md`：候选经验先进入 pending queue，被接受前不会进入 `LEARNINGS.md`，也不会被 recall。
- `remember` 支持 `propose=true`；CLI 支持 `/memory propose <note>`、`/memory accept <id>`、`/memory reject <id>`；Tauri Memory 面板可查看待审提议并接受/拒绝。
- 新增文档提炼入口：CLI `/memory harvest [path]` 和 Tauri Memory 面板“从文档提炼”会从 `HANDOFF.md`、`RELEASE_TASK.md`、`docs/release-checklist.md`、路线图等文档中抽取待审 proposals。
- 新增 edit/batch review：CLI 支持 `/memory edit <id> <note>`、`/memory accept-all`、`/memory reject-all`；Tauri Memory 面板可编辑提议文本/tags，并单条或批量接受/拒绝。
- 新增 runtime 自动提炼：AgentLoop 每轮结束后会从本轮用户纠正、工具失败输出、assistant 修复/验证说明中生成 pending proposals，只有 review 接受后才进入 recall。
- 新增本地 vector sidecar：已验证笔记会写入 `.ncx/memory/INDEX.json`，recall 会融合词法/同义词分数和本地向量分数；CLI `/memory index` 与 Tauri Memory 面板可重建索引。

下一步：

- 外部 embedding provider 作为增强 recall，不替代文本文件的可审计性。

6. 模型内建 memory / code execution / 长上下文能力

当前差距：约 80-90% 难以通过本地代码完全补齐。

这类能力依赖模型和平台：模型内部长期记忆、模型侧代码执行环境、1M context 质量、agent teams 的云端运行和安全边界。`nanocodex` 可以做 adapter 和本地近似，但不能把模型能力本身补出来。

下一步：

- 把 provider 层继续保持 OpenAI-compatible，方便切到更强模型。
- 给本地 code execution 做显式 sandbox tool，而不是假装等价于模型内建执行。
- 在 README 中持续区分“runtime 能力”和“模型/平台能力”。

## 当前新进展

- GUI/Tauri release 分支已集成到 `codex/gui-mcp-runtime-conflict-merge`（上一稳定集成分支为 `codex/gui-mcp-integration`）。
- Tauri GUI 已补回 Usage/Context 面板：每轮 `done` 事件携带 `iterations`、`tools_used`、token usage 和 context-edit telemetry；前端展示上一轮与当前 session 汇总。
- Tauri GUI 已新增 Tools 面板：agent 线程按需发出真实 runtime tool catalog，前端展示 core/MCP 来源、read-only/effectful 分类，以及 MCP server connected/error、注册工具数、启动耗时和最近错误。
- Tauri GUI Settings 已把配置入口扩展到 `~/.nanocodex/config.toml`、`mcp.toml` 和 `connectors.toml`，缺失的 MCP/connector 文件会用注释模板创建，方便配置 allow-list/permission。
- 已新增 connector install spec：`connectors.example.toml` 说明本地 connector metadata；`load_mcp_connectors()` 解析 `~/.nanocodex/connectors.toml`；stdio connector 会并入 `load_mcp_servers()`；MCP 注册会执行 `allowed_tools` 和 permission 策略；CLI `/mcp` 会展示 connector permission/trusted/allowed_tools。
- 已增强 tool_search：评分会分解 MCP server/tool namespace，MCP 工具 description 带 server/tool 元数据，并新增基础 tool-selection gold cases。
- 已新增 `TaskLedger`：CLI 与 Tauri 每轮完成后写 `.nanocodex/task-ledger.jsonl`，CLI 支持 `--budget-report` 和 `/budget report`，Tauri Usage 面板可读取最近任务报告；报告已加入平均耗时、预算耗尽率和模型/工具预算利用率。
- 已新增 provider payload snapshot：agent loop 在每次模型调用前把发送时编辑后的消息和工具 schema 摘要写入 `.nanocodex/context-payloads/`；CLI `/context payload [N]` 与 Tauri Usage 面板可审计最近快照。
- 已新增 context-pack bucket telemetry：CLI、payload snapshot 和 Tauri Usage 面板能看到 system/runtime notes/memory/history/tool result 字符占比。
- 已新增 context bucket budget：发送时策略会根据 history/tool-result 桶上限压缩旧工具输出或丢弃旧历史前缀；CLI/config/env/Tauri Settings 均可配置。
- 已新增长期任务摘要 checkpoint：旧历史前缀在截断前会变成确定性 assistant 摘要，CLI/Tauri telemetry 会暴露 summary checkpoint 计数。
- 已新增 orchestrator/subagent shared budget：CLI live runner 中的 reason/worker 节点共用父任务预算，parallel worker 按剩余 worker 数公平预留预算，未用额度退回池中，并在 orchestrator 状态行输出剩余预算。
- 已新增 memory proposal review queue 和本地 vector recall：候选经验写入 `.ncx/memory/PROPOSALS.md`，CLI `/memory` 可 propose/edit/accept/reject/accept-all/reject-all/harvest/index，Tauri Memory 面板可从文档提炼、编辑、重建索引并单条/批量接受拒绝；AgentLoop 也会从运行时纠正/失败/修复说明自动生成待审 proposals，只有接受后的内容才进入 `.ncx/memory/LEARNINGS.md` 并参与 recall。
- `cmd /c npm run build` 已通过，证明 Svelte/Tauri 前端合并后可构建。

## 最近可执行 backlog

1. 跑 Windows Rust 工具链验证：`powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\verify-rust.ps1`。
2. CI 通过后，从 `codex/gui-mcp-runtime-conflict-merge` 开 PR，优先处理 Rust/Tauri 编译问题。
3. 给 MCP/Tool catalog 增加 connector auth/OAuth、远程 transport 启动和权限审计。
4. 增加云端/队列 budget 策略、预算超限续跑策略和更完整的端到端预算回归测试。
5. 增加 focus compaction、1M context 适配策略和 context regression tests，进一步缩小 context editing 差距。
