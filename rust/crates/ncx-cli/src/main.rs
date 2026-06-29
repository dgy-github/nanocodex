//! ncx — nanocodex CLI (Rust). Entry point + REPL.
//!
//! Rust port of the runnable surface of `nanocodex/cli.py`: argument parsing,
//! config resolution, building the provider + tool registry + turn loop, a
//! one-shot mode (`ncx "do X"`) and an interactive REPL with slash commands.
//!
//! Kept dependency-light (hand-rolled arg parsing, no clap) in line with the
//! rewrite's goal: fast startup and a small single binary.

mod args;
mod runner;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ncx_config::{
    load_config, load_mcp_connectors, load_mcp_servers, permission_mode_to_knobs,
    write_nanocodex_config, Config, ConfigPaths, Overrides, VALID_PERMISSION_MODES,
    WRITABLE_KEYS,
};
use ncx_core::slash::{is_known, parse_slash, SLASH_HELP};
use std::rc::Rc;

use ncx_core::{
    custom_command_prompt, discover_skills, expand_file_mentions, list_custom_commands,
    load_project_instructions, new_session_id, parse_custom_command_query,
    register_mcp_server_with_policy, skills_index_block, AgentLoop, CheckpointMeta,
    CheckpointStore, ContextEditPolicy, ContextEditStats, ContextPayloadSnapshotStore, Genome,
    McpToolPolicy, MemoryStore, Orchestrator, OrchestratorConfig, Provider, Session,
    SessionIndex, SessionSummary, TaskBudget, TaskLedger, TaskLedgerRecord, ToolContext,
    ToolRegistry, TurnResult,
};
use ncx_provider::DeepSeekProvider;
use ncx_sandbox::SandboxPolicy;
use serde_json::json;

use args::{parse_args, Args};
use runner::{LiveRunner, LiveSummarizer};

const SYSTEM_PROMPT: &str = "You are nanocodex, a precise coding agent. Use the provided tools \
    (read_file, apply_patch, update_plan) to inspect and edit the workspace. Prefer apply_patch \
    for edits. Keep responses concise.";

/// Injected into the system prompt under `--permission-mode plan`.
const PLAN_MODE_NOTE: &str = "You are in PLAN MODE. Do NOT modify files or run state-changing \
    commands — apply_patch is disabled and write/escalating shell commands are blocked. \
    Investigate (read files, run read-only commands) and produce a concrete plan for the user \
    to approve; make no changes.";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ncx: {e}\n");
            eprintln!("{}", args::USAGE);
            std::process::exit(2);
        }
    };

    if args.help {
        println!("{}", args::USAGE);
        return;
    }
    if args.version {
        println!("ncx {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Build a current-thread runtime: the loop and tools are `!Send` by design.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime builds");

    std::process::exit(rt.block_on(run(args)));
}

async fn run(args: Args) -> i32 {
    let workspace = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let overrides = Overrides {
        workspace: Some(workspace.clone()),
        model: args.model.clone(),
        sandbox_mode: args.sandbox.clone(),
        approval_policy: args.approval.clone(),
        max_iterations: args.max_iterations,
        max_tool_calls: args.max_tool_calls,
        context_edit_enabled: if args.disable_context_edit {
            Some(false)
        } else {
            None
        },
        context_edit_max_chars: args.context_edit_max_chars,
        context_edit_keep_recent_messages: args.context_edit_keep_recent_messages,
        context_edit_max_tool_result_chars: args.context_edit_max_tool_result_chars,
        context_edit_max_history_chars: args.context_edit_max_history_chars,
        context_edit_max_tool_result_total_chars: args.context_edit_max_tool_result_total_chars,
        profile: args.profile.clone(),
        ..Default::default()
    };

    let cfg = match load_config(overrides) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ncx: config error: {e}");
            return 1;
        }
    };
    if args.history {
        println!("{}", render_history(&SessionIndex::default().entries(), 20));
        return 0;
    }
    if let Err(e) = cfg.validate() {
        eprintln!("ncx: {e}");
        return 1;
    }
    if let Some(pm) = args.permission_mode.as_deref() {
        if !VALID_PERMISSION_MODES.contains(&pm) {
            eprintln!(
                "ncx: invalid --permission-mode {pm:?}; expected one of {VALID_PERMISSION_MODES:?}"
            );
            return 1;
        }
    }
    if args.budget_report {
        println!("{}", render_budget_report(&TaskLedger::new(&cfg.workspace), 20));
        return 0;
    }
    if args.tools_eval_report {
        println!("{}", render_tool_trace_eval(&TaskLedger::new(&cfg.workspace), 50));
        return 0;
    }

    // Maintenance: LLM-fold near-duplicate memory notes, then exit.
    if args.memory_merge {
        let mem = MemoryStore::new(cfg.workspace.join(".ncx").join("memory"));
        let summarizer = LiveSummarizer::new(cfg.clone());
        return match mem.summarize_consolidate(&summarizer, 0.85).await {
            Ok(n) => {
                println!("memory: folded {n} near-duplicate note(s) via the LLM.");
                0
            }
            Err(e) => {
                eprintln!("memory merge failed: {e}");
                1
            }
        };
    }

    let provider = DeepSeekProvider::with_opts(
        cfg.api_key.clone(),
        &cfg.base_url,
        cfg.model.clone(),
        cfg.timeout_s as u64,
        cfg.max_retries as u32,
    );
    // --permission-mode (when given) is the single source of gating, overriding
    // --sandbox/--approval; otherwise keep the classic sandbox+approval behavior.
    let (sandbox_mode, approval_policy, require_edit, plan_mode) =
        match args.permission_mode.as_deref() {
            Some(pm) => {
                let (s, a, r, p) = permission_mode_to_knobs(pm);
                (s.to_string(), a.to_string(), r, p)
            }
            None => (cfg.sandbox_mode.clone(), cfg.approval_policy.clone(), false, false),
        };
    let network = sandbox_mode == "danger-full-access";
    let policy = SandboxPolicy::new(sandbox_mode, &cfg.workspace).with_network_access(network);
    // Project memory: recalled per prompt by AgentLoop; the `remember`
    // tool lets the agent append verified notes (it gets smarter on THIS repo).
    let memory = Rc::new(MemoryStore::new(cfg.workspace.join(".ncx").join("memory")));
    // Periodic consolidation: fold near-duplicate notes on every start (cheap,
    // idempotent) so the store stays tidy as it grows.
    let _ = memory.consolidate(0.85);
    let recall_query = args.prompt.as_deref().unwrap_or("");
    let recall = memory.recall(recall_query, 8, 4000);
    let instructions = load_project_instructions(&cfg.workspace, 16_000);
    // Agent Skills: inject only the name+description index (progressive
    // disclosure); the `skill` tool loads a full SKILL.md body on demand.
    let skills = discover_skills(&cfg.workspace);
    let skills_index = skills_index_block(&skills);
    // Training-time harness overrides (NCX_GENOME). Empty/unset => no-op: the
    // base prompt stays SYSTEM_PROMPT and tool descriptions are untouched.
    let genome = Genome::from_env();
    if !genome.is_empty() {
        eprintln!(
            "[ncx] NCX_GENOME active: system_prompt={}, tool_desc overrides={}",
            genome.system_prompt.is_some(),
            genome.tool_desc.len()
        );
    }
    let base_prompt = genome.base_system_prompt(SYSTEM_PROMPT).to_string();
    let plan_note = if plan_mode {
        PLAN_MODE_NOTE.to_string()
    } else {
        String::new()
    };
    let system_prompt =
        compose_system_prompt(&base_prompt, &[instructions, recall, skills_index, plan_note]);
    let ctx = ToolContext::new(cfg.workspace.clone(), policy)
        .with_approval_policy(approval_policy)
        .with_require_edit_approval(require_edit)
        .with_plan_mode(plan_mode)
        .with_timeout(cfg.timeout_s as u64)
        .with_search(cfg.search_provider.clone(), cfg.search_api_key.clone())
        .with_memory(memory)
        .with_hooks(cfg.hooks.clone())
        .with_skills(skills)
        .with_genome(genome);
    let mut tools = ToolRegistry::new(ctx);
    // ncx-forge: emit the default harness genome (base prompt + core tool
    // descriptions) as TOML and exit. Done BEFORE MCP registration so the dump
    // contains only the evolvable core surface, not server-provided tools.
    if args.dump_genome {
        print!(
            "{}",
            dump_genome_toml(&base_prompt, &tools.ctx.tool_catalog.borrow())
        );
        return 0;
    }
    if args.mcp {
        let servers = load_mcp_servers();
        if servers.is_empty() {
            eprintln!("mcp: --mcp set but no enabled servers found in ~/.nanocodex/mcp.toml");
        }
        for srv in servers {
            let policy = mcp_tool_policy(&srv);
            match register_mcp_server_with_policy(
                &mut tools,
                &srv.name,
                &srv.command,
                &srv.args,
                &srv.env,
                &policy,
            )
            .await
            {
                Ok(n) => eprintln!("mcp({}): {} tool(s) registered", srv.name, n),
                Err(e) => eprintln!("mcp({}): connect failed: {e}", srv.name),
            }
        }
    }
    let log_path = session_log_path(&cfg.workspace);
    let session_id = new_session_id();
    let session = if args.resume {
        Session::resume(system_prompt, Some(log_path.clone()))
    } else {
        Session::with_log(system_prompt, Some(log_path.clone()))
    };
    let restored_count = session.restored_count;
    let mut agent = AgentLoop::new(Box::new(provider), tools, session)
        .with_task_budget(task_budget_from_config(&cfg))
        .with_context_edit(context_edit_from_config(&cfg))
        .with_vision_provider(build_vision_provider(&cfg));
    let mut recorder = SessionRecorder::new(session_id, cfg.workspace.clone(), log_path);

    if args.resume {
        if restored_count > 0 {
            eprintln!("resumed {restored_count} message(s) from the workspace session log.");
        } else {
            eprintln!("no previous workspace session log found; starting fresh.");
        }
    }

    // One-shot mode: run the prompt and exit.
    if let Some(prompt) = &args.prompt {
        let expanded = expand_file_mentions(prompt, &cfg.workspace);
        checkpoint_before_turn(&cfg.workspace, &expanded);
        if args.orchestrate {
            if !args.images.is_empty() {
                eprintln!("ncx: --image is ignored with --orchestrate (text-only path).");
            }
            return run_orchestrated(cfg, &expanded).await;
        }
        let user_input = match build_image_user_input(&expanded, &args.images) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("ncx: {e}");
                return 1;
            }
        };
        let approvals_before = agent.tools.ctx.approval_request_count();
        let started_at = ncx_core::task_ledger_now();
        let started = Instant::now();
        let result = agent.run_turn(user_input, None).await;
        let approval_requests = agent
            .tools
            .ctx
            .approval_request_count()
            .saturating_sub(approvals_before);
        record_task_ledger(
            &TaskLedger::new(&cfg.workspace),
            &recorder.session_id,
            &cfg.workspace,
            &cfg.model,
            agent.task_budget.clone(),
            &result,
            approval_requests,
            started_at,
            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        );
        recorder.record(&agent.session);
        println!("{}", result.final_text);
        // Emit a stable, parseable token-usage line on stderr so external tools
        // (e.g. the ncx-forge evaluator's Pareto cost axis) can read real token
        // cost rather than wall-clock. Always printed in one-shot mode.
        emit_usage_line(&result.usage);
        return if result.stop_reason == "error" { 1 } else { 0 };
    }

    repl(&mut agent, &cfg, &mut recorder).await;
    0
}

