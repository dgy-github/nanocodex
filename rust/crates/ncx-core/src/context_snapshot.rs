//! Provider payload snapshots for context-debugging.
//!
//! Every model call can persist the edited messages actually sent to the
//! provider, plus compact role/tool statistics. This makes send-time context
//! editing auditable without mutating the live session.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::session::{redact_image_data, ContextEditStats};

#[derive(Debug, Clone)]
pub struct ContextPayloadSnapshot {
    pub id: String,
    pub model: String,
    pub sequence: usize,
    pub messages: Vec<Value>,
    pub tool_names: Vec<String>,
    pub stats: ContextEditStats,
}

impl ContextPayloadSnapshot {
    pub fn to_value(&self) -> Value {
        let redacted_messages = self
            .messages
            .iter()
            .map(|m| redact_image_data(m, "[image omitted from context payload snapshot]"))
            .collect::<Vec<_>>();
        let role_counts = role_counts(&redacted_messages);
        let role_chars = role_chars(&redacted_messages);
        json!({
            "id": self.id,
            "model": self.model,
            "sequence": self.sequence,
            "message_count": redacted_messages.len(),
            "role_counts": role_counts,
            "role_chars": role_chars,
            "tool_schema_count": self.tool_names.len(),
            "tool_names": self.tool_names.clone(),
            "context_edit": {
                "original_chars": self.stats.original_chars,
                "edited_chars": self.stats.edited_chars,
                "compressed_tool_results": self.stats.compressed_tool_results,
                "dropped_messages": self.stats.dropped_messages,
            },
            "messages": redacted_messages,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ContextPayloadSnapshotStore {
    root: PathBuf,
}

impl ContextPayloadSnapshotStore {
    pub fn new(workspace: &Path) -> Self {
        ContextPayloadSnapshotStore {
            root: workspace.join(".nanocodex").join("context-payloads"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, snapshot: &ContextPayloadSnapshot) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.root)?;
        let path = self.root.join(format!("{}.json", snapshot.id));
        let bytes =
            serde_json::to_vec_pretty(&snapshot.to_value()).unwrap_or_else(|_| b"{}".to_vec());
        fs::write(&path, bytes)?;
        Ok(path)
    }

    pub fn recent_values(&self, limit: usize) -> Vec<Value> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut files = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        files.sort();
        files.reverse();
        files
            .into_iter()
            .take(limit)
            .filter_map(|path| fs::read_to_string(path).ok())
            .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
            .collect()
    }

    pub fn render_report(&self, limit: usize) -> String {
        let limit = limit.clamp(1, 50);
        let rows = self.recent_values(limit);
        if rows.is_empty() {
            return format!(
                "Context payload snapshots\npath: {}\n\nNo provider payload snapshots yet.",
                self.root.display()
            );
        }
        let mut out = format!(
            "Context payload snapshots\npath: {}\nrecent: {}",
            self.root.display(),
            rows.len()
        );
        for row in rows {
            let id = string_field(&row, "id");
            let model = string_field(&row, "model");
            let message_count = usize_field(&row, "message_count");
            let tool_schema_count = usize_field(&row, "tool_schema_count");
            let stats = row.get("context_edit").unwrap_or(&Value::Null);
            let original = usize_field(stats, "original_chars");
            let edited = usize_field(stats, "edited_chars");
            let compressed = usize_field(stats, "compressed_tool_results");
            let dropped = usize_field(stats, "dropped_messages");
            let roles = row
                .get("role_counts")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .map(|(k, v)| format!("{}={}", k, v.as_u64().unwrap_or(0)))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "(none)".into());
            out.push_str(&format!(
                "\n- {id} model={model} messages={message_count} tools={tool_schema_count} chars={edited}/{original} compressed={compressed} dropped={dropped} roles={roles}"
            ));
        }
        out
    }
}

pub fn new_snapshot_id(sequence: usize) -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:x}-{:04}", sequence)
}

pub fn schema_tool_names(schemas: &[Value]) -> Vec<String> {
    schemas
        .iter()
        .filter_map(|schema| {
            schema
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect()
}

fn role_counts(messages: &[Value]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for msg in messages {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        *out.entry(role.to_string()).or_insert(0) += 1;
    }
    out
}

fn role_chars(messages: &[Value]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for msg in messages {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        *out.entry(role.to_string()).or_insert(0) += json_chars(msg);
    }
    out
}

fn json_chars(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn usize_field(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_index::new_session_id;

    #[test]
    fn snapshot_redacts_images_and_reports_recent_payloads() {
        let dir = std::env::temp_dir().join(format!("ncx_context_snapshot_{}", new_session_id()));
        let store = ContextPayloadSnapshotStore::new(&dir);
        let snapshot = ContextPayloadSnapshot {
            id: "0001".into(),
            model: "test-model".into(),
            sequence: 1,
            messages: vec![
                json!({"role": "system", "content": "sys"}),
                json!({"role": "user", "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,secret"}}
                ]}),
            ],
            tool_names: vec!["read_file".into(), "tool_search".into()],
            stats: ContextEditStats {
                original_chars: 100,
                edited_chars: 80,
                compressed_tool_results: 1,
                dropped_messages: 2,
            },
        };
        let path = store.write(&snapshot).unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("secret"));
        assert!(text.contains("image omitted"));

        let report = store.render_report(5);
        assert!(report.contains("Context payload snapshots"));
        assert!(report.contains("model=test-model"));
        assert!(report.contains("messages=2"));
        assert!(report.contains("tools=2"));
        assert!(report.contains("compressed=1"));
        assert!(report.contains("dropped=2"));
    }
}
