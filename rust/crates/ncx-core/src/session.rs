//! Conversation history — Rust port of the parts of `nanocodex/agent/session.py`
//! the turn loop relies on.
//!
//! Messages are stored as `serde_json::Value` objects in OpenAI chat shape so
//! they go straight onto the wire. The system prompt is held separately and
//! prepended by [`Session::for_model`].

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ContextEditPolicy {
    pub enabled: bool,
    pub max_chars: usize,
    pub keep_recent_messages: usize,
    pub max_tool_result_chars: usize,
    pub max_history_chars: usize,
    pub max_tool_result_total_chars: usize,
}

impl Default for ContextEditPolicy {
    fn default() -> Self {
        ContextEditPolicy {
            enabled: true,
            max_chars: 120_000,
            keep_recent_messages: 30,
            max_tool_result_chars: 4_000,
            max_history_chars: 90_000,
            max_tool_result_total_chars: 35_000,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextEditStats {
    pub original_chars: usize,
    pub edited_chars: usize,
    pub system_chars: usize,
    pub system_note_chars: usize,
    pub memory_recall_chars: usize,
    pub history_chars: usize,
    pub tool_result_chars: usize,
    pub compressed_tool_results: usize,
    pub dropped_messages: usize,
    pub summary_checkpoints: usize,
}

#[derive(Debug, Clone)]
pub struct ContextMessages {
    pub messages: Vec<Value>,
    pub stats: ContextEditStats,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub system: String,
    pub messages: Vec<Value>,
    pub log_path: Option<PathBuf>,
    pub restored_count: usize,
}

impl Session {
    pub fn new(system: impl Into<String>) -> Self {
        Self::with_log(system, None)
    }

    pub fn with_log(system: impl Into<String>, log_path: Option<PathBuf>) -> Self {
        if let Some(path) = &log_path {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
        }
        Session {
            system: system.into(),
            messages: Vec::new(),
            log_path,
            restored_count: 0,
        }
    }

    pub fn resume(system: impl Into<String>, log_path: Option<PathBuf>) -> Self {
        let restored = read_log(log_path.as_deref())
            .into_iter()
            .filter(|m| role(m) != Some("system"))
            .collect::<Vec<_>>();
        let body = sanitize_restored_messages(restored, "[interrupted: tool result not recorded]");
        let mut session = Self::with_log(system, log_path);
        session.restored_count = body.len();
        session.messages = body;
        session
    }

    pub fn fork(
        system: impl Into<String>,
        seed_messages: Vec<Value>,
        log_path: Option<PathBuf>,
    ) -> Self {
        let body = seed_messages
            .into_iter()
            .filter(|m| role(m) != Some("system"))
            .collect::<Vec<_>>();
        let body = sanitize_restored_messages(body, "[interrupted: tool result not recorded]");
        let mut session = Self::with_log(system, log_path);
        session.restored_count = body.len();
        session.messages = body;
        session
    }

    pub fn full_messages(&self) -> Vec<Value> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        out.push(json!({"role": "system", "content": self.system}));
        out.extend(self.messages.clone());
        out
    }

    /// Append a user message. `content` may be a plain string or a multimodal
    /// content array (already a JSON value).
    pub fn add_user(&mut self, content: Value) {
        self.append(json!({"role": "user", "content": content}));
    }

    pub fn add_user_text(&mut self, text: &str) {
        self.add_user(Value::String(text.to_string()));
    }

    /// Append an assistant message, optionally carrying tool_calls and reasoning.
    pub fn add_assistant(
        &mut self,
        content: &str,
        tool_calls: Option<Vec<Value>>,
        reasoning: &str,
    ) {
        let mut msg = serde_json::Map::new();
        msg.insert("role".into(), json!("assistant"));
        msg.insert("content".into(), json!(content));
        if let Some(tcs) = tool_calls {
            if !tcs.is_empty() {
                msg.insert("tool_calls".into(), Value::Array(tcs));
            }
        }
        if !reasoning.trim().is_empty() {
            msg.insert("reasoning_content".into(), json!(reasoning));
        }
        self.append(Value::Object(msg));
    }

    /// Append a tool result message answering a specific tool_call id.
    pub fn add_tool_result(&mut self, call_id: &str, name: &str, result: &str) {
        self.append(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "name": name,
            "content": result,
        }));
    }

    /// System message + history, ready for the provider.
    pub fn for_model(&self) -> Vec<Value> {
        self.for_model_edited(
            &[],
            &ContextEditPolicy {
                enabled: false,
                ..Default::default()
            },
        )
        .messages
    }

    /// System message + optional runtime notes + an edited history, ready for
    /// the provider. Editing is a non-destructive send-time view: the complete
    /// session log remains in `self.messages`.
    pub fn for_model_edited(
        &self,
        system_notes: &[String],
        policy: &ContextEditPolicy,
    ) -> ContextMessages {
        self.for_model_edited_with_focus(system_notes, policy, "")
    }

