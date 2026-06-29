//! McpTool — wraps an MCP server tool as a first-class ncx `Tool`.
//!
//! Each `McpTool` holds a reference-counted handle to the `McpClient` that owns
//! the server process. Multiple tools from the same server share one client via
//! `Rc<tokio::sync::Mutex<McpClient>>`, which serialises concurrent calls safely
//! on the current-thread runtime.
//!
//! Non-read-only tools go through the normal `ctx.approver` approval path unless
//! a connector policy marks the server trusted. Policies can also filter tools
//! before registration.

use std::collections::HashMap;
use std::rc::Rc;

use async_trait::async_trait;
use ncx_mcp::{McpClient, McpToolDef};
use ncx_sandbox::{Approver, Decision};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::tools::{ApprovalRequest, Tool, ToolContext, ToolRegistry};

// ── McpTool ───────────────────────────────────────────────────────────────────

pub struct McpTool {
    display_name: String,
    def: McpToolDef,
    client: Rc<Mutex<McpClient>>,
    read_only: bool,
    approval_required: bool,
}

impl McpTool {
    pub fn new(def: McpToolDef, client: Rc<Mutex<McpClient>>) -> Self {
        let read_only = is_read_only_name(&def.name);
        let display_name = def.name.clone();
        McpTool {
            display_name,
            def,
            client,
            read_only,
            approval_required: !read_only,
        }
    }

    pub fn with_policy(
        server: &str,
        def: McpToolDef,
        client: Rc<Mutex<McpClient>>,
        policy: &McpToolPolicy,
    ) -> Self {
        let read_only = is_read_only_name(&def.name);
        let approval_required = policy.approval_required(read_only);
        let display_name = mcp_tool_name(server, &def.name);
        McpTool {
            display_name,
            def,
            client,
            read_only,
            approval_required,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpToolPolicy {
    pub allowed_tools: Vec<String>,
    pub trusted: bool,
    pub permission: String,
}

impl McpToolPolicy {
    pub fn allows(&self, server: &str, tool: &str) -> bool {
        let permission = self.permission.trim().to_ascii_lowercase();
        if matches!(permission.as_str(), "deny" | "disabled" | "none") {
            return false;
        }
        if self.allowed_tools.is_empty() {
            return true;
        }
        let qualified = mcp_tool_name(server, tool);
        self.allowed_tools
            .iter()
            .any(|allowed| matches_allowed_tool(allowed, tool, &qualified))
    }

    fn approval_required(&self, read_only: bool) -> bool {
        let permission = self.permission.trim().to_ascii_lowercase();
        if read_only {
            return false;
        }
        if self.trusted || matches!(permission.as_str(), "trusted" | "auto" | "auto-approve") {
            return false;
        }
        true
    }

    fn read_only_only(&self) -> bool {
        self.permission.trim().eq_ignore_ascii_case("read-only")
    }
}

/// Heuristic: tool names that look like reads/queries don't require approval.
fn is_read_only_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    for prefix in &["read_", "get_", "list_", "fetch_", "search_", "find_"] {
        if lower.starts_with(prefix) {
            return true;
        }
    }
    matches!(lower.as_str(), "read" | "get" | "list" | "search" | "find")
}

fn matches_allowed_tool(allowed: &str, tool: &str, qualified: &str) -> bool {
    let allowed = allowed.trim();
    allowed == "*" || allowed == tool || allowed == qualified
}

fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", sanitize_tool_part(server), sanitize_tool_part(tool))
}

fn sanitize_tool_part(value: &str) -> String {
    let out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if out.is_empty() {
        "tool".to_string()
    } else {
        out
    }
}

#[async_trait(?Send)]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.display_name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    fn parameters(&self) -> Value {
        self.def.input_schema.clone()
    }

    fn read_only(&self) -> bool {
        self.read_only
    }

    async fn execute(&self, ctx: &ToolContext, args: &Value) -> String {
        if self.approval_required {
            let decision = Approver::new(&ctx.approval_policy).classify(&self.display_name, true);
            match decision {
                Decision::AutoDeny => {
                    return format!(
                        "Error: MCP tool '{}' denied by approval policy '{}' (non-read-only).",
                        self.display_name, ctx.approval_policy
                    );
                }
                Decision::Ask => {
                    if let Some(approver) = &ctx.approver {
                        let details = serde_json::to_string_pretty(args).unwrap_or_default();
                        let ans = approver
                            .request(ApprovalRequest {
                                command: format!("mcp:{} {args}", self.display_name),
                                reason: format!(
                                    "MCP tool '{}' may have side effects.",
                                    self.display_name
                                ),
                                cwd: ctx.workspace.display().to_string(),
                                escalated: true,
                                details,
                            })
                            .await;
                        if !ans.approved() {
                            return format!(
                                "Error: MCP tool '{}' not approved by the user.",
                                self.display_name
                            );
                        }
                    }
                    // No approver configured: fall through and call the tool.
                    // (Consistent with how ShellTool behaves when auto-approving.)
                }
                Decision::AutoApprove => {}
            }
        }

        let mut client = self.client.lock().await;
        match client.call_tool(&self.def.name, args).await {
            Ok(out) => out,
            Err(e) => format!("Error: MCP tool '{}' failed: {e}", self.display_name),
        }
    }
}

// ── startup helper ────────────────────────────────────────────────────────────

