//! Per-task budget and usage ledger.
//!
//! This is the local equivalent of a small observability ledger: one JSONL row
//! per completed user task, written under the workspace `.nanocodex/` runtime
//! directory so CLI, GUI, release checks, and benchmarks can read the same data.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLedgerRecord {
    pub session_id: String,
    pub workspace: String,
    pub model: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub model_calls: usize,
    pub tool_calls: usize,
    pub approval_requests: usize,
    pub stop_reason: String,
    pub task_model_budget: usize,
    pub task_tool_budget: usize,
    pub usage: BTreeMap<String, i64>,
}

impl TaskLedgerRecord {
    pub fn to_value(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "workspace": self.workspace,
            "model": self.model,
            "started_at": self.started_at,
            "duration_ms": self.duration_ms,
            "model_calls": self.model_calls,
            "tool_calls": self.tool_calls,
            "approval_requests": self.approval_requests,
            "stop_reason": self.stop_reason,
            "task_model_budget": self.task_model_budget,
            "task_tool_budget": self.task_tool_budget,
            "usage": self.usage,
        })
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        Some(TaskLedgerRecord {
            session_id: string_field(value, "session_id"),
            workspace: string_field(value, "workspace"),
            model: string_field(value, "model"),
            started_at: string_field(value, "started_at"),
            duration_ms: u64_field(value, "duration_ms"),
            model_calls: usize_field(value, "model_calls"),
            tool_calls: usize_field(value, "tool_calls"),
            approval_requests: usize_field(value, "approval_requests"),
            stop_reason: string_field(value, "stop_reason"),
            task_model_budget: usize_field(value, "task_model_budget"),
            task_tool_budget: usize_field(value, "task_tool_budget"),
            usage: value
                .get("usage")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_i64().map(|n| (k.clone(), n)))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLedgerTotals {
    pub tasks: usize,
    pub model_calls: usize,
    pub tool_calls: usize,
    pub approval_requests: usize,
    pub duration_ms: u64,
    pub usage: BTreeMap<String, i64>,
    pub stop_reasons: BTreeMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct TaskLedger {
    path: PathBuf,
}

impl TaskLedger {
    pub fn new(workspace: &Path) -> Self {
        TaskLedger {
            path: workspace.join(".nanocodex").join("task-ledger.jsonl"),
        }
    }

    pub fn at(path: PathBuf) -> Self {
        TaskLedger { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: &TaskLedgerRecord) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(&record.to_value()).unwrap_or_else(|_| "{}".into());
        writeln!(file, "{line}")
    }

    pub fn records(&self) -> Vec<TaskLedgerRecord> {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|v| TaskLedgerRecord::from_value(&v))
            .collect()
    }

    pub fn recent(&self, limit: usize) -> Vec<TaskLedgerRecord> {
        let mut rows = self.records();
        rows.reverse();
        rows.truncate(limit);
        rows
    }

    pub fn render_report(&self, limit: usize) -> String {
        let limit = limit.clamp(1, 200);
        let rows = self.recent(limit);
        if rows.is_empty() {
            return format!(
                "Task ledger\npath: {}\n\nNo completed task records yet.",
                self.path.display()
            );
        }
        let totals = self.totals(Some(limit));
        let stop_reasons = if totals.stop_reasons.is_empty() {
            "(none)".into()
        } else {
            totals
                .stop_reasons
                .iter()
                .map(|(reason, count)| format!("{reason}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let prompt = usage_value(&totals.usage, "prompt_tokens");
        let completion = usage_value(&totals.usage, "completion_tokens");
        let mut out = format!(
            "Task ledger\npath: {}\nlast_tasks: {}\nmodel_calls: {}\ntool_calls: {}\napproval_requests: {}\nwall_time_ms: {}\nprompt_tokens: {}\ncompletion_tokens: {}\ntotal_tokens: {}\nstop_reasons: {}",
            self.path.display(),
            totals.tasks,
            totals.model_calls,
            totals.tool_calls,
            totals.approval_requests,
            totals.duration_ms,
            prompt,
            completion,
            prompt + completion,
            stop_reasons
        );
        out.push_str("\n\nRecent tasks:");
        for row in rows {
            out.push_str(&format!(
                "\n- [{}] {} model={}/{} tools={}/{} approvals={} time={}ms session={} model_name={}",
                row.started_at,
                row.stop_reason,
                row.model_calls,
                row.task_model_budget,
                row.tool_calls,
                row.task_tool_budget,
                row.approval_requests,
                row.duration_ms,
                short_id(&row.session_id),
                row.model
            ));
        }
        out
    }

    pub fn totals(&self, limit: Option<usize>) -> TaskLedgerTotals {
        let rows = match limit {
            Some(n) => self.recent(n),
            None => self.records(),
        };
        let mut totals = TaskLedgerTotals {
            tasks: rows.len(),
            model_calls: 0,
            tool_calls: 0,
            approval_requests: 0,
            duration_ms: 0,
            usage: BTreeMap::new(),
            stop_reasons: BTreeMap::new(),
        };
        for row in rows {
            totals.model_calls += row.model_calls;
            totals.tool_calls += row.tool_calls;
            totals.approval_requests += row.approval_requests;
            totals.duration_ms = totals.duration_ms.saturating_add(row.duration_ms);
            *totals.stop_reasons.entry(row.stop_reason).or_insert(0) += 1;
            for (k, v) in row.usage {
                *totals.usage.entry(k).or_insert(0) += v;
            }
        }
        totals
    }
}

pub fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
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

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn usage_value(usage: &BTreeMap<String, i64>, key: &str) -> i64 {
    usage.get(key).copied().unwrap_or(0)
}

fn short_id(id: &str) -> &str {
    id.get(..id.len().min(10)).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_index::new_session_id;

    #[test]
    fn ledger_appends_reads_recent_and_totals() {
        let dir = std::env::temp_dir().join(format!("ncx_task_ledger_{}", new_session_id()));
        let ledger = TaskLedger::new(&dir);
        let mut usage = BTreeMap::new();
        usage.insert("prompt_tokens".into(), 100);
        usage.insert("completion_tokens".into(), 40);
        ledger
            .append(&TaskLedgerRecord {
                session_id: "s1".into(),
                workspace: dir.display().to_string(),
                model: "m".into(),
                started_at: "1".into(),
                duration_ms: 25,
                model_calls: 2,
                tool_calls: 3,
                approval_requests: 1,
                stop_reason: "completed".into(),
                task_model_budget: 5,
                task_tool_budget: 8,
                usage: usage.clone(),
            })
            .unwrap();
        ledger
            .append(&TaskLedgerRecord {
                session_id: "s2".into(),
                workspace: dir.display().to_string(),
                model: "m".into(),
                started_at: "2".into(),
                duration_ms: 50,
                model_calls: 1,
                tool_calls: 0,
                approval_requests: 0,
                stop_reason: "task_budget".into(),
                task_model_budget: 1,
                task_tool_budget: 0,
                usage,
            })
            .unwrap();

        let rows = ledger.records();
        assert_eq!(rows.len(), 2);
        assert_eq!(ledger.recent(1)[0].session_id, "s2");

        let totals = ledger.totals(None);
        assert_eq!(totals.tasks, 2);
        assert_eq!(totals.model_calls, 3);
        assert_eq!(totals.tool_calls, 3);
        assert_eq!(totals.approval_requests, 1);
        assert_eq!(totals.duration_ms, 75);
        assert_eq!(totals.usage.get("prompt_tokens"), Some(&200));
        assert_eq!(totals.stop_reasons.get("completed"), Some(&1));
        assert_eq!(totals.stop_reasons.get("task_budget"), Some(&1));
        let report = ledger.render_report(20);
        assert!(report.contains("Task ledger"));
        assert!(report.contains("approval_requests: 1"));
        assert!(report.contains("stop_reasons: completed=1, task_budget=1"));
        assert!(report.contains("Recent tasks:"));
    }
}