    /// Like [`Self::for_model_edited`], but adds an explicit focus hint to any
    /// summary checkpoint created while truncating older history.
    pub fn for_model_edited_with_focus(
        &self,
        system_notes: &[String],
        policy: &ContextEditPolicy,
        focus: &str,
    ) -> ContextMessages {
        let (body, mut stats) = self.edited_body(system_notes, policy, focus);

        let mut out = Vec::with_capacity(self.messages.len() + 1);
        out.push(json!({"role": "system", "content": self.system}));
        for note in system_notes {
            if !note.trim().is_empty() {
                out.push(json!({"role": "system", "content": note}));
            }
        }
        out.extend(body);
        stats.edited_chars = out.iter().map(json_chars).sum();
        ContextMessages {
            messages: out,
            stats,
        }
    }

    /// Materialize the send-time context editing policy into the live session.
    /// This powers `/compact`: after it runs, future turns and `--resume` see
    /// the compacted history instead of only a temporary provider view.
    pub fn compact(&mut self, policy: &ContextEditPolicy) -> ContextEditStats {
        self.compact_with_focus(policy, "")
    }

    /// Materialize context editing with a user-supplied focus hint. This mirrors
    /// Claude-style focused compaction while keeping the local deterministic
    /// summary auditable.
    pub fn compact_with_focus(
        &mut self,
        policy: &ContextEditPolicy,
        focus: &str,
    ) -> ContextEditStats {
        let mut policy = policy.clone();
        policy.enabled = true;
        let (body, stats) = self.edited_body(&[], &policy, focus);
        if stats.compressed_tool_results > 0 || stats.dropped_messages > 0 {
            self.messages = body;
            self.rewrite_log();
        }
        stats
    }

    fn edited_body(
        &self,
        system_notes: &[String],
        policy: &ContextEditPolicy,
        focus: &str,
    ) -> (Vec<Value>, ContextEditStats) {
        let original_chars = total_chars(&self.system, system_notes, &self.messages);
        let mut body = self.messages.clone();
        let mut stats = ContextEditStats {
            original_chars,
            edited_chars: original_chars,
            ..Default::default()
        };

        if policy.enabled {
            let recent_cutoff = body.len().saturating_sub(policy.keep_recent_messages);
            for (i, msg) in body.iter_mut().enumerate() {
                if i < recent_cutoff
                    && role(msg) == Some("tool")
                    && compress_tool_result(msg, policy.max_tool_result_chars)
                {
                    stats.compressed_tool_results += 1;
                }
            }

            stats.compressed_tool_results += enforce_tool_result_bucket(
                &mut body,
                policy.keep_recent_messages,
                policy.max_tool_result_total_chars,
            );

            if body.iter().map(json_chars).sum::<usize>() > policy.max_history_chars {
                let (dropped, summaries) =
                    compact_old_prefix(&mut body, policy.keep_recent_messages, focus);
                stats.dropped_messages += dropped;
                stats.summary_checkpoints += summaries;
            }

            if total_chars(&self.system, system_notes, &body) > policy.max_chars
                && body.len() > policy.keep_recent_messages
            {
                let (dropped, summaries) =
                    compact_old_prefix(&mut body, policy.keep_recent_messages, focus);
                stats.dropped_messages += dropped;
                stats.summary_checkpoints += summaries;
            }
        }

        stats.edited_chars = total_chars(&self.system, system_notes, &body);
        stats.system_chars = self.system.chars().count();
        stats.system_note_chars = system_notes.iter().map(|n| n.chars().count()).sum();
        stats.memory_recall_chars = system_notes
            .iter()
            .filter(|n| n.contains("[memory recall for this prompt]"))
            .map(|n| n.chars().count())
            .sum();
        stats.history_chars = body.iter().map(json_chars).sum();
        stats.tool_result_chars = tool_result_chars(&body);
        (body, stats)
    }

    /// Set of tool_call ids that already have a `tool` reply.
    fn answered_ids(&self) -> std::collections::HashSet<String> {
        self.messages
            .iter()
            .filter(|m| m.get("role").and_then(|v| v.as_str()) == Some("tool"))
            .filter_map(|m| {
                m.get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .collect()
    }

    /// Backfill synthetic tool results for any assistant tool_call left
    /// unanswered, so the history stays valid (every tool_call has a tool reply).
    pub fn backfill_unanswered_tool_calls(&mut self, placeholder: &str) {
        let answered = self.answered_ids();
        let mut missing: Vec<(String, String)> = Vec::new();
        for m in &self.messages {
            if m.get("role").and_then(|v| v.as_str()) != Some("assistant") {
                continue;
            }
            let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) else {
                continue;
            };
            for tc in tcs {
                let Some(id) = tc.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                if answered.contains(id) || missing.iter().any(|(mid, _)| mid == id) {
                    continue;
                }
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                missing.push((id.to_string(), name));
            }
        }
        for (id, name) in missing {
            self.add_tool_result(&id, &name, placeholder);
        }
    }

    fn append(&mut self, msg: Value) {
        self.messages.push(msg.clone());
        self.append_log(&msg);
    }

    fn append_log(&self, msg: &Value) {
        let Some(path) = &self.log_path else {
            return;
        };
        let Some(mut record) = redact_image_data(msg, "[image omitted from log]")
            .as_object()
            .cloned()
        else {
            return;
        };
        record.insert("_ts".into(), json!(now_stamp()));
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };
        if let Ok(line) = serde_json::to_string(&Value::Object(record)) {
            let _ = writeln!(file, "{line}");
        }
    }

