//! nanocodex GUI (Tauri v2) — Rust backend.
//!
//! The agent loop runs on a dedicated `!Send` thread (see [`bridge`]); the
//! frontend talks to it through the `send_prompt` command and listens for
//! `ncx://event` window events. `get_status` is a cheap synchronous snapshot
//! for the header.

mod bridge;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ncx_config::{
    load_config, write_nanocodex_config, Config, ConfigPaths, Overrides, VALID_APPROVAL_POLICIES,
    VALID_SANDBOX_MODES,
};
use ncx_core::{
    custom_command_prompt, list_custom_commands, CheckpointMeta, CheckpointStore, MemoryEntry,
    MemoryStore, RestoreReport, SessionIndex, SessionSummary, Summarizer,
};
use ncx_provider::DeepSeekProvider;
use serde::Serialize;
use serde_json::json;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use bridge::{spawn_worker, Command, PendingMap};

#[derive(Serialize)]
pub struct Status {
    model: String,
    sandbox: String,
    approval: String,
    workspace: String,
    /// Masked (`****1234`) — never the real key.
    api_key: String,
    max_iterations: i64,
    max_tool_calls: i64,
    context_edit_enabled: bool,
    context_edit_max_chars: i64,
}

/// Tauri managed state: the channel into the agent thread + pending approvals.
struct AppState {
    tx: UnboundedSender<Command>,
    pending: PendingMap,
}

#[derive(Serialize)]
pub struct CheckpointView {
    id: String,
    label: String,
    created_at: String,
    files: usize,
    skipped: usize,
    total_bytes: u64,
}

#[derive(Serialize)]
pub struct ConfigLocation {
    config_path: String,
    config_dir: String,
}

#[derive(Serialize)]
pub struct RestoreView {
    checkpoint_id: String,
    safety_checkpoint_id: Option<String>,
    restored_files: usize,
    deleted_files: usize,
}

#[derive(Serialize)]
pub struct CustomCommandView {
    scope: String,
    name: String,
    slash: String,
    path: String,
}

#[derive(Serialize)]
pub struct MemoryEntryView {
    ts: u64,
    tags: Vec<String>,
    text: String,
}

#[derive(Serialize)]
pub struct MemorySnapshot {
    path: String,
    count: usize,
    entries: Vec<MemoryEntryView>,
}

#[derive(Serialize)]
pub struct MemoryMergeReport {
    path: String,
    removed: usize,
    count: usize,
}

#[derive(Serialize)]
pub struct SessionSummaryView {
    session_id: String,
    workspace: String,
    title: String,
    snippet: String,
    user_messages: usize,
    assistant_messages: usize,
    tool_calls: usize,
    recent_tools: Vec<String>,
    created_at: String,
    updated_at: String,
    log_path: String,
    has_snapshot: bool,
    current_workspace: bool,
}

#[derive(Serialize)]
pub struct ResumeSessionReport {
    session_id: String,
    restored_messages: usize,
    log_path: String,
}

/// Load the resolved config and return a display-safe snapshot.
#[tauri::command]
fn get_status() -> Result<Status, String> {
    let workspace = std::env::current_dir().ok();
    let overrides = Overrides {
        workspace,
        ..Default::default()
    };
    let cfg = load_config(overrides).map_err(|e| e.to_string())?;
    let red = cfg.redacted();
    Ok(Status {
        model: cfg.model.clone(),
        sandbox: cfg.sandbox_mode.clone(),
        approval: cfg.approval_policy.clone(),
        workspace: cfg.workspace.display().to_string(),
        api_key: red.get("api_key").cloned().unwrap_or_default(),
        max_iterations: cfg.max_iterations,
        max_tool_calls: cfg.max_tool_calls,
        context_edit_enabled: cfg.context_edit_enabled,
        context_edit_max_chars: cfg.context_edit_max_chars,
    })
}

/// Queue a user prompt for the agent thread. Replies arrive as `ncx://event`s.
#[tauri::command]
fn send_prompt(text: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state
        .tx
        .send(Command::Prompt(text))
        .map_err(|_| "agent thread is not running".to_string())
}

