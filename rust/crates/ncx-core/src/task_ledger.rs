//! Per-task budget and usage ledger.
//!
//! This is the local equivalent of a small observability ledger: one JSONL row
//! per completed user task, written under the workspace `.nanocodex/` runtime
//! directory so CLI, GUI, release checks, and benchmarks can read the same data.

use std::collections::{BTreeMap, BTreeSet};
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
    pub visible_tools: Vec<String>,
    pub called_tools: Vec<String>,
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
            "visible_tools": self.visible_tools.clone(),
            "called_tools": self.called_tools.clone(),
            "approval_requests": self.approval_requests,
            "stop_reason": self.stop_reason,
            "task_model_budget": self.task_model_budget,
            "task_tool_budget": self.task_tool_budget,
            "usage": self.usage.clone(),
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
            visible_tools: string_vec_field(value, "visible_tools"),
            called_tools: string_vec_field(value, "called_tools"),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLedgerTrend {
    pub tasks: usize,
    pub avg_duration_ms: u64,
    pub budget_exhausted_tasks: usize,
    pub model_budget_used: usize,
    pub model_budget_total: usize,
    pub tool_budget_used: usize,
    pub tool_budget_total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTraceEval {
    pub tasks: usize,
    pub trace_tasks: usize,
    pub legacy_tasks_without_trace: usize,
    pub tasks_with_calls: usize,
    pub tasks_with_misses: usize,
    pub visible_tool_total: usize,
    pub called_tool_events: usize,
    pub visible_called_events: usize,
    pub missed_called_events: usize,
    pub visible_only_tools_total: usize,
    pub tool_search_tasks: usize,
    pub mcp_called_events: usize,
    pub mcp_visible_called_events: usize,
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
        let trend = self.trend(Some(limit));
        let mut out = format!(
            "Task ledger\npath: {}\nlast_tasks: {}\nmodel_calls: {}\ntool_calls: {}\napproval_requests: {}\nwall_time_ms: {}\nprompt_tokens: {}\ncompletion_tokens: {}\ntotal_tokens: {}\nstop_reasons: {}\n\nTrend\navg_duration_ms: {}\nbudget_exhausted: {}/{} ({}%)\nmodel_budget_utilization: {}/{} ({}%)\ntool_budget_utilization: {}/{} ({}%)",
            self.path.display(),
            totals.tasks,
            totals.model_calls,
            totals.tool_calls,
            totals.approval_requests,
            totals.duration_ms,
            prompt,
            completion,
            prompt + completion,
            stop_reasons,
            trend.avg_duration_ms,
            trend.budget_exhausted_tasks,
            trend.tasks,
            pct(trend.budget_exhausted_tasks, trend.tasks),
            trend.model_budget_used,
            trend.model_budget_total,
            pct(trend.model_budget_used, trend.model_budget_total),
            trend.tool_budget_used,
            trend.tool_budget_total,
            pct(trend.tool_budget_used, trend.tool_budget_total),
        );
        out.push_str("\n\nRecent tasks:");
        for row in rows {
            let visible = compact_tool_list(&row.visible_tools, 8);
            let called = compact_tool_list(&row.called_tools, 8);
            let continuation = if row.stop_reason == "task_budget" {
                " continuation=resume_same_session compact_focus_then_continue"
            } else {
                ""
            };
            out.push_str(&format!(
                "\n- [{}] {} model={}/{} tools={}/{} visible={} called={} approvals={} time={}ms session={} model_name={} visible_tools=[{}] called_tools=[{}]{}",
                row.started_at,
                row.stop_reason,
                row.model_calls,
                row.task_model_budget,
                row.tool_calls,
                row.task_tool_budget,
                row.visible_tools.len(),
                row.called_tools.len(),
                row.approval_requests,
                row.duration_ms,
                short_id(&row.session_id),
                row.model,
                visible,
                called,
                continuation
            ));
        }
        out
    }

    pub fn render_tool_trace_eval(&self, limit: usize) -> String {
        let limit = limit.clamp(1, 200);
        let rows = self.recent(limit);
        if rows.is_empty() {
            return format!(
                "Tool trace eval\npath: {}\n\nNo completed task records yet.",
                self.path.display()
            );
        }

        let eval = tool_trace_eval(&rows);
        let avg_visible = if eval.trace_tasks == 0 {
            0
        } else {
            eval.visible_tool_total / eval.trace_tasks
        };
        let mut out = format!(
            "Tool trace eval\npath: {}\nlast_tasks: {}\ntrace_tasks: {}\nlegacy_without_trace: {}\ntasks_with_calls: {}\ntasks_with_misses: {}\ncalled_tool_events: {}\nvisible_called_events: {}\nschema_recall: {}/{} ({}%)\nmissed_called_events: {}\nmcp_schema_recall: {}/{} ({}%)\navg_visible_tools: {}\nvisible_only_tools_total: {}\ntool_search_used_tasks: {}",
            self.path.display(),
            eval.tasks,
            eval.trace_tasks,
            eval.legacy_tasks_without_trace,
            eval.tasks_with_calls,
            eval.tasks_with_misses,
            eval.called_tool_events,
            eval.visible_called_events,
            eval.visible_called_events,
            eval.called_tool_events,
            pct(eval.visible_called_events, eval.called_tool_events),
            eval.missed_called_events,
            eval.mcp_visible_called_events,
            eval.mcp_called_events,
            pct(eval.mcp_visible_called_events, eval.mcp_called_events),
            avg_visible,
            eval.visible_only_tools_total,
            eval.tool_search_tasks,
        );

        let misses = rows
            .iter()
            .filter_map(trace_miss_row)
            .take(10)
            .collect::<Vec<_>>();
        if misses.is_empty() {
            out.push_str("\n\nRecent misses: (none)");
        } else {
            out.push_str("\n\nRecent misses:");
            for (row, missed) in misses {
                out.push_str(&format!(
                    "\n- [{}] session={} missed=[{}] visible=[{}] called=[{}]",
                    row.started_at,
                    short_id(&row.session_id),
                    compact_tool_list(&missed, 8),
                    compact_tool_list(&row.visible_tools, 8),
                    compact_tool_list(&row.called_tools, 8)
                ));
            }
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

    pub fn trend(&self, limit: Option<usize>) -> TaskLedgerTrend {
        let rows = match limit {
            Some(n) => self.recent(n),
            None => self.records(),
        };
        let tasks = rows.len();
        let mut trend = TaskLedgerTrend {
            tasks,
            avg_duration_ms: 0,
            budget_exhausted_tasks: 0,
            model_budget_used: 0,
            model_budget_total: 0,
            tool_budget_used: 0,
            tool_budget_total: 0,
        };
        let mut duration_ms = 0_u64;
        for row in rows {
            duration_ms = duration_ms.saturating_add(row.duration_ms);
            if row.stop_reason == "task_budget" {
                trend.budget_exhausted_tasks += 1;
            }
            trend.model_budget_used += row.model_calls;
            trend.model_budget_total += row.task_model_budget;
            trend.tool_budget_used += row.tool_calls;
            trend.tool_budget_total += row.task_tool_budget;
        }
        if tasks > 0 {
            trend.avg_duration_ms = duration_ms / u64::try_from(tasks).unwrap_or(1);
        }
        trend
    }
}

fn tool_trace_eval(rows: &[TaskLedgerRecord]) -> ToolTraceEval {
    let mut eval = ToolTraceEval {
        tasks: rows.len(),
        trace_tasks: 0,
        legacy_tasks_without_trace: 0,
        tasks_with_calls: 0,
        tasks_with_misses: 0,
        visible_tool_total: 0,
        called_tool_events: 0,
        visible_called_events: 0,
        missed_called_events: 0,
        visible_only_tools_total: 0,
        tool_search_tasks: 0,
        mcp_called_events: 0,
        mcp_visible_called_events: 0,
    };

    for row in rows {
        let has_trace = !row.visible_tools.is_empty() || !row.called_tools.is_empty();
        if has_trace {
            eval.trace_tasks += 1;
        } else if row.tool_calls > 0 {
            eval.legacy_tasks_without_trace += 1;
        }
        if !row.called_tools.is_empty() {
            eval.tasks_with_calls += 1;
        }
        if row.called_tools.iter().any(|name| name == "tool_search") {
            eval.tool_search_tasks += 1;
        }

        let visible = row.visible_tools.iter().cloned().collect::<BTreeSet<_>>();
        let called_unique = row.called_tools.iter().cloned().collect::<BTreeSet<_>>();
        eval.visible_tool_total += visible.len();
        eval.visible_only_tools_total += visible.difference(&called_unique).count();

        let mut missed_in_row = false;
        for called in &row.called_tools {
            eval.called_tool_events += 1;
            let was_visible = visible.contains(called);
            if was_visible {
                eval.visible_called_events += 1;
            } else {
                eval.missed_called_events += 1;
                missed_in_row = true;
            }
            if called.starts_with("mcp__") {
                eval.mcp_called_events += 1;
                if was_visible {
                    eval.mcp_visible_called_events += 1;
                }
            }
        }
        if missed_in_row {
            eval.tasks_with_misses += 1;
        }
    }
    eval
}

fn trace_miss_row(row: &TaskLedgerRecord) -> Option<(&TaskLedgerRecord, Vec<String>)> {
    if row.called_tools.is_empty() {
        return None;
    }
    let visible = row.visible_tools.iter().cloned().collect::<BTreeSet<_>>();
    let mut missed = Vec::new();
    for called in &row.called_tools {
        if !visible.contains(called) && !missed.iter().any(|name| name == called) {
            missed.push(called.clone());
        }
    }
    if missed.is_empty() {
        None
    } else {
        Some((row, missed))
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

fn string_vec_field(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
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

fn pct(numerator: usize, denominator: usize) -> usize {
    if denominator == 0 {
        0
    } else {
        numerator.saturating_mul(100) / denominator
    }
}

fn short_id(id: &str) -> &str {
    id.get(..id.len().min(10)).unwrap_or(id)
}

fn compact_tool_list(tools: &[String], limit: usize) -> String {
    if tools.is_empty() {
        return "(none)".into();
    }
    let mut shown = tools.iter().take(limit).cloned().collect::<Vec<_>>();
    if tools.len() > limit {
        shown.push(format!("+{} more", tools.len() - limit));
    }
    shown.join(",")
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
                visible_tools: vec!["read_file".into(), "shell".into(), "tool_search".into()],
                called_tools: vec!["read_file".into(), "shell".into(), "read_file".into()],
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
                visible_tools: vec!["read_file".into(), "tool_search".into()],
                called_tools: Vec::new(),
                approval_requests: 0,
                stop_reason: "task_budget".into(),
                task_model_budget: 1,
                task_tool_budget: 0,
                usage,
            })
            .unwrap();

        let rows = ledger.records();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].visible_tools,
            vec![
                "read_file".to_string(),
                "shell".to_string(),
                "tool_search".to_string()
            ]
        );
        assert_eq!(
            rows[0].called_tools,
            vec![
                "read_file".to_string(),
                "shell".to_string(),
                "read_file".to_string()
            ]
        );
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
        let trend = ledger.trend(None);
        assert_eq!(trend.tasks, 2);
        assert_eq!(trend.avg_duration_ms, 37);
        assert_eq!(trend.budget_exhausted_tasks, 1);
        assert_eq!(trend.model_budget_used, 3);
        assert_eq!(trend.model_budget_total, 6);
        assert_eq!(trend.tool_budget_used, 3);
        assert_eq!(trend.tool_budget_total, 8);
        let report = ledger.render_report(20);
        assert!(report.contains("Task ledger"));
        assert!(report.contains("approval_requests: 1"));
        assert!(report.contains("stop_reasons: completed=1, task_budget=1"));
        assert!(report.contains("Trend"));
        assert!(report.contains("avg_duration_ms: 37"));
        assert!(report.contains("budget_exhausted: 1/2 (50%)"));
        assert!(report.contains("model_budget_utilization: 3/6 (50%)"));
        assert!(report.contains("tool_budget_utilization: 3/8 (37%)"));
        assert!(report.contains("Recent tasks:"));
        assert!(report.contains("visible_tools=[read_file,tool_search]"));
        assert!(report.contains("called_tools=[(none)]"));
        assert!(report.contains(
            "continuation=resume_same_session compact_focus_then_continue"
        ));
    }

    #[test]
    fn tool_trace_eval_reports_schema_recall_and_misses() {
        let dir = std::env::temp_dir().join(format!(
            "ncx_tool_trace_eval_{}",
            new_session_id()
        ));
        let ledger = TaskLedger::new(&dir);
        ledger
            .append(&TaskLedgerRecord {
                session_id: "s1".into(),
                workspace: dir.display().to_string(),
                model: "m".into(),
                started_at: "1".into(),
                duration_ms: 10,
                model_calls: 1,
                tool_calls: 2,
                visible_tools: vec![
                    "read_file".into(),
                    "shell".into(),
                    "tool_search".into(),
                ],
                called_tools: vec!["read_file".into(), "shell".into()],
                approval_requests: 0,
                stop_reason: "completed".into(),
                task_model_budget: 5,
                task_tool_budget: 8,
                usage: BTreeMap::new(),
            })
            .unwrap();
        ledger
            .append(&TaskLedgerRecord {
                session_id: "s2".into(),
                workspace: dir.display().to_string(),
                model: "m".into(),
                started_at: "2".into(),
                duration_ms: 20,
                model_calls: 1,
                tool_calls: 2,
                visible_tools: vec!["tool_search".into()],
                called_tools: vec!["mcp__github__search_issues".into(), "tool_search".into()],
                approval_requests: 0,
                stop_reason: "completed".into(),
                task_model_budget: 5,
                task_tool_budget: 8,
                usage: BTreeMap::new(),
            })
            .unwrap();
        ledger
            .append(&TaskLedgerRecord {
                session_id: "legacy".into(),
                workspace: dir.display().to_string(),
                model: "m".into(),
                started_at: "3".into(),
                duration_ms: 30,
                model_calls: 1,
                tool_calls: 1,
                visible_tools: Vec::new(),
                called_tools: Vec::new(),
                approval_requests: 0,
                stop_reason: "completed".into(),
                task_model_budget: 5,
                task_tool_budget: 8,
                usage: BTreeMap::new(),
            })
            .unwrap();

        let report = ledger.render_tool_trace_eval(20);
        assert!(report.contains("last_tasks: 3"));
        assert!(report.contains("trace_tasks: 2"));
        assert!(report.contains("legacy_without_trace: 1"));
        assert!(report.contains("called_tool_events: 4"));
        assert!(report.contains("schema_recall: 3/4 (75%)"));
        assert!(report.contains("missed_called_events: 1"));
        assert!(report.contains("mcp_schema_recall: 0/1 (0%)"));
        assert!(report.contains("tool_search_used_tasks: 1"));
        assert!(report.contains("missed=[mcp__github__search_issues]"));
    }
}