    fn rewrite_log(&self) {
        let Some(path) = &self.log_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
        else {
            return;
        };
        for msg in &self.messages {
            let Some(mut record) = redact_image_data(msg, "[image omitted from log]")
                .as_object()
                .cloned()
            else {
                continue;
            };
            record.insert("_ts".into(), json!(now_stamp()));
            if let Ok(line) = serde_json::to_string(&Value::Object(record)) {
                let _ = writeln!(file, "{line}");
            }
        }
    }
}

fn role(msg: &Value) -> Option<&str> {
    msg.get("role").and_then(|v| v.as_str())
}

fn read_log(path: Option<&Path>) -> Vec<Value> {
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter_map(|mut value| {
            let obj = value.as_object_mut()?;
            if obj.get("role").and_then(|v| v.as_str()).is_none() {
                return None;
            }
            obj.remove("_ts");
            Some(value)
        })
        .collect()
}

fn sanitize_restored_messages(messages: Vec<Value>, placeholder: &str) -> Vec<Value> {
    let mut fulfilled: HashSet<String> = messages
        .iter()
        .filter(|m| role(m) == Some("tool"))
        .filter_map(|m| m.get("tool_call_id").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect();
    let mut out = Vec::new();
    for msg in messages {
        let calls = msg
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        out.push(msg);
        for tc in calls {
            let Some(id) = tc.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if fulfilled.contains(id) {
                continue;
            }
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool");
            out.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "name": name,
                "content": placeholder,
            }));
            fulfilled.insert(id.to_string());
        }
    }
    out
}

pub(crate) fn redact_image_data(msg: &Value, placeholder: &str) -> Value {
    let mut out = msg.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
    let Some(content) = obj.get_mut("content").and_then(|v| v.as_array_mut()) else {
        return out;
    };
    for block in content {
        let is_data_image = block.get("type").and_then(|v| v.as_str()) == Some("image_url")
            && block
                .get("image_url")
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
                .is_some_and(|url| url.starts_with("data:"));
        if is_data_image {
            *block = json!({"type": "text", "text": placeholder});
        }
    }
    out
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| format!("{:013}", d.as_millis()))
        .unwrap_or_else(|_| "0000000000000".into())
}

