# Claude Code / Fable 差距路线图

更新日期：2026-06-29

这份文档把 `nanocodex` 当前 Rust/Tauri 架构，与公开 Claude Code / Fable 级平台能力做工程层面对比。它不是模型 benchmark，也不声称能复刻 Anthropic 的模型内建能力；它用于决定本项目接下来最值得补的工程面。

参考来源：

- Claude Code overview: https://code.claude.com/docs/en/overview
- Claude Code context window: https://code.claude.com/docs/en/context-window
- Claude Code memory: https://code.claude.com/docs/en/memory
- Claude Code MCP: https://code.claude.com/docs/en/mcp
- Claude Code hooks: https://code.claude.com/docs/en/hooks
- Claude Agent SDK overview: https://code.claude.com/docs/en/agent-sdk/overview

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
| Task budget | Rust loop 强制 per-turn model/tool call budget；CLI `/budget` 可见；Tauri GUI 已恢复每轮模型/工具调用累计面板。 | 嵌套 subagent budget、wall-clock budget、预算超限后的续跑策略、CI 预算回归测试。 |
| Context editing | `Session::for_model_edited` 发送时压缩旧 tool result、按预算丢旧前缀；CLI `/context` 与 Tauri Usage 面板展示 telemetry。 | 策略化 context pack、按来源分配预算、长期任务的摘要检查点、对 1M context 模型的适配策略。 |
| Tool search | Tool catalog 有 read-only/effectful 标记，`schemas_for_query`/`tool_search` 降低 schema 过载；CLI `/tools` 与 Tauri Tools 面板都可检查 runtime catalog。 | ranking 评测集、MCP tool metadata 权重、connector auth/permission 治理。 |
| Semantic memory | `.ncx/memory/LEARNINGS.md`、query-scoped recall、`remember`、启发式/LLM merge、Tauri Memory 面板已有。 | 自动从纠错/复盘提炼 memory、embedding/vector 可选 provider、memory review queue、跨入口治理。 |

## 平台级差距

1. Anthropic 原生工具生态和 connector 平台

当前差距：约 65-75% 仍未补齐。

`nanocodex` 有 MCP stdio 客户端、工具注册表和审批，但还不是“平台 connector”。缺少 OAuth/remote auth UX、可安装 connector registry、工具权限审计、连接状态健康检查和大规模工具排序。

下一步：

- Tauri GUI 已有基础 Tools 面板，可展示 runtime tool catalog、core/MCP 来源、read-only/effectful 分类。
- 给 MCP server 增加健康状态、最近失败原因、启动耗时。
- 增加 connector install spec，而不是只读 `mcp.toml`。

2. Managed task budget / cloud execution

当前差距：约 60-70% 仍未补齐。

本地 task budget 已有，但 Claude Code/Agent SDK 的平台面更像托管执行环境：远端 runner、隔离 VM、队列、任务额度、日志、权限和监控联动。

下一步：

- 增加 `TaskLedger`：记录每个 session 的 model calls、tool calls、wall time、approval count、stop reason。
- 让 CLI/Tauri 都可导出 budget report，给 release/benchmark 用。
- 增加子任务 budget 传播，避免 orchestrator/subagent 把主任务预算绕开。

3. Context editing / context management

当前差距：约 45-55% 仍未补齐。

本项目现在做的是发送时编辑视图，优点是本地完整 session 不丢；缺点是策略还粗，无法等价于模型侧长上下文、上下文缓存、平台自动 compact 和长任务 memory 的组合。

下一步：

- context pack：把 project instructions、recent edits、tool results、memory recall 分桶预算化。
- 对每轮 provider payload 写 snapshot，方便调试“为什么这轮模型没看到某信息”。
- 增加 context regression tests，覆盖大工具输出、长会话、memory recall 竞争预算。

4. Tool search

当前差距：约 35-45% 仍未补齐。

已有 schema gating，但还缺评价体系。没有评测就不知道 tool search 是在减少干扰，还是错过必要工具。

下一步：

- 建立 20-50 条 tool-selection gold cases。
- 记录每轮 visible tools 与实际 called tools。
- 对 MCP tool 增加 description/namespace/category hints。

5. Semantic memory

当前差距：约 45-55% 仍未补齐。

当前 memory 是本地 project memory，适合仓库级经验；还不是跨入口、可审计、自动建议的长期 memory 系统。

下一步：

- 从失败修复、用户纠正、release checklist 中生成 memory proposals。
- Tauri Memory 面板增加 accept/reject/edit queue。
- 可选 embedding index，只作为增强 recall，不替代文本文件的可审计性。

6. 模型内建 memory / code execution / 长上下文能力

当前差距：约 80-90% 难以通过本地代码完全补齐。

这类能力依赖模型和平台：模型内部长期记忆、模型侧代码执行环境、1M context 质量、agent teams 的云端运行和安全边界。`nanocodex` 可以做 adapter 和本地近似，但不能把模型能力本身补出来。

下一步：

- 把 provider 层继续保持 OpenAI-compatible，方便切到更强模型。
- 给本地 code execution 做显式 sandbox tool，而不是假装等价于模型内建执行。
- 在 README 中持续区分“runtime 能力”和“模型/平台能力”。

## 当前新进展

- GUI/Tauri release 分支已集成到 `codex/gui-mcp-integration`。
- Tauri GUI 已补回 Usage/Context 面板：每轮 `done` 事件携带 `iterations`、`tools_used`、token usage 和 context-edit telemetry；前端展示上一轮与当前 session 汇总。
- Tauri GUI 已新增 Tools 面板：agent 线程按需发出真实 runtime tool catalog，前端展示 core/MCP 来源与 read-only/effectful 分类。
- `cmd /c npm run build` 已通过，证明 Svelte/Tauri 前端合并后可构建。

## 最近可执行 backlog

1. 跑 Windows Rust 工具链验证：`powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\verify-rust.ps1`。
2. CI 通过后，从 `codex/gui-mcp-integration` 开 PR，优先处理 Rust/Tauri 编译问题。
3. 给 MCP/Tool catalog 增加健康状态、最近错误、连接耗时和 auth/permission 治理。
4. 增加 `TaskLedger`，让 task budget 从“当前轮限制”升级为“可审计的 session/task 账本”。
5. 增加 context-pack 策略和 payload snapshot，进一步缩小 context editing 差距。