/// Run a single prompt through the tiered flash/pro orchestrator and print the
/// outcome (complexity, verify status, final text).
async fn run_orchestrated(cfg: Config, prompt: &str) -> i32 {
    let fast = if cfg.fast_model.is_empty() {
        cfg.model.clone()
    } else {
        cfg.fast_model.clone()
    };
    eprintln!("[orchestrator] main={}  fast={}", cfg.model, fast);
    let runner = LiveRunner::new(cfg);
    let orch = Orchestrator::new(&runner, OrchestratorConfig::default());
    let outcome = orch.handle(prompt).await;
    let (model_remaining, tool_remaining) = runner.budget_remaining();
    eprintln!(
        "[orchestrator] complexity={:?}  verify={}  rounds={}  best_worker={}  budget_remaining=model:{} tool:{}",
        outcome.complexity,
        if outcome.verify_passed {
            "PASS"
        } else {
            "UNVERIFIED"
        },
        outcome.verify_rounds,
        outcome.best_worker,
        model_remaining,
        tool_remaining,
    );
    println!("{}", outcome.final_text);
    if outcome.verify_passed {
        0
    } else {
        1
    }
}

/// Emit the default harness genome as TOML for the ncx-forge trainer: the base
/// system prompt + each registered (core) tool's description. Single-line basic
/// strings with `\n`/`\"` escapes so it round-trips through any TOML parser.
fn dump_genome_toml(system_prompt: &str, catalog: &[ncx_core::tools::ToolCatalogEntry]) -> String {
    let mut out = String::new();
    out.push_str("# Default nanocodex harness genome (ncx --dump-genome).\n");
    out.push_str("# Edit system_prompt and tool_desc.* to evolve the agent.\n\n");
    out.push_str(&format!(
        "system_prompt = \"{}\"\n\n",
        toml_escape(system_prompt)
    ));
    out.push_str("[tool_desc]\n");
    for entry in catalog {
        out.push_str(&format!(
            "{} = \"{}\"\n",
            entry.name,
            toml_escape(&entry.description)
        ));
    }
    out
}

/// Escape a string for a TOML single-line basic string (the content between the
/// surrounding quotes): backslash, double-quote, and the common control chars.
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn compose_system_prompt(base: &str, blocks: &[String]) -> String {
    let mut out = base.to_string();
    for block in blocks {
        if !block.trim().is_empty() {
            out.push_str("\n\n");
            out.push_str(block.trim());
        }
    }
    out
}

/// Interactive REPL. Slash commands are dispatched without a model call; any
/// other line becomes a turn (with `@file` mention expansion).
async fn repl(agent: &mut AgentLoop, cfg: &ncx_config::Config, recorder: &mut SessionRecorder) {
    println!(
        "nanocodex (ncx) — model {}, sandbox {}. /help for commands, /exit to quit. \
         (attach images inline: `--image <path> your question`)",
        cfg.model, cfg.sandbox_mode
    );
    let stdin = io::stdin();
    let mut usage = UsageTracker::default();
    loop {
        print!("\n› ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let line = line.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }

        let (cmd, arg) = parse_slash(line);
        if let Some(cmd) = cmd {
            match dispatch_slash(&cmd, &arg, agent, cfg, recorder, &usage) {
                SlashOutcome::Exit => break,
                SlashOutcome::Printed(text) => println!("{text}"),
                SlashOutcome::Prompt(text) => {
                    run_one_turn(agent, &text, cfg, recorder, &mut usage).await
                }
            }
            continue;
        }

        run_one_turn(agent, line, cfg, recorder, &mut usage).await;
    }
    println!("bye.");
}

async fn run_one_turn(
    agent: &mut AgentLoop,
    prompt: &str,
    cfg: &ncx_config::Config,
    recorder: &mut SessionRecorder,
    usage: &mut UsageTracker,
) {
    // Inline `--image <path>` tokens attach images (vision turn); the rest is text.
    let (text, images) = split_inline_images(prompt);
    let expanded = expand_file_mentions(&text, &cfg.workspace);
    checkpoint_before_turn(&cfg.workspace, &expanded);
    let user_input = match build_image_user_input(&expanded, &images) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ncx: {e}");
            return;
        }
    };
    let approvals_before = agent.tools.ctx.approval_request_count();
    let started_at = ncx_core::task_ledger_now();
    let started = Instant::now();
    let result = agent.run_turn(user_input, None).await;
    let approval_requests = agent
        .tools
        .ctx
        .approval_request_count()
        .saturating_sub(approvals_before);
    record_task_ledger(
        &TaskLedger::new(&cfg.workspace),
        &recorder.session_id,
        &cfg.workspace,
        &cfg.model,
        agent.task_budget.clone(),
        &result,
        approval_requests,
        started_at,
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    );
    recorder.record(&agent.session);
    usage.record(&result);
    println!("{}", result.final_text);
}

/// Pull inline `--image <path>` pairs out of a REPL line, returning the
/// remaining prompt text and the collected image paths (mirrors the one-shot
/// `--image` flag so the REPL can also drive vision turns).
fn split_inline_images(line: &str) -> (String, Vec<PathBuf>) {
    let mut images = Vec::new();
    let mut words = Vec::new();
    let mut it = line.split_whitespace();
    while let Some(w) = it.next() {
        if w == "--image" {
            if let Some(p) = it.next() {
                images.push(PathBuf::from(p));
            }
        } else {
            words.push(w);
        }
    }
    (words.join(" "), images)
}

enum SlashOutcome {
    Exit,
    Printed(String),
    Prompt(String),
}

/// Handle a slash command that doesn't require a model call. Returns the text to
/// print, an exit signal, or (for unknown commands) treats the line as a prompt.
fn dispatch_slash(
    cmd: &str,
    arg: &str,
    agent: &mut AgentLoop,
    cfg: &ncx_config::Config,
    recorder: &mut SessionRecorder,
    usage: &UsageTracker,
) -> SlashOutcome {
    match cmd {
        "/exit" => SlashOutcome::Exit,
        "/help" => SlashOutcome::Printed(render_help_for_workspace(&cfg.workspace)),
        "/status" => SlashOutcome::Printed(render_status(cfg)),
        "/usage" | "/cost" => SlashOutcome::Printed(usage.render()),
        "/budget" => {
            if arg.trim().starts_with("report") || arg.trim().starts_with("ledger") {
                SlashOutcome::Printed(render_budget_report(
                    &TaskLedger::new(&cfg.workspace),
                    budget_report_limit(arg),
                ))
            } else {
                SlashOutcome::Printed(render_budget_status(agent, cfg, usage))
            }
        }
        "/context" => {
            if arg.trim().starts_with("payload") || arg.trim().starts_with("snapshot") {
                SlashOutcome::Printed(render_context_payload_report(
                    &cfg.workspace,
                    context_payload_report_limit(arg),
                ))
            } else {
                SlashOutcome::Printed(render_context_status(agent, cfg, usage))
            }
        }
        "/config" => SlashOutcome::Printed(config_text(cfg, arg)),
        "/history" => SlashOutcome::Printed(render_history(&SessionIndex::default().entries(), 20)),
        "/checkpoint" => SlashOutcome::Printed(create_checkpoint_text(&cfg.workspace, arg)),
        "/checkpoints" => SlashOutcome::Printed(render_checkpoints(
            &CheckpointStore::new(&cfg.workspace).list(),
            20,
        )),
        "/restore" => SlashOutcome::Printed(restore_checkpoint_text(&cfg.workspace, arg)),
        "/compact" => SlashOutcome::Printed(compact_session_text(agent, recorder)),
        "/model" => {
            if arg.is_empty() {
                SlashOutcome::Printed(format!("model: {}", cfg.model))
            } else {
                SlashOutcome::Printed(format!(
                    "(model switch requires restart in this build; current: {})",
                    cfg.model
                ))
            }
        }
        "/skills" => SlashOutcome::Printed(render_skills(&agent.tools.ctx.skills)),
        "/memory" => SlashOutcome::Printed(handle_memory_command(
            agent.tools.ctx.memory.as_deref(),
            arg,
            &cfg.workspace,
        )),
        "/tools" => {
            if arg.trim().starts_with("eval") || arg.trim().starts_with("trace") {
                SlashOutcome::Printed(render_tool_trace_eval(
                    &TaskLedger::new(&cfg.workspace),
                    tool_trace_eval_limit(arg),
                ))
            } else {
                SlashOutcome::Printed(render_tools_status(&agent.tools, arg))
            }
        }
        "/mcp" => {
            let servers = load_mcp_servers();
            let connectors = load_mcp_connectors();
            let catalog = agent.tools.ctx.tool_catalog.borrow();
            SlashOutcome::Printed(render_mcp_status(&servers, &connectors, &catalog))
        }
        "/plan" => {
            let plan = agent.tools.ctx.plan.borrow();
            if plan.is_empty() {
                SlashOutcome::Printed("(no plan yet)".into())
            } else {
                let mut out = String::from("Plan:");
                for step in plan.iter() {
                    let s = step.get("step").and_then(|v| v.as_str()).unwrap_or("?");
                    let st = step.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    out.push_str(&format!("\n  [{st}] {s}"));
                }
                SlashOutcome::Printed(out)
            }
        }
        other if is_known(other) => {
            SlashOutcome::Printed(format!("({other} is not available in this build yet)"))
        }
        other => match custom_command_prompt(&cfg.workspace, other, arg) {
            Ok(Some(prompt)) => SlashOutcome::Prompt(prompt),
            Ok(None) => SlashOutcome::Printed(format!("Unknown command {other}. Try /help.")),
            Err(e) => SlashOutcome::Printed(format!("Custom command failed: {e}")),
        },
    }
}