fn json_chars(v: &Value) -> usize {
    serde_json::to_string(v)
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

fn total_chars(system: &str, notes: &[String], messages: &[Value]) -> usize {
    system.chars().count()
        + notes.iter().map(|n| n.chars().count()).sum::<usize>()
        + messages.iter().map(json_chars).sum::<usize>()
}

fn tool_result_chars(messages: &[Value]) -> usize {
    messages
        .iter()
        .filter(|m| role(m) == Some("tool"))
        .filter_map(|m| m.get("content").and_then(|v| v.as_str()))
        .map(|s| s.chars().count())
        .sum()
}

fn enforce_tool_result_bucket(
    messages: &mut [Value],
    keep_recent_messages: usize,
    max_total_chars: usize,
) -> usize {
    let recent_cutoff = messages.len().saturating_sub(keep_recent_messages);
    let mut compressed = 0;
    for i in 0..recent_cutoff {
        if tool_result_chars(messages) <= max_total_chars {
            break;
        }
        if role(&messages[i]) != Some("tool") {
            continue;
        }
        if summarize_tool_result(&mut messages[i]) {
            compressed += 1;
        }
    }
    compressed
}

fn compress_tool_result(msg: &mut Value, max_chars: usize) -> bool {
    let Some(obj) = msg.as_object_mut() else {
        return false;
    };
    let Some(content) = obj.get("content").and_then(|v| v.as_str()) else {
        return false;
    };
    if content.chars().count() <= max_chars {
        return false;
    }
    let head: String = content.chars().take(max_chars).collect();
    let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
    obj.insert(
        "content".into(),
        json!(format!(
            "{head}\n[context edited: omitted the rest of prior {name} result; original_chars={}]",
            content.chars().count()
        )),
    );
    true
}

fn summarize_tool_result(msg: &mut Value) -> bool {
    let Some(obj) = msg.as_object_mut() else {
        return false;
    };
    let Some(content) = obj.get("content").and_then(|v| v.as_str()) else {
        return false;
    };
    let original_chars = content.chars().count();
    let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
    let summary = format!(
        "[context edited: omitted prior {name} result to preserve tool-result context bucket; original_chars={original_chars}]"
    );
    if summary.chars().count() >= original_chars || content == summary.as_str() {
        return false;
    }
    obj.insert("content".into(), json!(summary));
    true
}

fn compact_old_prefix(
    body: &mut Vec<Value>,
    keep_recent_messages: usize,
    focus: &str,
) -> (usize, usize) {
    if body.len() <= keep_recent_messages {
        return (0, 0);
    }
    let mut start = body.len().saturating_sub(keep_recent_messages);
    if let Some(rel) = body[start..].iter().position(|m| role(m) == Some("user")) {
        start += rel;
    }
    while start < body.len() && role(&body[start]) == Some("tool") {
        start += 1;
    }
    if start > 0 && start < body.len() {
        let prefix = body[..start].to_vec();
        let mut tail = body[start..].to_vec();
        let summaries = context_summary_checkpoint(&prefix, &tail, focus)
            .map(|summary| {
                tail.insert(0, summary);
                1
            })
            .unwrap_or(0);
        *body = tail;
        (start, summaries)
    } else {
        (0, 0)
    }
}

fn context_summary_checkpoint(prefix: &[Value], tail: &[Value], focus: &str) -> Option<Value> {
    if prefix.is_empty() {
        return None;
    }
    let focus = truncate_one_line(focus, 180);
    let mut roles: BTreeMap<String, usize> = BTreeMap::new();
    let mut first_user = None;
    let mut recent_user = None;
    let mut recent_assistant = None;
    let mut tools = Vec::<String>::new();
    let focus_anchors = focus_anchors(prefix, tail, &focus, 3);
    for msg in prefix {
        let role = role(msg).unwrap_or("unknown").to_string();
        *roles.entry(role.clone()).or_insert(0) += 1;
        match role.as_str() {
            "user" => {
                let text = truncate_one_line(&message_text(msg), 160);
                if first_user.is_none() {
                    first_user = Some(text.clone());
                }
                recent_user = Some(text);
            }
            "assistant" => {
                let text = truncate_one_line(&message_text(msg), 180);
                if !text.is_empty() {
                    recent_assistant = Some(text);
                }
                if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for call in calls {
                        if let Some(name) = call
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                        {
                            push_unique_tool(&mut tools, name);
                        }
                    }
                }
            }
            "tool" => {
                if let Some(name) = msg.get("name").and_then(|v| v.as_str()) {
                    push_unique_tool(&mut tools, name);
                }
            }
            _ => {}
        }
    }
    let role_line = roles
        .into_iter()
        .map(|(role, count)| format!("{role}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![
        "[context summary checkpoint]".to_string(),
        format!("- omitted_messages: {}", prefix.len()),
        format!("- role_counts: {role_line}"),
    ];
    if let Some(text) = first_user.filter(|s| !s.is_empty()) {
        lines.push(format!("- first_user: {text}"));
    }
    if let Some(text) = recent_user.filter(|s| !s.is_empty()) {
        lines.push(format!("- recent_user: {text}"));
    }
    if let Some(text) = recent_assistant.filter(|s| !s.is_empty()) {
        lines.push(format!("- recent_assistant: {text}"));
    }
    if !tools.is_empty() {
        lines.push(format!(
            "- tools_seen: {}",
            tools.into_iter().take(12).collect::<Vec<_>>().join(", ")
        ));
    }
    if !focus.is_empty() {
        lines.push(format!("- focus_instruction: {focus}"));
    }
    if !focus_anchors.is_empty() {
        lines.push("- focus_anchors:".into());
        for anchor in focus_anchors {
            lines.push(format!("  - {}: {}", anchor.role, anchor.text));
        }
    }
    lines.push("- note: older transcript was deterministically summarized before truncation.".into());
    let summary = json!({"role": "assistant", "content": lines.join("\n")});
    if json_chars(&summary) < prefix.iter().map(json_chars).sum() {
        Some(summary)
    } else {
        None
    }
}

fn push_unique_tool(tools: &mut Vec<String>, name: &str) {
    if !tools.iter().any(|tool| tool == name) {
        tools.push(name.to_string());
    }
}

#[derive(Debug)]
struct FocusAnchor {
    index: usize,
    role: String,
    score: usize,
    text: String,
}

fn focus_anchors(
    prefix: &[Value],
    tail: &[Value],
    explicit_focus: &str,
    limit: usize,
) -> Vec<FocusAnchor> {
    let terms = focus_terms(tail, explicit_focus);
    if terms.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut anchors = prefix
        .iter()
        .enumerate()
        .filter_map(|(index, msg)| {
            let text = truncate_one_line(&message_text(msg), 180);
            if text.is_empty() {
                return None;
            }
            let msg_terms = lexical_terms(&text);
            let score = msg_terms.intersection(&terms).count();
            if score == 0 {
                return None;
            }
            Some(FocusAnchor {
                index,
                role: role(msg).unwrap_or("unknown").to_string(),
                score,
                text,
            })
        })
        .collect::<Vec<_>>();
    anchors.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.index.cmp(&a.index)));
    anchors.truncate(limit);
    anchors.sort_by_key(|a| a.index);
    anchors
}

fn focus_terms(tail: &[Value], explicit_focus: &str) -> HashSet<String> {
    let query = tail
        .iter()
        .rev()
        .find(|msg| role(msg) == Some("user"))
        .map(message_text)
        .unwrap_or_else(|| {
            tail.iter()
                .map(message_text)
                .collect::<Vec<_>>()
                .join(" ")
        });
    let mut terms = lexical_terms(&query);
    terms.extend(lexical_terms(explicit_focus));
    terms
}