/// Connect to an MCP server, list its tools, and register each as a `McpTool`.
/// Prints a progress line to stderr. Returns the number of tools registered.
pub async fn register_mcp_server(
    tools: &mut ToolRegistry,
    name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<usize, String> {
    register_mcp_server_with_policy(
        tools,
        name,
        command,
        args,
        env,
        &McpToolPolicy::default(),
    )
    .await
}

pub async fn register_mcp_server_with_policy(
    tools: &mut ToolRegistry,
    name: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    policy: &McpToolPolicy,
) -> Result<usize, String> {
    let mut client = McpClient::connect(name, command, args, env).await?;
    let defs = client.list_tools().await?;
    let shared = Rc::new(Mutex::new(client));
    let mut n = 0;
    for def in defs {
        if !policy.allows(name, &def.name) {
            continue;
        }
        if policy.read_only_only() && !is_read_only_name(&def.name) {
            continue;
        }
        tools.register(Box::new(McpTool::with_policy(
            name,
            def,
            shared.clone(),
            policy,
        )));
        n += 1;
    }
    Ok(n)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_heuristic() {
        assert!(is_read_only_name("read_file"));
        assert!(is_read_only_name("get_weather"));
        assert!(is_read_only_name("list_todos"));
        assert!(is_read_only_name("fetch_url"));
        assert!(is_read_only_name("search_web"));
        assert!(is_read_only_name("find_issues"));
        assert!(is_read_only_name("read"));
        assert!(is_read_only_name("list"));
        assert!(!is_read_only_name("write_file"));
        assert!(!is_read_only_name("create_issue"));
        assert!(!is_read_only_name("delete_branch"));
        assert!(!is_read_only_name("execute_code"));
    }

    #[test]
    fn policy_allows_exact_or_qualified_tool_names() {
        let policy = McpToolPolicy {
            allowed_tools: vec!["list".into(), "mcp__fs__read".into()],
            trusted: false,
            permission: "ask".into(),
        };

        assert!(policy.allows("fs", "list"));
        assert!(policy.allows("fs", "read"));
        assert!(!policy.allows("fs", "write"));
    }

    #[test]
    fn mcp_tool_names_are_qualified_for_the_model() {
        assert_eq!(mcp_tool_name("fs", "list"), "mcp__fs__list");
        assert_eq!(
            mcp_tool_name("server.name", "read/path"),
            "mcp__server_name__read_path"
        );
    }

    #[test]
    fn policy_permission_controls_registration_and_approval() {
        let deny = McpToolPolicy {
            permission: "deny".into(),
            ..Default::default()
        };
        assert!(!deny.allows("fs", "list"));

        let trusted = McpToolPolicy {
            trusted: true,
            permission: "ask".into(),
            ..Default::default()
        };
        assert!(!trusted.approval_required(false));

        let ask = McpToolPolicy::default();
        assert!(ask.approval_required(false));
        assert!(!ask.approval_required(true));
    }

    // A live round-trip (connect → list_tools → register → execute echo tool)
    // against the same Python mock server used in ncx-mcp's own tests.
    fn write_mock_server() -> std::path::PathBuf {
        let src = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    mid = msg.get("id")
    method = msg.get("method")
    if method == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":mid,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"mock","version":"0"}}}), flush=True)
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        print(json.dumps({"jsonrpc":"2.0","id":mid,"result":{"tools":[
            {"name":"echo","description":"echo text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}},
            {"name":"write_note","description":"write a note","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}
        ]}}), flush=True)
    elif method == "tools/call":
        args = msg.get("params",{}).get("arguments",{})
        print(json.dumps({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"text","text":"echo: "+str(args.get("text",""))}]}}), flush=True)
    else:
        print(json.dumps({"jsonrpc":"2.0","id":mid,"result":{}}), flush=True)
"#;
        let dir = std::env::temp_dir().join("ncx_mcp_tool_mock");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("mock_server.py");
        std::fs::write(&p, src).unwrap();
        p
    }

    #[tokio::test]
    async fn register_and_execute_echo() {
        use ncx_sandbox::{SandboxPolicy, WORKSPACE_WRITE};

        let server = write_mock_server();
        let ws = std::env::temp_dir().join("ncx_mcp_tool_ws");
        std::fs::create_dir_all(&ws).unwrap();
        let ws = ws.canonicalize().unwrap();

        let ctx =
            crate::tools::ToolContext::new(ws.clone(), SandboxPolicy::new(WORKSPACE_WRITE, &ws));
        let mut reg = ToolRegistry::empty(ctx);

        let result = register_mcp_server(
            &mut reg,
            "mock",
            "python",
            &[server.to_string_lossy().to_string()],
            &HashMap::new(),
        )
        .await;

        let n = match result {
            Ok(n) => n,
            Err(e) => {
                eprintln!("skipping mcp_tool live test (no python?): {e}");
                return;
            }
        };
        assert_eq!(n, 2);

        assert!(reg.get("mcp__mock__echo").is_some());
        assert!(reg.get("mcp__mock__write_note").is_some());
        assert!(!reg.is_read_only("mcp__mock__write_note"));

        let out = reg
            .execute("mcp__mock__echo", &serde_json::json!({"text": "hello mcp"}))
            .await;
        assert_eq!(out, "echo: hello mcp");
    }
}