fn render_skills(skills: &[ncx_core::Skill]) -> String {
    if skills.is_empty() {
        return "(no skills available — add SKILL.md dirs under .ncx/skills/)".into();
    }
    let mut out = format!("Available skills ({}):", skills.len());
    for s in skills {
        let tag = if s.is_builtin() { " [builtin]" } else { "" };
        if s.description.is_empty() {
            out.push_str(&format!("\n  {}{tag}", s.name));
        } else {
            out.push_str(&format!("\n  {}{tag}\n      {}", s.name, s.description));
        }
    }
    out.push_str("\n\nThe agent loads a skill's full instructions on demand via the `skill` tool.");
    out
}

fn handle_memory_command(memory: Option<&MemoryStore>, arg: &str, workspace: &Path) -> String {
    let Some(memory) = memory else {
        return "Project memory is not enabled in this runtime.".into();
    };
    let arg = arg.trim();
    if arg == "index" || arg == "reindex" {
        return match memory.rebuild_vector_index() {
            Ok(count) => format!(
                "Rebuilt memory vector index with {count} entr{} at {}.",
                if count == 1 { "y" } else { "ies" },
                memory.index_path().display()
            ),
            Err(e) => format!("Error rebuilding memory vector index: {e}"),
        };
    }
    if arg == "harvest" || arg.starts_with("harvest ") {
        let raw = arg.strip_prefix("harvest").unwrap_or("").trim();
        let paths = if raw.is_empty() {
            default_memory_harvest_paths(workspace)
        } else {
            vec![resolve_workspace_path(workspace, raw)]
        };
        if paths.is_empty() {
            return "No handoff/release documents found to harvest.".into();
        }
        let mut out = String::from("Memory proposal harvest");
        let mut total = 0usize;
        let mut now = epoch_seconds();
        for path in paths {
            let source = path
                .strip_prefix(workspace)
                .ok()
                .and_then(|p| p.to_str())
                .unwrap_or_else(|| path.to_str().unwrap_or("document"))
                .replace('\\', "/");
            match std::fs::read_to_string(&path) {
                Ok(text) => match memory.harvest_proposals_from_text(&source, &text, now) {
                    Ok(created) => {
                        total += created.len();
                        now += created.len() as u64 + 1;
                        out.push_str(&format!("\n- {source}: queued {}", created.len()));
                    }
                    Err(e) => out.push_str(&format!("\n- {source}: error {e}")),
                },
                Err(e) => out.push_str(&format!("\n- {source}: read failed {e}")),
            }
        }
        out.push_str(&format!("\nqueued_total: {total}"));
        out.push_str("\nReview with /memory accept <id> or /memory reject <id>.");
        return out;
    }
    if arg == "accept-all" {
        return match memory.accept_all_proposals(epoch_seconds()) {
            Ok(count) => format!("Accepted {count} memory proposals."),
            Err(e) => format!("Error accepting memory proposals: {e}"),
        };
    }
    if arg == "reject-all" {
        return match memory.reject_all_proposals() {
            Ok(count) => format!("Rejected {count} memory proposals."),
            Err(e) => format!("Error rejecting memory proposals: {e}"),
        };
    }
    if let Some(rest) = arg.strip_prefix("edit ") {
        let rest = rest.trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let id = parts.next().unwrap_or("").trim();
        let note = parts.next().unwrap_or("").trim();
        if id.is_empty() || note.is_empty() {
            return "Usage: /memory edit <proposal-id> <new-note>".into();
        }
        let tags = memory
            .proposals()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.tags)
            .unwrap_or_default();
        return match memory.update_proposal(id, note, &tags) {
            Ok(true) => format!("Updated memory proposal {id}."),
            Ok(false) => format!("No editable memory proposal found for {id}, or the new note duplicates existing memory."),
            Err(e) => format!("Error updating memory proposal {id}: {e}"),
        };
    }
    if let Some(id) = arg.strip_prefix("accept ") {
        let id = id.trim();
        if id.is_empty() {
            return "Usage: /memory accept <proposal-id>".into();
        }
        return match memory.accept_proposal(id, epoch_seconds()) {
            Ok(true) => format!("Accepted memory proposal {id}."),
            Ok(false) => format!("No pending memory proposal found for {id}."),
            Err(e) => format!("Error accepting memory proposal {id}: {e}"),
        };
    }
    if let Some(id) = arg.strip_prefix("reject ") {
        let id = id.trim();
        if id.is_empty() {
            return "Usage: /memory reject <proposal-id>".into();
        }
        return match memory.reject_proposal(id) {
            Ok(true) => format!("Rejected memory proposal {id}."),
            Ok(false) => format!("No pending memory proposal found for {id}."),
            Err(e) => format!("Error rejecting memory proposal {id}: {e}"),
        };
    }
    if let Some(note) = arg.strip_prefix("propose ") {
        let note = note.trim();
        if note.is_empty() {
            return "Usage: /memory propose <candidate-note>".into();
        }
        return match memory.propose(note, &[], "cli", epoch_seconds()) {
            Ok(Some(p)) => format!("Queued memory proposal {}.", p.id),
            Ok(None) => "Already trusted/pending in project memory (or empty).".into(),
            Err(e) => format!("Error queuing memory proposal: {e}"),
        };
    }
    render_memory_status(Some(memory), arg)
}

fn default_memory_harvest_paths(workspace: &Path) -> Vec<PathBuf> {
    [
        "HANDOFF.md",
        "RELEASE_TASK.md",
        "docs/release-checklist.md",
        "docs/claude-fable-gap-roadmap.zh-CN.md",
    ]
    .iter()
    .map(|p| workspace.join(p))
    .filter(|p| p.exists())
    .collect()
}