fn lexical_terms(text: &str) -> HashSet<String> {
    const STOP_WORDS: &[&str] = &[
        "about", "after", "again", "also", "and", "are", "can", "for", "from", "how", "into",
        "more", "need", "now", "old", "please", "that", "the", "this", "with", "you",
    ];
    let mut terms = HashSet::new();
    let mut token = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            for lower in ch.to_lowercase() {
                token.push(lower);
            }
        } else {
            flush_term(&mut terms, &mut token, STOP_WORDS);
        }
    }
    flush_term(&mut terms, &mut token, STOP_WORDS);
    terms
}

fn flush_term(terms: &mut HashSet<String>, token: &mut String, stop_words: &[&str]) {
    if token.chars().count() >= 3 && !stop_words.iter().any(|word| *word == token.as_str()) {
        terms.insert(token.clone());
    }
    token.clear();
}

fn message_text(msg: &Value) -> String {
    let Some(content) = msg.get("content") else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(blocks) = content.as_array() {
        return blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
    }
    String::new()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_model_prepends_system() {
        let mut s = Session::new("sys");
        s.add_user_text("hi");
        let msgs = s.for_model();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "sys");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hi");
    }

    #[test]
    fn assistant_records_reasoning_only_when_present() {
        let mut s = Session::new("sys");
        s.add_assistant("answer", None, "");
        assert!(s.messages[0].get("reasoning_content").is_none());
        s.add_assistant("answer2", None, "because");
        assert_eq!(s.messages[1]["reasoning_content"], "because");
    }

    #[test]
    fn backfill_answers_dangling_tool_calls() {
        let mut s = Session::new("sys");
        let tcs = vec![
            json!({"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}),
            json!({"id": "c2", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}),
        ];
        s.add_assistant("", Some(tcs), "");
        s.add_tool_result("c1", "read_file", "real result");
        s.backfill_unanswered_tool_calls("[interrupted]");
        let tool_ids: std::collections::HashSet<_> = s
            .messages
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["tool_call_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            tool_ids,
            ["c1", "c2"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn context_edit_compresses_old_tool_results_without_mutating_session() {
        let mut s = Session::new("sys");
        s.add_user_text("inspect logs");
        s.add_assistant("", Some(vec![json!({"id": "c1", "type": "function", "function": {"name": "shell", "arguments": "{}"}})]), "");
        s.add_tool_result("c1", "shell", &"x".repeat(200));
        s.add_user_text("continue");

        let out = s.for_model_edited(
            &["budget note".into()],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 10_000,
                keep_recent_messages: 1,
                max_tool_result_chars: 20,
                ..Default::default()
            },
        );
        assert_eq!(out.stats.compressed_tool_results, 1);
        assert_eq!(out.stats.system_chars, 3);
        assert!(out.stats.system_note_chars >= "budget note".len());
        assert!(out.stats.history_chars > 0);
        assert!(out.stats.tool_result_chars <= 128);
        assert!(out
            .messages
            .iter()
            .any(|m| m["role"] == "system" && m["content"] == "budget note"));
        assert!(out.messages.iter().any(|m| {
            m["role"] == "tool"
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("context edited")
        }));
        assert_eq!(s.messages[2]["content"].as_str().unwrap().len(), 200);
    }

    #[test]
    fn context_edit_reports_memory_recall_pack_bucket() {
        let mut s = Session::new("sys");
        s.add_user_text("continue");

        let out = s.for_model_edited(
            &["[memory recall for this prompt]\n- use apply_patch".into()],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 10_000,
                keep_recent_messages: 4,
                max_tool_result_chars: 20,
                ..Default::default()
            },
        );

        assert!(out.stats.system_note_chars > 0);
        assert_eq!(out.stats.system_note_chars, out.stats.memory_recall_chars);
        assert!(out.stats.history_chars > 0);
    }

    #[test]
    fn context_edit_drops_old_prefix_when_over_budget() {
        let mut s = Session::new("sys");
        for i in 0..8 {
            s.add_user_text(&format!("old user {i} {}", "x".repeat(40)));
            s.add_assistant(&format!("old answer {i} {}", "y".repeat(40)), None, "");
        }
        let out = s.for_model_edited(
            &[],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 500,
                keep_recent_messages: 4,
                max_tool_result_chars: 20,
                ..Default::default()
            },
        );
        assert!(out.stats.dropped_messages > 0);
        assert!(out.stats.summary_checkpoints > 0);
        assert!(out.stats.edited_chars < out.stats.original_chars);
        assert_eq!(out.messages[0]["role"], "system");
        assert!(out.messages.iter().any(|m| {
            m["role"] == "assistant"
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("[context summary checkpoint]")
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("omitted_messages")
        }));
    }

    #[test]
    fn context_summary_checkpoint_keeps_focus_anchors() {
        let mut s = Session::new("sys");
        s.add_user_text("investigate payment latency and dashboard polish");
        s.add_assistant("payment latency is unrelated to the current auth budget work", None, "");
        s.add_user_text("authentication budget regression root cause is in the shared worker pool");
        s.add_assistant(
            "remember that authentication budget regression needs parent budget accounting",
            None,
            "",
        );
        for i in 0..8 {
            s.add_user_text(&format!("noise topic {i} {}", "x".repeat(50)));
            s.add_assistant(&format!("noise answer {i} {}", "y".repeat(50)), None, "");
        }
        s.add_user_text("continue the authentication budget regression fix");

        let out = s.for_model_edited(
            &[],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 100_000,
                keep_recent_messages: 1,
                max_tool_result_chars: 20,
                max_history_chars: 900,
                max_tool_result_total_chars: 10_000,
            },
        );

        let summary = out
            .messages
            .iter()
            .find(|m| {
                m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("[context summary checkpoint]")
            })
            .and_then(|m| m["content"].as_str())
            .unwrap_or("");
        assert!(summary.contains("focus_anchors"), "{summary}");
        assert!(summary.contains("authentication budget regression"), "{summary}");
    }

    #[test]
    fn context_summary_checkpoint_uses_explicit_focus_hint() {
        let mut s = Session::new("sys");
        s.add_user_text("authentication budget regression root cause is in shared worker pooling");
        s.add_assistant(
            "the auth budget fix must preserve parent task accounting",
            None,
            "",
        );
        for i in 0..8 {
            s.add_user_text(&format!("release packaging noise {i} {}", "x".repeat(50)));
            s.add_assistant(&format!("installer answer {i} {}", "y".repeat(50)), None, "");
        }
        s.add_user_text("continue polishing the installer UI");

        let out = s.for_model_edited_with_focus(
            &[],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 100_000,
                keep_recent_messages: 1,
                max_tool_result_chars: 20,
                max_history_chars: 900,
                max_tool_result_total_chars: 10_000,
            },
            "auth budget parent task accounting",
        );

        let summary = out
            .messages
            .iter()
            .find(|m| {
                m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("[context summary checkpoint]")
            })
            .and_then(|m| m["content"].as_str())
            .unwrap_or("");
        assert!(
            summary.contains("focus_instruction: auth budget parent task accounting"),
            "{summary}"
        );
        assert!(summary.contains("focus_anchors"), "{summary}");
        assert!(summary.contains("auth budget fix"), "{summary}");
    }

    #[test]
    fn context_edit_enforces_tool_result_bucket() {
        let mut s = Session::new("sys");
        for i in 0..3 {
            let id = format!("c{i}");
            s.add_user_text(&format!("inspect chunk {i}"));
            s.add_assistant(
                "",
                Some(vec![json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                })]),
                "",
            );
            s.add_tool_result(&format!("c{i}"), "shell", &"x".repeat(600));
        }
        s.add_user_text("continue");

        let out = s.for_model_edited(
            &[],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 10_000,
                keep_recent_messages: 1,
                max_tool_result_chars: 10_000,
                max_history_chars: 10_000,
                max_tool_result_total_chars: 450,
            },
        );

        assert!(out.stats.compressed_tool_results >= 3);
        assert!(out.stats.tool_result_chars <= 450);
        assert!(out.messages.iter().any(|m| {
            m["role"] == "tool"
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("tool-result context bucket")
        }));
    }

    #[test]
    fn context_edit_enforces_history_bucket_before_global_limit() {
        let mut s = Session::new("sys");
        for i in 0..12 {
            s.add_user_text(&format!("old user {i} {}", "x".repeat(80)));
            s.add_assistant(&format!("old answer {i} {}", "y".repeat(80)), None, "");
        }

        let out = s.for_model_edited(
            &[],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 100_000,
                keep_recent_messages: 4,
                max_tool_result_chars: 10_000,
                max_history_chars: 1_500,
                max_tool_result_total_chars: 10_000,
            },
        );

        assert!(out.stats.dropped_messages > 0);
        assert!(out.stats.summary_checkpoints > 0);
        assert!(out.stats.edited_chars < out.stats.original_chars);
        assert_eq!(out.messages[0]["role"], "system");
        assert!(out.messages.iter().any(|m| {
            m["role"] == "assistant"
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("[context summary checkpoint]")
        }));
    }

    #[test]
    fn context_regression_keeps_runtime_notes_when_history_and_tools_compete() {
        let mut s = Session::new("system contract");
        for i in 0..9 {
            let id = format!("call-{i}");
            s.add_user_text(&format!(
                "inspect migration shard {i} for auth latency regression"
            ));
            s.add_assistant(
                "",
                Some(vec![json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                })]),
                "",
            );
            s.add_tool_result(
                &format!("call-{i}"),
                "shell",
                &format!("auth latency shard {i}\n{}", "x".repeat(900)),
            );
        }
        s.add_user_text("continue the auth latency regression migration");

        let out = s.for_model_edited(
            &[
                "Runtime task budget: model=3 tool=8".into(),
                "[memory recall for this prompt]\n- auth latency regression uses migration shard notes"
                    .into(),
            ],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 1_800,
                keep_recent_messages: 1,
                max_tool_result_chars: 64,
                max_history_chars: 1_000,
                max_tool_result_total_chars: 420,
            },
        );

        assert!(out.stats.edited_chars < out.stats.original_chars);
        assert!(out.stats.compressed_tool_results > 0);
        assert!(out.stats.dropped_messages > 0);
        assert!(out.stats.summary_checkpoints > 0);
        assert!(out.stats.tool_result_chars <= 420, "{:?}", out.stats);
        assert_eq!(out.messages[0]["role"], "system");
        assert!(out.messages.iter().any(|m| {
            m["role"] == "system"
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Runtime task budget")
        }));
        assert!(out.messages.iter().any(|m| {
            m["role"] == "system"
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("[memory recall for this prompt]")
        }));
        let summary = summary_checkpoint_text(&out.messages);
        assert!(summary.contains("tools_seen: shell"), "{summary}");
        assert!(summary.contains("focus_anchors"), "{summary}");
        assert!(
            summary.contains("auth latency regression")
                || summary.contains("migration shard"),
            "{summary}"
        );
        assert_valid_tool_protocol(&out.messages);
    }

    #[test]
    fn context_regression_preserves_recent_tool_call_pair() {
        let mut s = Session::new("sys");
        for i in 0..10 {
            s.add_user_text(&format!("old unrelated topic {i} {}", "x".repeat(90)));
            s.add_assistant(&format!("old answer {i} {}", "y".repeat(90)), None, "");
        }
        s.add_user_text("inspect final failing build log");
        s.add_assistant(
            "",
            Some(vec![json!({
                "id": "recent-tool",
                "type": "function",
                "function": {"name": "read_file", "arguments": "{\"path\":\"build.log\"}"}
            })]),
            "",
        );
        s.add_tool_result("recent-tool", "read_file", "final build failure root cause");
        s.add_user_text("summarize the final failing build log root cause");

        let out = s.for_model_edited(
            &[],
            &ContextEditPolicy {
                enabled: true,
                max_chars: 1_400,
                keep_recent_messages: 4,
                max_tool_result_chars: 32,
                max_history_chars: 900,
                max_tool_result_total_chars: 400,
            },
        );

        assert!(out.stats.dropped_messages > 0);
        assert!(out.stats.summary_checkpoints > 0);
        assert!(out.messages.iter().any(|m| {
            m["role"] == "assistant"
                && m.get("tool_calls")
                    .and_then(|v| v.as_array())
                    .map(|calls| calls.iter().any(|c| c["id"] == "recent-tool"))
                    .unwrap_or(false)
        }));
        assert!(out.messages.iter().any(|m| {
            m["role"] == "tool" && m["tool_call_id"] == "recent-tool"
        }));
        assert_valid_tool_protocol(&out.messages);
    }

    fn summary_checkpoint_text(messages: &[Value]) -> String {
        messages
            .iter()
            .find_map(|m| {
                let content = m["content"].as_str().unwrap_or("");
                content
                    .contains("[context summary checkpoint]")
                    .then(|| content.to_string())
            })
            .unwrap_or_default()
    }

    fn assert_valid_tool_protocol(messages: &[Value]) {
        let mut pending = HashSet::<String>::new();
        for msg in messages {
            match role(msg) {
                Some("assistant") => {
                    assert!(
                        pending.is_empty(),
                        "assistant started before tool replies completed: {pending:?}"
                    );
                    if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                        for call in calls {
                            let Some(id) = call.get("id").and_then(|v| v.as_str()) else {
                                continue;
                            };
                            pending.insert(id.to_string());
                        }
                    }
                }
                Some("tool") => {
                    let id = msg
                        .get("tool_call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    assert!(
                        pending.remove(id),
                        "tool result without visible assistant call: {id}"
                    );
                }
                Some("user") | Some("system") => {
                    assert!(
                        pending.is_empty(),
                        "{} started before tool replies completed: {pending:?}",
                        role(msg).unwrap_or("message")
                    );
                }
                _ => {}
            }
        }
        assert!(pending.is_empty(), "dangling tool calls: {pending:?}");
    }

    #[test]
    fn compact_materializes_context_edit_and_rewrites_log() {
        let dir = std::env::temp_dir().join(format!("ncx_session_compact_{}", now_stamp()));
        let path = dir.join("session.jsonl");
        let mut s = Session::with_log("sys", Some(path.clone()));
        for i in 0..8 {
            s.add_user_text(&format!("old user {i} {}", "x".repeat(40)));
            s.add_assistant(&format!("old answer {i} {}", "y".repeat(40)), None, "");
        }
        let before = s.messages.len();

        let stats = s.compact(&ContextEditPolicy {
            enabled: true,
            max_chars: 500,
            keep_recent_messages: 4,
            max_tool_result_chars: 20,
            ..Default::default()
        });

        assert!(stats.dropped_messages > 0);
        assert!(stats.summary_checkpoints > 0);
        assert!(s.messages.len() < before);
        assert_eq!(s.messages[0]["role"], "assistant");
        assert!(s.messages[0]["content"]
            .as_str()
            .unwrap_or("")
            .contains("[context summary checkpoint]"));
        let resumed = Session::resume("fresh", Some(path));
        assert_eq!(resumed.messages.len(), s.messages.len());
        assert_eq!(resumed.messages[0]["role"], "assistant");
        assert!(resumed.messages[0]["content"]
            .as_str()
            .unwrap_or("")
            .contains("[context summary checkpoint]"));
    }

    #[test]
    fn compact_materializes_explicit_focus_hint() {
        let dir = std::env::temp_dir().join(format!(
            "ncx_session_compact_focus_{}",
            now_stamp()
        ));
        let path = dir.join("session.jsonl");
        let mut s = Session::with_log("sys", Some(path.clone()));
        s.add_user_text("semantic memory embedding provider should stay auditable");
        s.add_assistant("embedding provider config stores env var names only", None, "");
        for i in 0..8 {
            s.add_user_text(&format!("unrelated connector audit {i} {}", "x".repeat(40)));
            s.add_assistant(&format!("connector answer {i} {}", "y".repeat(40)), None, "");
        }

        let stats = s.compact_with_focus(
            &ContextEditPolicy {
                enabled: true,
                max_chars: 500,
                keep_recent_messages: 4,
                max_tool_result_chars: 20,
                ..Default::default()
            },
            "semantic memory embedding provider audit",
        );

        assert!(stats.summary_checkpoints > 0);
        let summary = s.messages[0]["content"].as_str().unwrap_or("");
        assert!(
            summary.contains("focus_instruction: semantic memory embedding provider audit"),
            "{summary}"
        );
        assert!(summary.contains("embedding provider"), "{summary}");
        let resumed = Session::resume("fresh", Some(path));
        assert_eq!(
            resumed.messages[0]["content"].as_str().unwrap_or(""),
            summary
        );
    }

    #[test]
    fn compact_noops_when_under_budget() {
        let mut s = Session::new("sys");
        s.add_user_text("hello");
        s.add_assistant("hi", None, "");

        let stats = s.compact(&ContextEditPolicy {
            enabled: true,
            max_chars: 10_000,
            keep_recent_messages: 4,
            max_tool_result_chars: 20,
            ..Default::default()
        });

        assert_eq!(stats.dropped_messages, 0);
        assert_eq!(stats.compressed_tool_results, 0);
        assert_eq!(stats.summary_checkpoints, 0);
        assert_eq!(s.messages.len(), 2);
    }

    #[test]
    fn logs_messages_as_jsonl_and_resumes_body() {
        let dir = std::env::temp_dir().join(format!("ncx_session_log_{}", now_stamp()));
        let path = dir.join("session.jsonl");
        let mut s = Session::with_log("sys", Some(path.clone()));
        s.add_user_text("hello");
        s.add_assistant("hi", None, "");

        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains("\"role\":\"user\""));
        assert!(log.contains("\"_ts\""));

        let resumed = Session::resume("fresh sys", Some(path));
        assert_eq!(resumed.system, "fresh sys");
        assert_eq!(resumed.restored_count, 2);
        assert_eq!(resumed.messages[0]["role"], "user");
        assert_eq!(resumed.messages[1]["content"], "hi");
    }

    #[test]
    fn resume_backfills_dangling_tool_call() {
        let dir = std::env::temp_dir().join(format!("ncx_session_resume_{}", now_stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"role":"system","content":"old sys"}"#,
                "\n",
                r#"{"role":"assistant","content":"","tool_calls":[{"id":"call_1","function":{"name":"shell"}}]}"#,
            ),
        )
        .unwrap();

        let resumed = Session::resume("sys", Some(path));
        assert_eq!(resumed.messages.len(), 2);
        assert_eq!(resumed.messages[1]["role"], "tool");
        assert_eq!(resumed.messages[1]["tool_call_id"], "call_1");
        assert!(resumed.messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("interrupted"));
    }

    #[test]
    fn log_redacts_inline_image_data() {
        let dir = std::env::temp_dir().join(format!("ncx_session_image_{}", now_stamp()));
        let path = dir.join("session.jsonl");
        let mut s = Session::with_log("sys", Some(path.clone()));
        s.add_user(json!([
            {"type": "text", "text": "describe"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
        ]));

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("[image omitted from log]"));
        assert!(!log.contains("data:image"));
    }

    #[test]
    fn fork_uses_seed_without_touching_source_log() {
        let dir = std::env::temp_dir().join(format!("ncx_session_fork_{}", now_stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.jsonl");
        std::fs::write(&source, "{\"role\":\"user\",\"content\":\"original\"}\n").unwrap();
        let before = std::fs::read_to_string(&source).unwrap();
        let fork_log = dir.join("fork.jsonl");

        let mut forked = Session::fork(
            "fresh",
            vec![
                json!({"role": "system", "content": "old"}),
                json!({"role": "user", "content": "original"}),
            ],
            Some(fork_log.clone()),
        );
        forked.add_user_text("new");

        assert_eq!(std::fs::read_to_string(source).unwrap(), before);
        assert!(std::fs::read_to_string(fork_log).unwrap().contains("new"));
        assert_eq!(forked.restored_count, 1);
    }
}
