# nanocodex

[English](README.md) | 简体中文

`nanocodex` 是一个用于本地实验的小型 Codex 风格编码 agent。它刻意把核心循环
做得简单：一个 chat-completions 模型提出工具调用，agent 执行安全的文件/shell
工具，记录会话，再让模型继续，直到任务完成。

这个项目主要用来测试一个编码 agent 在以下场景下的行为：DeepSeek、
OpenAI 兼容的本地模型、MCP 工具、skills、沙箱策略、上下文压缩，以及一个
轻量的 Windows GUI。

## 功能

- Codex 风格工具：`apply_patch`、`shell`、`update_plan`、`read_file`、
  `web_search`、记忆（memory）和定时任务（scheduled tasks）。
- DeepSeek / OpenAI 兼容后端，走 `/v1/chat/completions`。
- 本地模型支持：只需修改 `base_url`，例如 vLLM、llama-server、LM Studio，
  或任何 OpenAI 兼容的服务。
- 沙箱与审批策略状态机：`read-only`、`workspace-write`、
  `danger-full-access`；`untrusted`、`on-failure`、`on-request`、`never`。
- MCP 集成，来自 `~/.nanocodex/mcp.toml`，以 `mcp__<server>__<tool>` 形式
  暴露为工具。
- Skills 系统，来自 `~/.nanocodex/skills/<name>/SKILL.md`，外加内置的
  `code-review`、`debug`、`write-tests` 三个 skill。
- 会话 JSONL、上下文压缩、prompt 增强、token 用量/成本统计、GUI、
  定时器（scheduler），以及 A/B worktree 对比。

## 安装

```powershell
cd path\to\nanocodex
python -m pip install -e ".[dev]"
```

运行 CLI：

```powershell
nanocodex --cd .
nanocodex "add a --json flag to the CLI"
```

运行 GUI：

```powershell
nanocodex-gui --cd .
```

在 Windows 上，安装后也可以直接双击 `nanocodex-gui.cmd`。

## 配置

配置项按以下优先级解析：

```text
CLI 参数 > 环境变量 > ~/.nanocodex/config.toml > ~/.deepseek/config.toml > ~/.codex/config.toml > 默认值
```

真实 API key 应当留在仓库之外。可以用以下任意一种方式：

```powershell
$env:DEEPSEEK_API_KEY = "sk-..."
$env:NANOCODEX_API_KEY = "sk-..."
```

或创建 `~/.nanocodex/config.toml`：

```toml
api_key = "sk-..."
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
```

完整示例见 `config.example.toml`。

## 本地模型 / OpenAI 兼容接口

如果用本地模型服务，把 `base_url` 指向该服务的 `/v1` 根路径即可。很多本地
服务会忽略 API key，但 `nanocodex` 仍然要求一个非空的占位值，因为 OpenAI
SDK 需要它。

示例：

```toml
api_key = "local-dev-key"
base_url = "http://127.0.0.1:8005/v1"
model = "Qwen3.6-27B-Q4_K_M"
```

快速连通性检查：

```powershell
curl http://127.0.0.1:8005/v1/models
```

## MCP

MCP 服务是可选的（opt-in），并且运行在沙箱之外。在这里配置：

```text
~/.nanocodex/mcp.toml
```

示例：

```toml
[mcp_servers.fetch]
command = "uvx"
args = ["mcp-server-fetch"]
```

然后以启用 MCP 的方式启动 nanocodex：

```powershell
nanocodex --mcp
```

更多示例见 `mcp.example.toml`。

## Skills

Skills 是可复用的指令文档：

```text
~/.nanocodex/skills/<skill-name>/SKILL.md
```

只有每个 skill 的名称和描述会被注入系统提示。完整正文按需读取，因此一个庞大
的 skills 库不会自动吃掉整个上下文窗口。

最小 skill：

```markdown
---
name: code-review
description: Review code changes and focus on bugs, regressions, and missing tests.
---

# Code Review

Look for behavior regressions first, then missing tests, then maintainability.
```

包内还自带了只读的内置 skill，位于 `nanocodex/builtin_skills/`。

## 测试

```powershell
python -m pytest -q
```

测试套件是离线的，使用 mock 过的 provider；不需要真实 API key 或网络请求。

## 安全说明

- 不要提交真实 API key。`.env`、`*.key`、token 文件以及本地交接文件都已被
  忽略。
- 在 Windows 上，沙箱是策略级（policy-level）的。它会拦截工具行为和可写
  根目录，但不是内核级隔离。
- MCP 工具会在沙箱之外执行外部子进程。只启用你信任的 MCP 服务。