fn resolve_workspace_path(workspace: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn render_memory_status(memory: Option<&MemoryStore>, query: &str) -> String {
    let Some(memory) = memory else {
        return "Project memory is not enabled in this runtime.".into();
    };
    let query = query.trim();
    let mut entries = memory.entries();
    entries.sort_by(|a, b| b.ts.cmp(&a.ts));
    let mut proposals = memory.proposals();
    proposals.sort_by(|a, b| b.ts.cmp(&a.ts));

    let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();
    for entry in &entries {
        for tag in &entry.tags {
            *tag_counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut tags = tag_counts.into_iter().collect::<Vec<_>>();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let tag_line = if tags.is_empty() {
        "(none)".into()
    } else {
        tags.into_iter()
            .take(8)
            .map(|(tag, count)| format!("{tag}={count}"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut out = format!(
        "Project memory\npath: {}\nproposals_path: {}\nindex_path: {}\nentries: {}\npending_proposals: {}\nmax_entries: {}\nmax_proposals: {}\nvector_dims: {}\ntop_tags: {}",
        memory.path().display(),
        memory.proposal_path().display(),
        memory.index_path().display(),
        entries.len(),
        proposals.len(),
        ncx_core::memory::MAX_ENTRIES,
        ncx_core::memory::MAX_PROPOSALS,
        ncx_core::memory::VECTOR_DIMS,
        tag_line
    );
    out.push_str("\n\nRecent notes:");
    if entries.is_empty() {
        out.push_str(" (none)");
    } else {
        for entry in entries.iter().take(5) {
            out.push_str(&format!(
                "\n- [{}] {}",
                entry.ts,
                truncate_one_line(&entry.text, 120)
            ));
        }
    }
    out.push_str("\n\nPending proposals:");
    if proposals.is_empty() {
        out.push_str(" (none)");
    } else {
        for proposal in proposals.iter().take(5) {
            out.push_str(&format!(
                "\n- [{} @ {} {}] {}",
                proposal.id,
                proposal.ts,
                proposal.source,
                truncate_one_line(&proposal.text, 120)
            ));
        }
        out.push_str("\nUse /memory accept <id> or /memory reject <id> to review proposals.");
    }

    if query.is_empty() {
        out.push_str(
            "\n\nUse /memory <query> to preview recall, /memory index, /memory propose <note>, /memory harvest [path], /memory edit <id> <note>, /memory accept-all, or /memory reject-all.",
        );
    } else {
        let recall = memory.recall(query, 5, 2_000);
        out.push_str(&format!("\n\nRecall preview for '{query}':"));
        if recall.trim().is_empty() {
            out.push_str(" (none)");
        } else {
            out.push('\n');
            out.push_str(&recall);
        }
    }
    out
}

fn truncate_one_line(text: &str, max_chars: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for (i, ch) in one_line.chars().enumerate() {
        if i >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn render_tools_status(tools: &ToolRegistry, query: &str) -> String {
    let query = query.trim();
    let catalog = tools.ctx.tool_catalog.borrow();
    let read_only = catalog.iter().filter(|entry| entry.read_only).count();
    let write_or_effect = catalog.len().saturating_sub(read_only);
    let hints = tools.ctx.tool_hints.borrow().clone();
    let visible = tools
        .schemas_for_query(query)
        .iter()
        .filter_map(schema_tool_name)
        .collect::<Vec<_>>();

    let mut out = format!(
        "Tool catalog\nregistered: {}\nread_only: {}\nwrite_or_effect: {}\nvisible_now: {}",
        catalog.len(),
        read_only,
        write_or_effect,
        visible.len()
    );
    if !query.is_empty() {
        out.push_str(&format!("\nquery: {query}"));
    }
    out.push_str("\n\nVisible tools:");
    out.push_str(&format_name_list(&visible));
    out.push_str("\n\nTool search hints:");
    out.push_str(&format_name_list(&hints));
    out.push_str(
        "\n\nUse /tools <query> to preview the schema view for a prompt, or call \
         tool_search from the model loop to pin matching tools for the next turn.",
    );
    out
}

fn schema_tool_name(schema: &serde_json::Value) -> Option<String> {
    schema
        .get("function")
        .and_then(|f| f.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn format_name_list(names: &[String]) -> String {
    if names.is_empty() {
        " (none)".into()
    } else {
        let mut sorted = names.to_vec();
        sorted.sort();
        format!(" {}", sorted.join(", "))
    }
}

fn render_mcp_status(
    servers: &[ncx_config::McpServerConfig],
    connectors: &[ncx_config::McpConnectorConfig],
    catalog: &[ncx_core::tools::ToolCatalogEntry],
) -> String {
    let mut out = format!(
        "MCP enabled servers in ~/.nanocodex/mcp.toml: {}",
        servers.len()
    );
    if servers.is_empty() {
        out.push_str("\n  (none)");
    } else {
        for server in servers {
            let args = if server.args.is_empty() {
                String::new()
            } else {
                format!(" {}", server.args.join(" "))
            };
            let allowed = if server.allowed_tools.is_empty() {
                "*".to_string()
            } else {
                server.allowed_tools.join(",")
            };
            out.push_str(&format!(
                "\n  {}: {}{} permission={} trusted={} allowed_tools={}",
                server.name, server.command, args, server.permission, server.trusted, allowed
            ));
        }
    }

    out.push_str(&format!(
        "\n\nConnector install specs in ~/.nanocodex/connectors.toml: {}",
        connectors.len()
    ));
    if connectors.is_empty() {
        out.push_str("\n  (none)");
    } else {
        for connector in connectors {
            let allowed = if connector.allowed_tools.is_empty() {
                "*".to_string()
            } else {
                connector.allowed_tools.join(",")
            };
            let endpoint = connector_endpoint_summary(connector);
            let source = if connector.source.trim().is_empty() {
                "-"
            } else {
                connector.source.trim()
            };
            let auth = connector_auth_summary(connector);
            out.push_str(&format!(
                "\n  {}: transport={} enabled={} launch={} launch_gap={} trusted={} permission={} allowed_tools={} source={} auth={} endpoint={}",
                connector.name,
                connector.transport,
                connector.enabled,
                connector.launch_status(),
                connector.launch_gap_reason(),
                connector.trusted,
                connector.permission,
                allowed,
                source,
                auth,
                endpoint
            ));
        }
    }

    let mut tools_by_server: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in catalog {
        let Some((server, tool)) = mcp_tool_name_parts(&entry.name) else {
            continue;
        };
        tools_by_server
            .entry(server.to_string())
            .or_default()
            .push(tool.to_string());
    }

    let total_tools: usize = tools_by_server.values().map(Vec::len).sum();
    out.push_str(&format!("\n\nRegistered MCP tools: {total_tools}"));
    if total_tools == 0 {
        out.push_str("\n  (none; start the REPL with --mcp and check startup errors)");
        return out;
    }
    for (server, mut tools) in tools_by_server {
        tools.sort();
        out.push_str(&format!(
            "\n  {server} ({}): {}",
            tools.len(),
            tools.join(", ")
        ));
    }
    out
}

fn connector_endpoint_summary(connector: &ncx_config::McpConnectorConfig) -> String {
    if connector.transport == "stdio" {
        return format!("{} {}", connector.command, connector.args.join(" "))
            .trim()
            .to_string();
    }
    redact_remote_endpoint(&connector.url)
}

fn redact_remote_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    let (before_fragment, fragment) = match trimmed.split_once('#') {
        Some((base, _)) => (base, true),
        None => (trimmed, false),
    };
    let mut redacted = match before_fragment.split_once('?') {
        Some((base, _)) => format!("{base}?<redacted>"),
        None => before_fragment.to_string(),
    };
    if let Some(scheme_end) = redacted.find("://") {
        let authority_start = scheme_end + 3;
        let authority_end = redacted[authority_start..]
            .find('/')
            .map(|i| authority_start + i)
            .unwrap_or(redacted.len());
        if let Some(at_rel) = redacted[authority_start..authority_end].rfind('@') {
            redacted.replace_range(authority_start..authority_start + at_rel, "<redacted>");
        }
    }
    if fragment {
        redacted.push_str("#<redacted>");
    }
    redacted
}

fn connector_auth_summary(connector: &ncx_config::McpConnectorConfig) -> String {
    let mut parts = vec![connector.auth.clone()];
    if !connector.headers.is_empty() {
        parts.push(format!("headers={}", connector.headers.len()));
    }
    if !connector.headers_helper.trim().is_empty() {
        parts.push("headers_helper=set".into());
    }
    if connector.oauth.has_metadata() {
        parts.push(format!(
            "oauth_client_id={}",
            if connector.oauth.client_id.trim().is_empty() {
                "missing"
            } else {
                "set"
            }
        ));
        parts.push(format!(
            "oauth_secret_env={}",
            if connector.oauth.client_secret_env.trim().is_empty() {
                "missing"
            } else {
                "set"
            }
        ));
        if let Some(port) = connector.oauth.callback_port {
            parts.push(format!("callback_port={port}"));
        }
        if !connector.oauth.scopes.is_empty() {
            parts.push(format!("scopes={}", connector.oauth.scopes.len()));
        }
        if !connector.oauth.auth_server_metadata_url.trim().is_empty() {
            parts.push("metadata_url=set".into());
        }
    }
    parts.join(";")
}

fn mcp_tool_policy(server: &ncx_config::McpServerConfig) -> McpToolPolicy {
    McpToolPolicy {
        allowed_tools: server.allowed_tools.clone(),
        trusted: server.trusted,
        permission: server.permission.clone(),
    }
}

fn mcp_tool_name_parts(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

fn record_task_ledger(
    ledger: &TaskLedger,
    session_id: &str,
    workspace: &Path,
    model: &str,
    budget: TaskBudget,
    result: &TurnResult,
    approval_requests: usize,
    started_at: String,
    duration_ms: u64,
) {
    let record = TaskLedgerRecord {
        session_id: session_id.to_string(),
        workspace: workspace.display().to_string(),
        model: model.to_string(),
        started_at,
        duration_ms,
        model_calls: result.iterations,
        tool_calls: result.tools_used.len(),
        visible_tools: result.visible_tools.clone(),
        called_tools: result.tools_used.clone(),
        approval_requests,
        stop_reason: result.stop_reason.clone(),
        task_model_budget: budget.max_model_calls,
        task_tool_budget: budget.max_tool_calls,
        usage: result.usage.clone(),
    };
    if let Err(e) = ledger.append(&record) {
        eprintln!(
            "ncx: could not write task ledger {}: {e}",
            ledger.path().display()
        );
    }
}

fn budget_report_limit(arg: &str) -> usize {
    arg.split_whitespace()
        .filter_map(|part| part.parse::<usize>().ok())
        .next()
        .unwrap_or(20)
        .clamp(1, 200)
}

fn render_budget_report(ledger: &TaskLedger, limit: usize) -> String {
    ledger.render_report(limit)
}

fn tool_trace_eval_limit(arg: &str) -> usize {
    arg.split_whitespace()
        .filter_map(|part| part.parse::<usize>().ok())
        .next()
        .unwrap_or(50)
        .clamp(1, 200)
}

fn render_tool_trace_eval(ledger: &TaskLedger, limit: usize) -> String {
    ledger.render_tool_trace_eval(limit)
}

fn render_help() -> String {
    let mut out = String::from("Commands:");
    for (cmd, help) in SLASH_HELP {
        out.push_str(&format!("\n  {cmd:<12} {help}"));
    }
    out
}

fn render_help_for_workspace(workspace: &Path) -> String {
    let mut out = render_help();
    let custom = list_custom_commands(workspace);
    if !custom.is_empty() {
        out.push_str("\n\nCustom commands:");
        for cmd in custom {
            out.push_str(&format!(
                "\n  /{}:{:<10} {}",
                cmd.scope,
                cmd.name,
                cmd.path.display()
            ));
        }
        out.push_str("\n  /<name>       Runs project commands before user commands.");
    }
    out
}

fn render_status(cfg: &ncx_config::Config) -> String {
    let red = cfg.redacted();
    format!(
        "model:     {}\nbase_url:  {}\nsandbox:   {}\napproval:  {}\nworkspace: {}\napi_key:   {}\nmodel_budget: {}  tool_budget: {}  retries: {}\ncontext_edit: {}  max_chars: {}  keep_recent: {}  tool_result_chars: {}  history_chars: {}  tool_total_chars: {}\nhooks:     {}",
        cfg.model,
        cfg.base_url,
        cfg.sandbox_mode,
        cfg.approval_policy,
        cfg.workspace.display(),
        red.get("api_key").cloned().unwrap_or_default(),
        cfg.max_iterations,
        cfg.max_tool_calls,
        cfg.max_retries,
        cfg.context_edit_enabled,
        cfg.context_edit_max_chars,
        cfg.context_edit_keep_recent_messages,
        cfg.context_edit_max_tool_result_chars,
        cfg.context_edit_max_history_chars,
        cfg.context_edit_max_tool_result_total_chars,
        cfg.hooks.len(),
    )
}

fn render_budget_status(
    agent: &AgentLoop,
    cfg: &ncx_config::Config,
    usage: &UsageTracker,
) -> String {
    let model_limit = agent.task_budget.max_model_calls;
    let tool_limit = agent.task_budget.max_tool_calls;
    let last_block = usage
        .last
        .as_ref()
        .map(|last| {
            let model_remaining = model_limit.saturating_sub(last.model_calls);
            let tool_remaining = tool_limit.saturating_sub(last.tool_calls);
            format!(
                "model_calls: {}  remaining: {}\ntool_calls:  {}  remaining: {}\nstop_reason: {}",
                last.model_calls,
                model_remaining,
                last.tool_calls,
                tool_remaining,
                last.stop_reason
            )
        })
        .unwrap_or_else(|| "No model turn recorded yet.".into());

    format!(
        "Task budget\nper_task_model_calls: {}\nper_task_tool_calls: {}\ncontext_edit_max_chars: {}\ncontext_token_budget: {}\nconfig_max_iterations: {}\nconfig_max_tool_calls: {}\nledger: {}\n\nSession use\nmodel_calls: {}\ntool_calls:  {}\n\nLast turn vs budget\n{}\n\nUse /budget report [N] to inspect recent completed task records.",
        model_limit,
        tool_limit,
        agent.context_edit.max_chars,
        cfg.context_token_budget,
        cfg.max_iterations,
        cfg.max_tool_calls,
        TaskLedger::new(&cfg.workspace).path().display(),
        usage.total_model_calls,
        usage.total_tool_calls,
        last_block
    )
}

fn render_context_status(
    agent: &AgentLoop,
    cfg: &ncx_config::Config,
    usage: &UsageTracker,
) -> String {
    let preview = agent
        .session
        .for_model_edited(&[], &agent.context_edit)
        .stats;
    let log_path = agent
        .session
        .log_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(not logging)".into());
    let last = usage
        .last
        .as_ref()
        .map(|last| format_context_edit_stats_block(&last.context_edit))
        .unwrap_or_else(|| "No model turn recorded yet.".into());

    let snapshot_dir = ContextPayloadSnapshotStore::new(&cfg.workspace)
        .root()
        .display()
        .to_string();

    format!(
        "Context editing\nenabled: {}\nmax_chars: {}\nkeep_recent_messages: {}\nmax_tool_result_chars: {}\nmax_history_chars: {}\nmax_tool_result_total_chars: {}\ncontext_token_budget: {}\npayload_snapshots: {}\n\nSession\nmessages: {}\nrestored_messages: {}\nlog: {}\n\nNext send preview\n{}\n\nLast turn context edit\n{}\n\nUse /context payload [N] to inspect recent provider payload snapshots.",
        agent.context_edit.enabled,
        agent.context_edit.max_chars,
        agent.context_edit.keep_recent_messages,
        agent.context_edit.max_tool_result_chars,
        agent.context_edit.max_history_chars,
        agent.context_edit.max_tool_result_total_chars,
        cfg.context_token_budget,
        snapshot_dir,
        agent.session.messages.len(),
        agent.session.restored_count,
        log_path,
        format_context_edit_stats_block(&preview),
        last
    )
}

fn context_payload_report_limit(arg: &str) -> usize {
    arg.split_whitespace()
        .filter_map(|part| part.parse::<usize>().ok())
        .next()
        .unwrap_or(10)
        .clamp(1, 50)
}

fn render_context_payload_report(workspace: &Path, limit: usize) -> String {
    ContextPayloadSnapshotStore::new(workspace).render_report(limit)
}

fn config_text(cfg: &ncx_config::Config, arg: &str) -> String {
    let path = ConfigPaths::default().nanocodex;
    config_text_at(cfg, arg, &path)
}

fn config_text_at(cfg: &ncx_config::Config, arg: &str, path: &Path) -> String {
    let arg = arg.trim();
    if arg.is_empty() {
        return render_config_overview(cfg, path);
    }

    let (key, value) = match parse_config_assignment(arg) {
        Ok(pair) => pair,
        Err(e) => return format!("usage: /config key=value\n{e}"),
    };
    if !WRITABLE_KEYS.contains(&key.as_str()) {
        return format!(
            "Unknown writable config key: {key}\nWritable keys: {}",
            WRITABLE_KEYS.join(", ")
        );
    }

    let mut updates: HashMap<&str, &str> = HashMap::new();
    updates.insert(key.as_str(), value.as_str());
    match write_nanocodex_config(&updates, path) {
        Ok(()) => {
            let shown = if key.contains("key") {
                "<redacted>"
            } else {
                value.as_str()
            };
            format!(
                "Saved config: {key} = {shown}\npath: {}\nRestart the REPL for provider, model, sandbox, or budget changes to affect the active session.",
                path.display()
            )
        }
        Err(e) => format!("config write failed: {e}"),
    }
}

fn render_config_overview(cfg: &ncx_config::Config, path: &Path) -> String {
    let red = cfg.redacted();
    format!(
        "config path: {}\nmodel:     {}\nbase_url:  {}\nsandbox:   {}\napproval:  {}\napi_key:   {}\nwritable keys: {}",
        path.display(),
        cfg.model,
        cfg.base_url,
        cfg.sandbox_mode,
        cfg.approval_policy,
        red.get("api_key").cloned().unwrap_or_default(),
        WRITABLE_KEYS.join(", ")
    )
}

fn parse_config_assignment(arg: &str) -> Result<(String, String), String> {
    let Some((key, value)) = arg.split_once('=') else {
        return Err("missing '='; example: /config model=deepseek-chat".into());
    };
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() {
        return Err("config key is empty".into());
    }
    if key.chars().any(char::is_whitespace) {
        return Err("config key cannot contain whitespace".into());
    }
    if value.is_empty() {
        return Err("config value is empty".into());
    }
    Ok((key.to_string(), value.to_string()))
}

#[derive(Debug, Default, Clone)]
struct UsageTracker {
    last: Option<TurnUsage>,
    total: BTreeMap<String, i64>,
    total_model_calls: usize,
    total_tool_calls: usize,
    total_compressed_tool_results: usize,
    total_dropped_messages: usize,
    total_summary_checkpoints: usize,
}

#[derive(Debug, Clone)]
struct TurnUsage {
    usage: BTreeMap<String, i64>,
    model_calls: usize,
    tool_calls: usize,
    stop_reason: String,
    context_edit: ContextEditStats,
}

impl UsageTracker {
    fn record(&mut self, result: &TurnResult) {
        self.total_model_calls += result.iterations;
        self.total_tool_calls += result.tools_used.len();
        self.total_compressed_tool_results += result.context_edit.compressed_tool_results;
        self.total_dropped_messages += result.context_edit.dropped_messages;
        self.total_summary_checkpoints += result.context_edit.summary_checkpoints;
        add_usage(&mut self.total, &result.usage);
        self.last = Some(TurnUsage {
            usage: result.usage.clone(),
            model_calls: result.iterations,
            tool_calls: result.tools_used.len(),
            stop_reason: result.stop_reason.clone(),
            context_edit: result.context_edit.clone(),
        });
    }

    fn render(&self) -> String {
        let Some(last) = &self.last else {
            return "No token usage recorded yet.".into();
        };
        format!(
            "Last turn:\n{}\n\nContext edit:\n{}\n\nSession total:\n{}\n\nCost: raw token usage only; no Rust price table is configured.",
            format_usage_block(
                last.model_calls,
                last.tool_calls,
                Some(&last.stop_reason),
                &last.usage
            ),
            format_context_edit_block(
                &last.context_edit,
                self.total_compressed_tool_results,
                self.total_dropped_messages,
                self.total_summary_checkpoints
            ),
            format_usage_block(
                self.total_model_calls,
                self.total_tool_calls,
                None,
                &self.total
            )
        )
    }
}

fn add_usage(total: &mut BTreeMap<String, i64>, usage: &BTreeMap<String, i64>) {
    for (key, value) in usage {
        *total.entry(key.clone()).or_insert(0) += *value;
    }
}

fn format_usage_block(
    model_calls: usize,
    tool_calls: usize,
    stop_reason: Option<&str>,
    usage: &BTreeMap<String, i64>,
) -> String {
    let prompt = usage_value(usage, "prompt_tokens");
    let completion = usage_value(usage, "completion_tokens");
    let hit = usage_value(usage, "prompt_cache_hit_tokens");
    let miss = usage_value(usage, "prompt_cache_miss_tokens");
    let total = prompt + completion;
    let mut lines = vec![
        format!("model_calls: {model_calls}"),
        format!("tool_calls:  {tool_calls}"),
    ];
    if let Some(reason) = stop_reason {
        lines.push(format!("stop_reason: {reason}"));
    }
    lines.push(format!("prompt_tokens:     {prompt}"));
    lines.push(format!("completion_tokens: {completion}"));
    lines.push(format!("total_tokens:      {total}"));
    if hit > 0 || miss > 0 {
        lines.push(format!("prompt_cache_hit_tokens:  {hit}"));
        lines.push(format!("prompt_cache_miss_tokens: {miss}"));
    }
    lines.join("\n")
}

fn usage_value(usage: &BTreeMap<String, i64>, key: &str) -> i64 {
    usage.get(key).copied().unwrap_or(0)
}

fn format_context_edit_block(
    last: &ContextEditStats,
    session_compressed_tool_results: usize,
    session_dropped_messages: usize,
    session_summary_checkpoints: usize,
) -> String {
    format!(
        "{}\nsession_compressed:      {session_compressed_tool_results}\nsession_dropped:         {session_dropped_messages}\nsession_summaries:       {session_summary_checkpoints}",
        format_context_edit_stats_block(last)
    )
}

fn format_context_edit_stats_block(stats: &ContextEditStats) -> String {
    let saved_chars = stats.original_chars.saturating_sub(stats.edited_chars);
    [
        format!("original_chars:          {}", stats.original_chars),
        format!("edited_chars:            {}", stats.edited_chars),
        format!("saved_chars:             {saved_chars}"),
        format!("pack_system_chars:       {}", stats.system_chars),
        format!("pack_note_chars:         {}", stats.system_note_chars),
        format!("pack_memory_chars:       {}", stats.memory_recall_chars),
        format!("pack_history_chars:      {}", stats.history_chars),
        format!("pack_tool_chars:         {}", stats.tool_result_chars),
        format!(
            "compressed_tool_results: {}",
            stats.compressed_tool_results
        ),
        format!("dropped_messages:        {}", stats.dropped_messages),
        format!("summary_checkpoints:     {}", stats.summary_checkpoints),
    ]
    .join("\n")
}

struct SessionRecorder {
    index: SessionIndex,
    session_id: String,
    workspace: PathBuf,
    log_path: PathBuf,
}

impl SessionRecorder {
    fn new(session_id: String, workspace: PathBuf, log_path: PathBuf) -> Self {
        SessionRecorder {
            index: SessionIndex::default(),
            session_id,
            workspace,
            log_path,
        }
    }

    fn record(&mut self, session: &Session) {
        let _ = self
            .index
            .record_turn(&self.session_id, &self.workspace, session, &self.log_path);
    }
}

fn session_log_path(workspace: &Path) -> PathBuf {
    workspace.join(".nanocodex").join("session.jsonl")
}

/// Print a stable, parseable token-usage line to stderr (one-shot mode).
/// Format: `[ncx-usage] prompt_tokens=P completion_tokens=C total_tokens=T`.
/// `total_tokens` is P+C (the provider does not report a total directly).
fn emit_usage_line(usage: &std::collections::BTreeMap<String, i64>) {
    let prompt = usage.get("prompt_tokens").copied().unwrap_or(0);
    let completion = usage.get("completion_tokens").copied().unwrap_or(0);
    eprintln!(
        "[ncx-usage] prompt_tokens={prompt} completion_tokens={completion} total_tokens={}",
        prompt + completion
    );
}

fn render_history(entries: &[SessionSummary], limit: usize) -> String {
    if entries.is_empty() {
        return "No saved sessions.".into();
    }
    let mut out = String::from("Saved sessions:");
    for summary in entries.iter().take(limit) {
        let title = if summary.title.trim().is_empty() {
            "(no prompt yet)"
        } else {
            summary.title.as_str()
        };
        out.push_str(&format!(
            "\n  {}  {}  {}  users={} assistants={} tools={}",
            summary.updated_at,
            summary.session_id,
            title,
            summary.user_messages,
            summary.assistant_messages,
            summary.tool_calls
        ));
    }
    out
}

fn compact_session_text(agent: &mut AgentLoop, recorder: &mut SessionRecorder) -> String {
    let stats = agent.session.compact(&agent.context_edit);
    recorder.record(&agent.session);
    format!(
        "Compacted session: chars {} -> {}; compressed_tool_results={} dropped_messages={} summary_checkpoints={}",
        stats.original_chars,
        stats.edited_chars,
        stats.compressed_tool_results,
        stats.dropped_messages,
        stats.summary_checkpoints
    )
}

fn checkpoint_before_turn(workspace: &Path, prompt: &str) {
    let label = format!("auto: {}", clipped_label(prompt, 80));
    match CheckpointStore::new(workspace).create(&label) {
        Ok(meta) => eprintln!(
            "checkpoint {} saved ({} file(s), {} skipped).",
            meta.id,
            meta.files.len(),
            meta.skipped_paths.len()
        ),
        Err(e) => eprintln!("checkpoint warning: {e}"),
    }
}

fn create_checkpoint_text(workspace: &Path, label: &str) -> String {
    let label = if label.trim().is_empty() {
        "manual checkpoint"
    } else {
        label.trim()
    };
    match CheckpointStore::new(workspace).create(label) {
        Ok(meta) => format_checkpoint_saved(&meta),
        Err(e) => format!("checkpoint failed: {e}"),
    }
}

fn restore_checkpoint_text(workspace: &Path, id: &str) -> String {
    if id.trim().is_empty() {
        return "usage: /restore <checkpoint-id>".into();
    }
    match CheckpointStore::new(workspace).restore(id) {
        Ok(report) => {
            let safety = report
                .safety_checkpoint_id
                .map(|id| format!("\nsafety checkpoint: {id}"))
                .unwrap_or_else(|| "\nsafety checkpoint: failed".into());
            format!(
                "restored checkpoint {}\nrestored_files: {}\ndeleted_files: {}{}",
                report.checkpoint_id, report.restored_files, report.deleted_files, safety
            )
        }
        Err(e) => format!("restore failed: {e}"),
    }
}

fn format_checkpoint_saved(meta: &CheckpointMeta) -> String {
    format!(
        "checkpoint: {}\nlabel: {}\nfiles: {}  skipped: {}  bytes: {}",
        meta.id,
        meta.label,
        meta.files.len(),
        meta.skipped_paths.len(),
        meta.total_bytes
    )
}

fn render_checkpoints(entries: &[CheckpointMeta], limit: usize) -> String {
    if entries.is_empty() {
        return "No checkpoints.".into();
    }
    let mut out = String::from("Checkpoints:");
    for meta in entries.iter().take(limit) {
        out.push_str(&format!(
            "\n  {}  {}  {}  files={} skipped={}",
            meta.created_at,
            meta.id,
            if meta.label.is_empty() {
                "(unlabeled)"
            } else {
                meta.label.as_str()
            },
            meta.files.len(),
            meta.skipped_paths.len()
        ));
    }
    out
}

fn clipped_label(text: &str, limit: usize) -> String {
    let s = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= limit {
        s
    } else {
        format!(
            "{}...",
            s.chars().take(limit.saturating_sub(3)).collect::<String>()
        )
    }
}

fn positive_usize(value: i64, fallback: usize) -> usize {
    usize::try_from(value)
        .ok()
        .filter(|v| *v > 0)
        .unwrap_or(fallback)
}

fn nonnegative_usize(value: i64, fallback: usize) -> usize {
    usize::try_from(value).ok().unwrap_or(fallback)
}

fn task_budget_from_config(cfg: &ncx_config::Config) -> TaskBudget {
    TaskBudget {
        max_model_calls: positive_usize(cfg.max_iterations, 60),
        max_tool_calls: nonnegative_usize(cfg.max_tool_calls, 120),
    }
}

/// Build a dedicated vision provider from the `vl_*` config, or `None` when no
/// vision model is configured (image turns then stay on the main provider).
///
/// Only `vl_model` is required; `vl_base_url` / `vl_api_key` fall back to the
/// main `base_url` / `api_key`, so a user can either point at a separate VL
/// endpoint (e.g. DashScope) or just name a vision model on the same endpoint.
fn build_vision_provider(cfg: &ncx_config::Config) -> Option<Box<dyn Provider>> {
    if cfg.vl_model.trim().is_empty() {
        return None;
    }
    let base_url = if cfg.vl_base_url.trim().is_empty() {
        &cfg.base_url
    } else {
        &cfg.vl_base_url
    };
    let api_key = if cfg.vl_api_key.trim().is_empty() {
        cfg.api_key.clone()
    } else {
        cfg.vl_api_key.clone()
    };
    Some(Box::new(DeepSeekProvider::with_opts(
        api_key,
        base_url,
        cfg.vl_model.clone(),
        cfg.timeout_s as u64,
        cfg.max_retries as u32,
    )))
}

/// Build the one-shot user input. With no images it is just the prompt text;
/// with `--image` paths it becomes an OpenAI-style multimodal `content` array
/// (`text` block + one `image_url` block per file, each a base64 `data:` URL),
/// which trips [`AgentLoop`]'s image detection and routes to the vision model.
fn build_image_user_input(text: &str, images: &[PathBuf]) -> Result<serde_json::Value, String> {
    if images.is_empty() {
        return Ok(json!(text));
    }
    let mut content = vec![json!({"type": "text", "text": text})];
    for path in images {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("cannot read image {}: {e}", path.display()))?;
        let url = format!("data:{};base64,{}", image_mime(path), base64_encode(&bytes));
        content.push(json!({"type": "image_url", "image_url": {"url": url}}));
    }
    Ok(serde_json::Value::Array(content))
}

/// Guess an image MIME type from the file extension (defaults to PNG).
fn image_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

/// Standard base64 encoding (RFC 4648, with `=` padding). Hand-rolled to avoid a
/// new crate dependency for the single image-attachment use site.
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn context_edit_from_config(cfg: &ncx_config::Config) -> ContextEditPolicy {
    ContextEditPolicy {
        enabled: cfg.context_edit_enabled,
        max_chars: positive_usize(cfg.context_edit_max_chars, 120_000),
        keep_recent_messages: positive_usize(cfg.context_edit_keep_recent_messages, 30),
        max_tool_result_chars: positive_usize(cfg.context_edit_max_tool_result_chars, 4_000),
        max_history_chars: positive_usize(cfg.context_edit_max_history_chars, 90_000),
        max_tool_result_total_chars: positive_usize(
            cfg.context_edit_max_tool_result_total_chars,
            35_000,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_all_commands() {
        let help = render_help();
        for (cmd, _) in SLASH_HELP {
            assert!(help.contains(cmd), "{cmd}");
        }
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 §10 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn image_input_builds_multimodal_content() {
        let dir = std::env::temp_dir().join(format!("ncx_img_{}", new_session_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("pic.jpg");
        std::fs::write(&img, b"foobar").unwrap();

        // No images -> plain text string.
        assert_eq!(build_image_user_input("hi", &[]).unwrap(), json!("hi"));

        // With an image -> [text, image_url(data: URL)].
        let v = build_image_user_input("describe", &[img]).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0], json!({"type": "text", "text": "describe"}));
        assert_eq!(arr[1]["type"], "image_url");
        assert_eq!(
            arr[1]["image_url"]["url"].as_str().unwrap(),
            "data:image/jpeg;base64,Zm9vYmFy"
        );

        // A missing file is a clean error, not a panic.
        assert!(build_image_user_input("x", &[dir.join("nope.png")]).is_err());
    }

    #[test]
    fn inline_images_split_from_prompt() {
        // No flag -> all text, no images.
        let (t, imgs) = split_inline_images("what is this");
        assert_eq!(t, "what is this");
        assert!(imgs.is_empty());

        // Flags anywhere are pulled out; remaining words form the prompt.
        let (t, imgs) = split_inline_images("--image a.png compare these --image b.jpg now");
        assert_eq!(t, "compare these now");
        assert_eq!(imgs, vec![PathBuf::from("a.png"), PathBuf::from("b.jpg")]);
    }

    #[test]
    fn vision_provider_only_built_when_vl_model_set() {
        let mut cfg = ncx_config::Config::default();
        // No vl_model -> image turns stay on the main provider.
        assert!(build_vision_provider(&cfg).is_none());
        // vl_model set -> a dedicated vision provider is constructed.
        cfg.vl_model = "qwen-vl-max".into();
        assert!(build_vision_provider(&cfg).is_some());
    }

    #[test]
    fn custom_command_expands_project_prompt_template() {
        let ws = std::env::temp_dir().join(format!("ncx_custom_cmd_{}", new_session_id()));
        let dir = ws.join(".nanocodex").join("commands");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("review.md"),
            "---\ndescription: Review a file\n---\nReview $ARGUMENTS[0] with $0. Full: $ARGUMENTS",
        )
        .unwrap();

        let out = custom_command_prompt(&ws, "/review", "src/main.rs extra")
            .unwrap()
            .unwrap();

        assert_eq!(
            out,
            "Review src/main.rs with src/main.rs. Full: src/main.rs extra"
        );
        assert!(!out.contains("description"));
    }

    #[test]
    fn custom_command_supports_claude_compatible_project_dir() {
        let ws = std::env::temp_dir().join(format!("ncx_custom_claude_{}", new_session_id()));
        let dir = ws.join(".claude").join("commands");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("audit.md"), "Audit the current change.").unwrap();

        let out = custom_command_prompt(&ws, "/project:audit", "focus tests")
            .unwrap()
            .unwrap();

        assert_eq!(out, "Audit the current change.\n\nArguments: focus tests");
    }

    #[test]
    fn help_lists_custom_project_commands() {
        let ws = std::env::temp_dir().join(format!("ncx_custom_help_{}", new_session_id()));
        let dir = ws.join(".nanocodex").join("commands");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ship.md"), "Prepare release notes.").unwrap();

        let help = render_help_for_workspace(&ws);

        assert!(help.contains("Custom commands"));
        assert!(help.contains("/project:ship"));
    }

    #[test]
    fn custom_command_parser_rejects_unknown_scope_or_bad_name() {
        assert!(parse_custom_command_query("/project:review").is_some());
        assert!(parse_custom_command_query("/user:review").is_some());
        assert!(parse_custom_command_query("/team:review").is_none());
        assert!(parse_custom_command_query("/bad/name").is_none());
        assert!(parse_custom_command_query("/bad name").is_none());
    }

    #[test]
    fn parse_config_assignment_accepts_trimmed_key_value() {
        assert_eq!(
            parse_config_assignment(" model = deepseek-chat ").unwrap(),
            ("model".into(), "deepseek-chat".into())
        );
        assert!(parse_config_assignment("model").is_err());
        assert!(parse_config_assignment("bad key=value").is_err());
        assert!(parse_config_assignment("model=").is_err());
    }

    #[test]
    fn usage_tracker_renders_last_and_total_usage() {
        let mut tracker = UsageTracker::default();
        assert_eq!(tracker.render(), "No token usage recorded yet.");

        let mut first_usage = BTreeMap::new();
        first_usage.insert("prompt_tokens".into(), 100);
        first_usage.insert("completion_tokens".into(), 20);
        first_usage.insert("prompt_cache_hit_tokens".into(), 80);
        first_usage.insert("prompt_cache_miss_tokens".into(), 20);
        tracker.record(&TurnResult {
            final_text: "ok".into(),
            iterations: 2,
            stop_reason: "completed".into(),
            visible_tools: vec!["read_file".into(), "tool_search".into()],
            tools_used: vec!["read_file".into()],
            usage: first_usage,
            context_edit: ContextEditStats {
                original_chars: 1000,
                edited_chars: 700,
                compressed_tool_results: 2,
                dropped_messages: 3,
                summary_checkpoints: 1,
                ..Default::default()
            },
        });

        let mut second_usage = BTreeMap::new();
        second_usage.insert("prompt_tokens".into(), 10);
        second_usage.insert("completion_tokens".into(), 5);
        tracker.record(&TurnResult {
            final_text: "ok".into(),
            iterations: 1,
            stop_reason: "completed".into(),
            visible_tools: vec!["read_file".into(), "tool_search".into()],
            tools_used: vec![],
            usage: second_usage,
            context_edit: ContextEditStats {
                original_chars: 700,
                edited_chars: 650,
                compressed_tool_results: 1,
                dropped_messages: 0,
                summary_checkpoints: 2,
                ..Default::default()
            },
        });

        let rendered = tracker.render();
        assert!(rendered.contains("Last turn"));
        assert!(rendered.contains("Session total"));
        assert!(rendered.contains("model_calls: 3"));
        assert!(rendered.contains("tool_calls:  1"));
        assert!(rendered.contains("prompt_tokens:     110"));
        assert!(rendered.contains("completion_tokens: 25"));
        assert!(rendered.contains("prompt_cache_hit_tokens:  80"));
        assert!(rendered.contains("Context edit"));
        assert!(rendered.contains("original_chars:          700"));
        assert!(rendered.contains("saved_chars:             50"));
        assert!(rendered.contains("summary_checkpoints:     2"));
        assert!(rendered.contains("session_compressed:      3"));
        assert!(rendered.contains("session_dropped:         3"));
        assert!(rendered.contains("session_summaries:       3"));
        assert!(rendered.contains("raw token usage only"));
    }

    #[test]
    fn budget_status_renders_limits_last_turn_and_session_use() {
        let ws = std::env::temp_dir().join(format!("ncx_budget_status_{}", new_session_id()));
        let policy = SandboxPolicy::new("workspace-write", &ws);
        let ctx = ToolContext::new(ws.clone(), policy);
        let tools = ToolRegistry::new(ctx);
        let agent = AgentLoop::new(
            Box::new(DeepSeekProvider::new(
                "sk-test",
                "http://127.0.0.1:9/v1",
                "test-model",
            )),
            tools,
            Session::new("system prompt"),
        )
        .with_task_budget(TaskBudget {
            max_model_calls: 5,
            max_tool_calls: 8,
        });
        let cfg = ncx_config::Config {
            workspace: ws,
            max_iterations: 5,
            max_tool_calls: 8,
            context_token_budget: 2048,
            ..Default::default()
        };
        let mut usage = UsageTracker::default();
        usage.record(&TurnResult {
            final_text: "ok".into(),
            iterations: 2,
            stop_reason: "completed".into(),
            visible_tools: vec!["read_file".into(), "shell".into(), "grep".into()],
            tools_used: vec!["read_file".into(), "shell".into(), "grep".into()],
            usage: BTreeMap::new(),
            context_edit: ContextEditStats::default(),
        });

        let out = render_budget_status(&agent, &cfg, &usage);

        assert!(out.contains("Task budget"));
        assert!(out.contains("per_task_model_calls: 5"));
        assert!(out.contains("per_task_tool_calls: 8"));
        assert!(out.contains("context_token_budget: 2048"));
        assert!(out.contains("Session use"));
        assert!(out.contains("model_calls: 2"));
        assert!(out.contains("tool_calls:  3"));
        assert!(out.contains("remaining: 3"));
        assert!(out.contains("remaining: 5"));
        assert!(out.contains("stop_reason: completed"));
    }

    #[test]
    fn context_status_renders_active_policy_and_preview() {
        let ws = std::env::temp_dir().join(format!("ncx_context_status_{}", new_session_id()));
        let policy = SandboxPolicy::new("workspace-write", &ws);
        let ctx = ToolContext::new(ws.clone(), policy);
        let tools = ToolRegistry::new(ctx);
        let mut session = Session::new("system prompt");
        session.add_user_text("first request");
        session.add_tool_result("call_1", "shell", &"x".repeat(300));
        session.add_user_text("latest request");

        let agent = AgentLoop::new(
            Box::new(DeepSeekProvider::new(
                "sk-test",
                "http://127.0.0.1:9/v1",
                "test-model",
            )),
            tools,
            session,
        )
        .with_context_edit(ContextEditPolicy {
            enabled: true,
            max_chars: 120,
            keep_recent_messages: 1,
            max_tool_result_chars: 20,
            max_history_chars: 90,
            max_tool_result_total_chars: 40,
        });
        let cfg = ncx_config::Config {
            workspace: ws,
            context_token_budget: 2048,
            ..Default::default()
        };

        let out = render_context_status(&agent, &cfg, &UsageTracker::default());

        assert!(out.contains("Context editing"));
        assert!(out.contains("enabled: true"));
        assert!(out.contains("max_chars: 120"));
        assert!(out.contains("max_history_chars: 90"));
        assert!(out.contains("max_tool_result_total_chars: 40"));
        assert!(out.contains("context_token_budget: 2048"));
        assert!(out.contains("messages: 3"));
        assert!(out.contains("Next send preview"));
        assert!(out.contains("pack_system_chars:"), "{out}");
        assert!(out.contains("pack_history_chars:"), "{out}");
        assert!(out.contains("pack_tool_chars:"), "{out}");
        assert!(out.contains("compressed_tool_results: 1"), "{out}");
        assert!(out.contains("dropped_messages:        2"), "{out}");
        assert!(out.contains("summary_checkpoints:"), "{out}");
        assert!(out.contains("No model turn recorded yet."));
    }

    #[test]
    fn config_text_writes_known_key_to_path() {
        let dir = std::env::temp_dir().join(format!("ncx_config_slash_{}", new_session_id()));
        let path = dir.join("config.toml");
        let cfg = ncx_config::Config::default();
        let out = config_text_at(&cfg, "model=deepseek-chat", &path);

        assert!(out.contains("Saved config"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("model = \"deepseek-chat\""), "{text}");
    }

    #[test]
    fn config_text_rejects_unknown_key() {
        let dir = std::env::temp_dir().join(format!("ncx_config_slash_bad_{}", new_session_id()));
        let path = dir.join("config.toml");
        let cfg = ncx_config::Config::default();
        let out = config_text_at(&cfg, "bogus=value", &path);

        assert!(out.contains("Unknown writable config key"));
        assert!(!path.exists());
    }

    #[test]
    fn status_masks_api_key() {
        let cfg = ncx_config::Config {
            api_key: "sk-secret1234".into(),
            ..Default::default()
        };
        let status = render_status(&cfg);
        assert!(status.contains("****1234"));
        assert!(!status.contains("secret"));
    }

    #[test]
    fn history_renders_saved_sessions() {
        let rows = vec![SessionSummary {
            session_id: "sid".into(),
            workspace: "/p".into(),
            title: "fix bug".into(),
            snippet: "done".into(),
            user_messages: 1,
            assistant_messages: 2,
            tool_calls: 3,
            recent_tools: vec!["read_file".into()],
            created_at: "2026-06-01T09:00:00".into(),
            updated_at: "2026-06-01T10:00:00".into(),
            log_path: "/p/.nanocodex/session.jsonl".into(),
            has_snapshot: true,
            archived: false,
        }];
        let out = render_history(&rows, 10);
        assert!(out.contains("sid"));
        assert!(out.contains("fix bug"));
        assert!(out.contains("tools=3"));
    }

    #[test]
    fn memory_status_renders_entries_tags_and_recall() {
        let dir = std::env::temp_dir().join(format!("ncx_memory_status_{}", new_session_id()));
        let memory = MemoryStore::new(&dir);
        memory
            .remember(
                "Tauri desktop shell builds compact Windows bundles",
                &["release".into(), "windows".into()],
                10,
            )
            .unwrap();
        memory
            .remember(
                "Use crate-type lib for the Tauri backend on GNU",
                &["build".into()],
                20,
            )
            .unwrap();
        let proposal = memory
            .propose(
                "Review generated memory before trusting it",
                &["memory".into()],
                "test",
                30,
            )
            .unwrap()
            .unwrap();

        let out = render_memory_status(Some(&memory), "native installer release");

        assert!(out.contains("Project memory"));
        assert!(out.contains("LEARNINGS.md"));
        assert!(out.contains("PROPOSALS.md"));
        assert!(out.contains("INDEX.json"));
        assert!(out.contains("entries: 2"));
        assert!(out.contains("pending_proposals: 1"));
        assert!(out.contains("vector_dims:"));
        assert!(out.contains("release=1"));
        assert!(out.contains("Recent notes:"));
        assert!(out.contains("Pending proposals:"));
        assert!(out.contains(&proposal.id));
        assert!(out.contains("Recall preview"));
        assert!(out.contains("Tauri desktop shell"));
    }

    #[test]
    fn memory_command_accepts_and_rejects_proposals() {
        let dir = std::env::temp_dir().join(format!("ncx_memory_review_{}", new_session_id()));
        let memory = MemoryStore::new(&dir);
        let accept = handle_memory_command(Some(&memory), "propose Accept this memory", &dir);
        assert!(accept.contains("Queued memory proposal"), "{accept}");
        let first = memory.proposals().first().unwrap().id.clone();

        let out = handle_memory_command(Some(&memory), &format!("accept {first}"), &dir);
        assert!(out.contains("Accepted memory proposal"), "{out}");
        assert!(memory.proposals().is_empty());
        assert_eq!(memory.entries().len(), 1);

        let p = memory
            .propose("Reject this memory", &[], "test", 10)
            .unwrap()
            .unwrap();
        let out = handle_memory_command(Some(&memory), &format!("reject {}", p.id), &dir);
        assert!(out.contains("Rejected memory proposal"), "{out}");
        assert!(memory.proposals().is_empty());
        assert_eq!(memory.entries().len(), 1);
    }

    #[test]
    fn memory_command_edits_and_batches_proposals() {
        let dir = std::env::temp_dir().join(format!("ncx_memory_batch_{}", new_session_id()));
        let memory = MemoryStore::new(&dir);
        let _ = handle_memory_command(Some(&memory), "propose Draft one", &dir);
        let _ = handle_memory_command(Some(&memory), "propose Draft two", &dir);
        let first = memory.proposals().first().unwrap().id.clone();

        let out = handle_memory_command(
            Some(&memory),
            &format!("edit {first} Edited memory note"),
            &dir,
        );
        assert!(out.contains("Updated memory proposal"), "{out}");
        assert!(memory
            .proposals()
            .iter()
            .any(|p| p.text == "Edited memory note"));

        let out = handle_memory_command(Some(&memory), "accept-all", &dir);
        assert!(out.contains("Accepted 2 memory proposals"), "{out}");
        assert!(memory.proposals().is_empty());
        assert_eq!(memory.entries().len(), 2);

        let _ = handle_memory_command(Some(&memory), "propose Draft three", &dir);
        let out = handle_memory_command(Some(&memory), "reject-all", &dir);
        assert!(out.contains("Rejected 1 memory proposals"), "{out}");
        assert!(memory.proposals().is_empty());
    }

    #[test]
    fn memory_command_rebuilds_vector_index() {
        let dir = std::env::temp_dir().join(format!("ncx_memory_index_{}", new_session_id()));
        let memory = MemoryStore::new(&dir);
        memory
            .remember("Vector index supports memory recall", &["memory".into()], 1)
            .unwrap();
        let _ = std::fs::remove_file(memory.index_path());

        let out = handle_memory_command(Some(&memory), "index", &dir);

        assert!(out.contains("Rebuilt memory vector index"), "{out}");
        assert!(memory.index_path().exists());
    }

    #[test]
    fn memory_command_harvests_release_document() {
        let ws = std::env::temp_dir().join(format!("ncx_memory_harvest_{}", new_session_id()));
        let _ = std::fs::create_dir_all(&ws);
        let doc = ws.join("RELEASE_TASK.md");
        std::fs::write(
            &doc,
            "- [ ] Run `cmd /c npm run build` before packaging the Tauri installer.\n",
        )
        .unwrap();
        let memory = MemoryStore::new(ws.join(".ncx").join("memory"));

        let out = handle_memory_command(Some(&memory), "harvest RELEASE_TASK.md", &ws);

        assert!(out.contains("Memory proposal harvest"), "{out}");
        assert!(out.contains("queued_total: 1"), "{out}");
        let proposals = memory.proposals();
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].text.contains("Tauri installer"));
    }

    #[test]
    fn memory_status_mentions_when_disabled() {
        let out = render_memory_status(None, "");

        assert!(out.contains("not enabled"));
    }

    #[test]
    fn tools_status_renders_catalog_visible_tools_and_hints() {
        let ws = std::env::temp_dir().join(format!("ncx_tools_status_{}", new_session_id()));
        let policy = SandboxPolicy::new("workspace-write", &ws);
        let ctx = ToolContext::new(ws, policy);
        let registry = ToolRegistry::new(ctx);
        registry.ctx.tool_hints.borrow_mut().push("shell".into());

        let out = render_tools_status(&registry, "search files");

        assert!(out.contains("Tool catalog"));
        assert!(out.contains("registered:"));
        assert!(out.contains("read_only:"));
        assert!(out.contains("write_or_effect:"));
        assert!(out.contains("query: search files"));
        assert!(out.contains("Visible tools:"));
        assert!(out.contains("tool_search"));
        assert!(out.contains("Tool search hints: shell"));
    }

    #[test]
    fn mcp_status_groups_registered_tools_by_server() {
        let servers = vec![ncx_config::McpServerConfig {
            name: "fs".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "server-fs".into()],
            env: HashMap::new(),
            enabled: true,
            trusted: false,
            permission: "ask".into(),
            allowed_tools: vec![],
        }];
        let connectors = vec![
            ncx_config::McpConnectorConfig {
                name: "fs".into(),
                display_name: "Filesystem".into(),
                description: "local files".into(),
                transport: "stdio".into(),
                command: "npx".into(),
                args: vec!["-y".into(), "server-fs".into()],
                url: String::new(),
                env: HashMap::new(),
                headers: HashMap::new(),
                auth: "none".into(),
                headers_helper: String::new(),
                oauth: ncx_config::McpConnectorOAuthConfig::default(),
                enabled: true,
                trusted: false,
                permission: "ask".into(),
                allowed_tools: vec!["list".into()],
                source: "npm:server-fs".into(),
            },
            ncx_config::McpConnectorConfig {
                name: "remote_docs".into(),
                display_name: "Remote Docs".into(),
                description: "remote docs".into(),
                transport: "sse".into(),
                command: String::new(),
                args: vec![],
                url: "https://user:pass@example.test/mcp?token=secret#frag-secret".into(),
                env: HashMap::new(),
                headers: HashMap::from([("Authorization".into(), "Bearer secret".into())]),
                auth: "oauth".into(),
                headers_helper: String::new(),
                oauth: ncx_config::McpConnectorOAuthConfig {
                    client_id: "docs-client".into(),
                    client_secret_env: "DOCS_CLIENT_SECRET".into(),
                    callback_port: Some(8765),
                    scopes: vec!["docs:read".into(), "search".into()],
                    auth_server_metadata_url:
                        "https://example.test/.well-known/oauth-authorization-server".into(),
                },
                enabled: true,
                trusted: false,
                permission: "ask".into(),
                allowed_tools: vec![],
                source: "remote:https://example.test".into(),
            },
        ];
        let catalog = vec![
            ncx_core::tools::ToolCatalogEntry {
                name: "read_file".into(),
                description: "core".into(),
                read_only: true,
            },
            ncx_core::tools::ToolCatalogEntry {
                name: "mcp__fs__list".into(),
                description: "list".into(),
                read_only: true,
            },
            ncx_core::tools::ToolCatalogEntry {
                name: "mcp__fs__write".into(),
                description: "write".into(),
                read_only: false,
            },
        ];

        let out = render_mcp_status(&servers, &connectors, &catalog);

        assert!(out.contains("fs: npx -y server-fs"));
        assert!(out.contains("Connector install specs"));
        assert!(out.contains("permission=ask"));
        assert!(out.contains("allowed_tools=list"));
        assert!(out.contains("source=npm:server-fs"));
        assert!(out.contains("auth=none"));
        assert!(out.contains("fs: transport=stdio enabled=true launch=launchable"));
        assert!(out.contains("remote_docs: transport=sse"));
        assert!(out.contains("enabled=true launch=audit-only"));
        assert!(out.contains("launch_gap=remote_transport_not_implemented"));
        assert!(out.contains("source=remote:https://example.test"));
        assert!(out.contains("auth=oauth;headers=1;oauth_client_id=set"));
        assert!(out.contains("oauth_secret_env=set"));
        assert!(out.contains("callback_port=8765"));
        assert!(out.contains("scopes=2"));
        assert!(out.contains("metadata_url=set"));
        assert!(out.contains("endpoint=https://<redacted>@example.test/mcp?<redacted>#<redacted>"));
        assert!(!out.contains("Bearer secret"));
        assert!(!out.contains("DOCS_CLIENT_SECRET"));
        assert!(!out.contains("user:pass"));
        assert!(!out.contains("token=secret"));
        assert!(!out.contains("frag-secret"));
        assert!(out.contains("Registered MCP tools: 2"));
        assert!(out.contains("fs (2): list, write"));
        assert!(!out.contains("read_file"));
    }

    #[test]
    fn mcp_status_mentions_when_no_tools_registered() {
        let out = render_mcp_status(&[], &[], &[]);

        assert!(out.contains("(none)"));
        assert!(out.contains("start the REPL with --mcp"));
    }

    #[test]
    fn checkpoints_render_saved_entries() {
        let rows = vec![CheckpointMeta {
            id: "cp1".into(),
            label: "before edit".into(),
            created_at: "2026-06-01T10:00:00".into(),
            files: vec!["a.txt".into()],
            skipped_paths: vec!["target/big".into()],
            total_bytes: 12,
        }];
        let out = render_checkpoints(&rows, 10);
        assert!(out.contains("cp1"));
        assert!(out.contains("before edit"));
        assert!(out.contains("skipped=1"));
    }
}