#[tauri::command]
fn get_config_location() -> Result<ConfigLocation, String> {
    config_location()
}

#[tauri::command]
fn open_config_file() -> Result<(), String> {
    let path = ensure_config_file()?;
    open_file(&path)
}

#[tauri::command]
fn open_config_dir() -> Result<(), String> {
    let dir = ensure_config_dir()?;
    open_dir(&dir)
}

/// The editable settings shown in the Settings panel. The API key is never
/// returned in the clear — only whether one is set, plus a masked tail.
#[derive(Serialize)]
pub struct Settings {
    model: String,
    base_url: String,
    sandbox_mode: String,
    approval_policy: String,
    reasoning_effort: String,
    max_iterations: i64,
    max_tool_calls: i64,
    context_edit_enabled: bool,
    context_edit_max_chars: i64,
    context_edit_keep_recent_messages: i64,
    context_edit_max_tool_result_chars: i64,
    api_key_masked: String,
    has_api_key: bool,
    available_models: Vec<String>,
    sandbox_modes: Vec<String>,
    approval_policies: Vec<String>,
}

/// Read the current settings for the panel (with dropdown option lists).
#[tauri::command]
fn get_settings() -> Result<Settings, String> {
    let workspace = std::env::current_dir().ok();
    let cfg = load_config(Overrides {
        workspace,
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    let masked = cfg.redacted().get("api_key").cloned().unwrap_or_default();
    Ok(Settings {
        model: cfg.model.clone(),
        base_url: cfg.base_url.clone(),
        sandbox_mode: cfg.sandbox_mode.clone(),
        approval_policy: cfg.approval_policy.clone(),
        reasoning_effort: cfg.reasoning_effort.clone(),
        max_iterations: cfg.max_iterations,
        max_tool_calls: cfg.max_tool_calls,
        context_edit_enabled: cfg.context_edit_enabled,
        context_edit_max_chars: cfg.context_edit_max_chars,
        context_edit_keep_recent_messages: cfg.context_edit_keep_recent_messages,
        context_edit_max_tool_result_chars: cfg.context_edit_max_tool_result_chars,
        api_key_masked: masked,
        has_api_key: !cfg.api_key.is_empty(),
        available_models: cfg.available_models.clone(),
        sandbox_modes: VALID_SANDBOX_MODES.iter().map(|s| s.to_string()).collect(),
        approval_policies: VALID_APPROVAL_POLICIES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    })
}

/// Persist settings to `~/.nanocodex/config.toml`, then rebuild the agent so the
/// change applies live. Empty values are skipped (so a blank API key keeps the
/// existing one). Only known keys are written.
#[tauri::command]
fn save_settings(
    updates: std::collections::HashMap<String, String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let borrowed: HashMap<&str, &str> = updates
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let path = ConfigPaths::default().nanocodex;
    write_nanocodex_config(&borrowed, &path).map_err(|e| e.to_string())?;
    // Apply live (fresh session with the new config).
    let _ = state.tx.send(Command::Reload);
    Ok(())
}

fn config_location() -> Result<ConfigLocation, String> {
    let path = ConfigPaths::default().nanocodex;
    let dir = path
        .parent()
        .ok_or_else(|| "config path has no parent directory".to_string())?
        .to_path_buf();
    Ok(ConfigLocation {
        config_path: path.display().to_string(),
        config_dir: dir.display().to_string(),
    })
}

fn ensure_config_dir() -> Result<PathBuf, String> {
    let path = ConfigPaths::default().nanocodex;
    let dir = path
        .parent()
        .ok_or_else(|| "config path has no parent directory".to_string())?
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn ensure_config_file() -> Result<PathBuf, String> {
    let path = ConfigPaths::default().nanocodex;
    if !path.exists() {
        let empty: HashMap<&str, &str> = HashMap::new();
        write_nanocodex_config(&empty, &path).map_err(|e| e.to_string())?;
    }
    Ok(path)
}

#[cfg(target_os = "windows")]
fn open_file(path: &Path) -> Result<(), String> {
    ProcessCommand::new("notepad.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to open config file: {e}"))
}

#[cfg(target_os = "windows")]
fn open_dir(path: &Path) -> Result<(), String> {
    ProcessCommand::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to open config directory: {e}"))
}

#[cfg(target_os = "macos")]
fn open_file(path: &Path) -> Result<(), String> {
    open_with("open", path, "config file")
}

#[cfg(target_os = "macos")]
fn open_dir(path: &Path) -> Result<(), String> {
    open_with("open", path, "config directory")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_file(path: &Path) -> Result<(), String> {
    open_with("xdg-open", path, "config file")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_dir(path: &Path) -> Result<(), String> {
    open_with("xdg-open", path, "config directory")
}

#[cfg(not(target_os = "windows"))]
fn open_with(program: &str, path: &Path, label: &str) -> Result<(), String> {
    ProcessCommand::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to open {label}: {e}"))
}

/// Answer a pending approval request (raised by an `approval` event).
#[tauri::command]
fn approve(id: u64, approved: bool, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let sender = state.pending.lock().unwrap().remove(&id);
    match sender {
        Some(tx) => tx
            .send(approved)
            .map_err(|_| "approval already resolved".to_string()),
        None => Err(format!("no pending approval with id {id}")),
    }
}

#[tauri::command]
fn get_checkpoints() -> Result<Vec<CheckpointView>, String> {
    let cfg = load_config(Overrides {
        workspace: std::env::current_dir().ok(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    Ok(CheckpointStore::new(&cfg.workspace)
        .list()
        .into_iter()
        .map(checkpoint_view)
        .collect())
}

#[tauri::command]
fn create_checkpoint(label: String) -> Result<CheckpointView, String> {
    let cfg = load_config(Overrides {
        workspace: std::env::current_dir().ok(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    let label = if label.trim().is_empty() {
        "manual checkpoint"
    } else {
        label.trim()
    };
    CheckpointStore::new(&cfg.workspace)
        .create(label)
        .map(checkpoint_view)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn restore_checkpoint(id: String) -> Result<RestoreView, String> {
    let cfg = load_config(Overrides {
        workspace: std::env::current_dir().ok(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    CheckpointStore::new(&cfg.workspace)
        .restore(&id)
        .map(restore_view)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_memory() -> Result<MemorySnapshot, String> {
    let (store, path) = memory_store()?;
    Ok(memory_snapshot(&store, &path))
}

#[tauri::command]
fn remember_note(text: String, tags: Vec<String>) -> Result<MemorySnapshot, String> {
    let (store, path) = memory_store()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let tags = clean_tags(&tags);
    store
        .remember(&text, &tags, now)
        .map_err(|e| e.to_string())?;
    Ok(memory_snapshot(&store, &path))
}

#[tauri::command]
fn consolidate_memory() -> Result<MemoryMergeReport, String> {
    let (store, path) = memory_store()?;
    let removed = store.consolidate(0.85).map_err(|e| e.to_string())?;
    Ok(memory_report(&store, &path, removed))
}

#[tauri::command]
fn summarize_memory() -> Result<MemoryMergeReport, String> {
    let cfg = gui_config()?;
    let store = MemoryStore::new(cfg.workspace.join(".ncx").join("memory"));
    let path = memory_path(&cfg);
    let summarizer = GuiSummarizer::new(cfg);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let removed = rt
        .block_on(store.summarize_consolidate(&summarizer, 0.85))
        .map_err(|e| e.to_string())?;
    Ok(memory_report(&store, &path, removed))
}

#[tauri::command]
fn open_memory_file() -> Result<(), String> {
    let (_store, path) = memory_store()?;
    ensure_parent_dir(&path)?;
    if !path.exists() {
        std::fs::write(&path, "# Project memory (nanocodex)\n\n").map_err(|e| e.to_string())?;
    }
    open_file(&path)
}

#[tauri::command]
fn get_sessions() -> Result<Vec<SessionSummaryView>, String> {
    let cfg = gui_config()?;
    let workspace = cfg.workspace.display().to_string();
    Ok(SessionIndex::default()
        .entries()
        .into_iter()
        .map(|summary| session_summary_view(summary, &workspace))
        .collect())
}

#[tauri::command]
fn open_session_log(session_id: String) -> Result<(), String> {
    let index = SessionIndex::default();
    let summary = index
        .get(&session_id)
        .ok_or_else(|| format!("unknown session: {session_id}"))?;
    if summary.log_path.trim().is_empty() {
        return Err("session has no log path".into());
    }
    let path = PathBuf::from(&summary.log_path);
    if !path.exists() {
        return Err(format!("session log does not exist: {}", path.display()));
    }
    open_file(&path)
}

#[tauri::command]
fn open_session_snapshot(session_id: String) -> Result<(), String> {
    let index = SessionIndex::default();
    let summary = index
        .get(&session_id)
        .ok_or_else(|| format!("unknown session: {session_id}"))?;
    if !summary.has_snapshot {
        return Err("session has no snapshot".into());
    }
    let path = index.snapshot_path(&session_id);
    if !path.exists() {
        return Err(format!(
            "session snapshot does not exist: {}",
            path.display()
        ));
    }
    open_file(&path)
}

#[tauri::command]
fn resume_session(
    session_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ResumeSessionReport, String> {
    let cfg = gui_config()?;
    let current_workspace = cfg.workspace.display().to_string();
    let index = SessionIndex::default();
    let summary = index
        .get(&session_id)
        .ok_or_else(|| format!("unknown session: {session_id}"))?;
    if summary.workspace != current_workspace {
        return Err(format!(
            "session belongs to {}; current workspace is {}",
            summary.workspace, current_workspace
        ));
    }
    let messages = index
        .load_snapshot(&session_id)
        .ok_or_else(|| "session snapshot is missing or unreadable".to_string())?;
    let log_path = cfg.workspace.join(".nanocodex").join("session.jsonl");
    write_session_log(&log_path, &messages)?;
    state
        .tx
        .send(Command::Resume)
        .map_err(|_| "agent thread is not running".to_string())?;
    Ok(ResumeSessionReport {
        session_id,
        restored_messages: messages.len(),
        log_path: log_path.display().to_string(),
    })
}

#[tauri::command]
fn get_custom_commands() -> Result<Vec<CustomCommandView>, String> {
    let cfg = load_config(Overrides {
        workspace: std::env::current_dir().ok(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    Ok(list_custom_commands(&cfg.workspace)
        .into_iter()
        .map(|cmd| CustomCommandView {
            scope: cmd.scope.to_string(),
            name: cmd.name.clone(),
            slash: format!("/{}:{}", cmd.scope, cmd.name),
            path: cmd.path.display().to_string(),
        })
        .collect())
}

#[tauri::command]
fn expand_custom_command(slash: String, arg: String) -> Result<String, String> {
    let cfg = load_config(Overrides {
        workspace: std::env::current_dir().ok(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    match custom_command_prompt(&cfg.workspace, &slash, &arg) {
        Ok(Some(prompt)) => Ok(prompt),
        Ok(None) => Err(format!("unknown custom command: {slash}")),
        Err(e) => Err(e),
    }
}

fn gui_config() -> Result<Config, String> {
    load_config(Overrides {
        workspace: std::env::current_dir().ok(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())
}

fn memory_store() -> Result<(MemoryStore, PathBuf), String> {
    let cfg = gui_config()?;
    let path = memory_path(&cfg);
    Ok((
        MemoryStore::new(cfg.workspace.join(".ncx").join("memory")),
        path,
    ))
}

fn memory_path(cfg: &Config) -> PathBuf {
    cfg.workspace
        .join(".ncx")
        .join("memory")
        .join("LEARNINGS.md")
}

fn memory_snapshot(store: &MemoryStore, path: &Path) -> MemorySnapshot {
    let mut entries = store.entries();
    entries.sort_by(|a, b| b.ts.cmp(&a.ts));
    let views: Vec<MemoryEntryView> = entries.into_iter().map(memory_entry_view).collect();
    MemorySnapshot {
        path: path.display().to_string(),
        count: views.len(),
        entries: views,
    }
}

fn memory_entry_view(entry: MemoryEntry) -> MemoryEntryView {
    MemoryEntryView {
        ts: entry.ts,
        tags: entry.tags,
        text: entry.text,
    }
}

fn memory_report(store: &MemoryStore, path: &Path, removed: usize) -> MemoryMergeReport {
    MemoryMergeReport {
        path: path.display().to_string(),
        removed,
        count: store.entries().len(),
    }
}

fn session_summary_view(summary: SessionSummary, current_workspace: &str) -> SessionSummaryView {
    let current = summary.workspace == current_workspace;
    SessionSummaryView {
        session_id: summary.session_id,
        workspace: summary.workspace,
        title: summary.title,
        snippet: summary.snippet,
        user_messages: summary.user_messages,
        assistant_messages: summary.assistant_messages,
        tool_calls: summary.tool_calls,
        recent_tools: summary.recent_tools,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        log_path: summary.log_path,
        has_snapshot: summary.has_snapshot,
        current_workspace: current,
    }
}

fn write_session_log(path: &Path, messages: &[serde_json::Value]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    for message in messages {
        let line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        writeln!(file, "{line}").map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim().trim_start_matches('#').to_lowercase();
        if tag.is_empty() || out.iter().any(|existing| existing == &tag) {
            continue;
        }
        out.push(tag);
    }
    out
}

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "memory path has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())
}

struct GuiSummarizer {
    cfg: Config,
}

impl GuiSummarizer {
    fn new(cfg: Config) -> Self {
        GuiSummarizer { cfg }
    }

    fn fast_model(&self) -> String {
        if self.cfg.fast_model.is_empty() {
            self.cfg.model.clone()
        } else {
            self.cfg.fast_model.clone()
        }
    }
}

#[async_trait(?Send)]
impl Summarizer for GuiSummarizer {
    async fn merge(&self, facts: &[String]) -> Option<String> {
        let provider = DeepSeekProvider::with_opts(
            self.cfg.api_key.clone(),
            &self.cfg.base_url,
            self.fast_model(),
            self.cfg.timeout_s as u64,
            self.cfg.max_retries as u32,
        );
        let user = facts
            .iter()
            .enumerate()
            .map(|(i, fact)| format!("{}. {fact}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let messages = vec![
            json!({"role": "system", "content": "Merge these related project notes into ONE concise factual note (at most 2 sentences). Output ONLY the merged note — no preamble, no list, no quotes."}),
            json!({"role": "user", "content": user}),
        ];
        match provider.chat(&messages, None, None, None, None).await {
            Ok(response) if !response.content.trim().is_empty() => {
                Some(response.content.trim().to_string())
            }
            _ => None,
        }
    }
}

fn checkpoint_view(meta: CheckpointMeta) -> CheckpointView {
    CheckpointView {
        id: meta.id,
        label: meta.label,
        created_at: meta.created_at,
        files: meta.files.len(),
        skipped: meta.skipped_paths.len(),
        total_bytes: meta.total_bytes,
    }
}

fn restore_view(report: RestoreReport) -> RestoreView {
    RestoreView {
        checkpoint_id: report.checkpoint_id,
        safety_checkpoint_id: report.safety_checkpoint_id,
        restored_files: report.restored_files,
        deleted_files: report.deleted_files,
    }
}

pub fn run() {
    let (tx, rx) = unbounded_channel::<Command>();
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_for_worker = pending.clone();

    tauri::Builder::default()
        .manage(AppState { tx, pending })
        .setup(move |app| {
            // Hand the agent thread an AppHandle (to emit events), the receiver
            // (to take prompts), and the shared pending-approvals map.
            spawn_worker(app.handle().clone(), rx, pending_for_worker);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            send_prompt,
            approve,
            get_settings,
            save_settings,
            get_config_location,
            open_config_file,
            open_config_dir,
            get_checkpoints,
            create_checkpoint,
            restore_checkpoint,
            get_custom_commands,
            expand_custom_command,
            get_memory,
            remember_note,
            consolidate_memory,
            summarize_memory,
            open_memory_file,
            get_sessions,
            open_session_log,
            open_session_snapshot,
            resume_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running the nanocodex GUI");
}
