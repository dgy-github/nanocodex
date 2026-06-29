//! Project memory — a tiny "self-evolution" store so the agent gets more
//! project-fit with use. NOT raised IQ: it's accumulated, verified experience
//! (conventions, gotchas, solutions) recalled into the prompt as *leads*.
//!
//! Verified notes are stored in one human-readable markdown file
//! `.ncx/memory/LEARNINGS.md`, one entry per note with a parseable comment
//! header:
//!
//! ```text
//! <!-- ts:1719300000 tags:build,windows -->
//! The GNU linker overflows on cdylib; use crate-type=["lib"].
//! ```
//!
//! Memory proposals are kept separately in `.ncx/memory/PROPOSALS.md` so
//! auto-detected learnings can be reviewed before they become trusted recall.
//! On write: deduplicate (normalized text) and cap to the newest [`MAX_ENTRIES`].
//! On recall: score by a lightweight semantic lexical ranker (keywords, tags,
//! phrases, Jaccard, and a tiny domain synonym map), tie-break by recency, and
//! return a capped block to prepend to the system prompt.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use async_trait::async_trait;

/// Merges several same-topic facts into one concise note (the LLM-backed
/// consolidation). `None` = couldn't summarize → caller keeps the newest.
/// Supplied by the CLI/GUI (uses the fast model); tests use a mock.
#[async_trait(?Send)]
pub trait Summarizer {
    async fn merge(&self, facts: &[String]) -> Option<String>;
}

/// Hard cap so the store can't grow unbounded; oldest are dropped first.
pub const MAX_ENTRIES: usize = 200;
pub const MAX_PROPOSALS: usize = 100;
pub const MAX_HARVEST_PROPOSALS: usize = 20;
const RECALL_HEADER: &str =
    "Project memory (verified notes from past work — treat as leads, verify before acting):";

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEntry {
    /// UNIX epoch seconds when recorded (recency).
    pub ts: u64,
    pub tags: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryProposal {
    /// Stable id used by CLI/GUI review actions.
    pub id: String,
    /// UNIX epoch seconds when proposed.
    pub ts: u64,
    /// Where the proposal came from: remember_tool, gui, auto_fix, release, ...
    pub source: String,
    pub tags: Vec<String>,
    pub text: String,
}

/// Append-only-ish markdown fact store under a project's `.ncx/memory/`.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    path: PathBuf,
    proposal_path: PathBuf,
}

impl MemoryStore {
    /// `dir` is the `.ncx/memory` directory (created on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        MemoryStore {
            path: dir.join("LEARNINGS.md"),
            proposal_path: dir.join("PROPOSALS.md"),
        }
    }

    /// Path to the backing markdown file.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Path to the pending memory proposal queue.
    pub fn proposal_path(&self) -> &std::path::Path {
        &self.proposal_path
    }

    /// Record a verified note. Returns `Ok(false)` if it duplicates an existing
    /// entry (skipped). `now` is the epoch-seconds timestamp (caller supplies it
    /// so this is deterministic / testable).
    pub fn remember(&self, text: &str, tags: &[String], now: u64) -> std::io::Result<bool> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(false);
        }
        let mut entries = self.entries();
        let norm = normalize(text);
        if entries.iter().any(|e| normalize(&e.text) == norm) {
            return Ok(false); // dedup
        }
        entries.push(MemoryEntry {
            ts: now,
            tags: tags.to_vec(),
            text: text.to_string(),
        });
        // Cap: keep the newest MAX_ENTRIES (stable by ts ascending, drop front).
        entries.sort_by_key(|e| e.ts);
        if entries.len() > MAX_ENTRIES {
            let drop = entries.len() - MAX_ENTRIES;
            entries.drain(0..drop);
        }
        self.write_all(&entries)?;
        Ok(true)
    }

    /// Queue a candidate learning for human/model review. Returns `Ok(None)`
    /// when the proposal is empty, already trusted, or already pending.
    pub fn propose(
        &self,
        text: &str,
        tags: &[String],
        source: &str,
        now: u64,
    ) -> std::io::Result<Option<MemoryProposal>> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let norm = normalize(text);
        if self.entries().iter().any(|e| normalize(&e.text) == norm) {
            return Ok(None);
        }
        let mut proposals = self.proposals();
        if proposals.iter().any(|p| normalize(&p.text) == norm) {
            return Ok(None);
        }
        let tags = clean_tags(tags);
        let source = clean_source(source);
        let proposal = MemoryProposal {
            id: proposal_id(now, &source, &tags, text),
            ts: now,
            source,
            tags,
            text: text.to_string(),
        };
        proposals.push(proposal.clone());
        proposals.sort_by_key(|p| p.ts);
        if proposals.len() > MAX_PROPOSALS {
            let drop = proposals.len() - MAX_PROPOSALS;
            proposals.drain(0..drop);
        }
        self.write_proposals(&proposals)?;
        Ok(Some(proposal))
    }

    /// Extract candidate learnings from a handoff/release/checklist document
    /// into the pending proposal queue. This deliberately uses conservative
    /// heuristics and keeps the review step mandatory.
    pub fn harvest_proposals_from_text(
        &self,
        source: &str,
        text: &str,
        now: u64,
    ) -> std::io::Result<Vec<MemoryProposal>> {
        let candidates = extract_memory_candidates(source, text);
        let mut created = Vec::new();
        for (i, (note, tags)) in candidates.into_iter().enumerate() {
            if let Some(p) = self.propose(&note, &tags, source, now + i as u64)? {
                created.push(p);
            }
        }
        Ok(created)
    }

    /// Parse pending memory proposals (empty if absent / unreadable).
    pub fn proposals(&self) -> Vec<MemoryProposal> {
        let Ok(text) = std::fs::read_to_string(&self.proposal_path) else {
            return Vec::new();
        };
        parse_proposals(&text)
    }

    /// Accept a pending proposal into verified project memory and remove it from
    /// the review queue. Returns `Ok(false)` when no proposal has that id.
    pub fn accept_proposal(&self, id: &str, now: u64) -> std::io::Result<bool> {
        let mut proposals = self.proposals();
        let Some(pos) = proposals.iter().position(|p| p.id == id.trim()) else {
            return Ok(false);
        };
        let proposal = proposals.remove(pos);
        let _ = self.remember(&proposal.text, &proposal.tags, now)?;
        self.write_proposals(&proposals)?;
        Ok(true)
    }

    /// Edit a pending proposal before accepting it. Returns `Ok(false)` for a
    /// missing id, empty text, or text that duplicates trusted/other pending
    /// memory.
    pub fn update_proposal(
        &self,
        id: &str,
        text: &str,
        tags: &[String],
    ) -> std::io::Result<bool> {
        let id = id.trim();
        let text = text.trim();
        if id.is_empty() || text.is_empty() {
            return Ok(false);
        }
        let norm = normalize(text);
        if self.entries().iter().any(|e| normalize(&e.text) == norm) {
            return Ok(false);
        }
        let mut proposals = self.proposals();
        let Some(pos) = proposals.iter().position(|p| p.id == id) else {
            return Ok(false);
        };
        if proposals
            .iter()
            .enumerate()
            .any(|(i, p)| i != pos && normalize(&p.text) == norm)
        {
            return Ok(false);
        }
        proposals[pos].text = text.to_string();
        proposals[pos].tags = clean_tags(tags);
        self.write_proposals(&proposals)?;
        Ok(true)
    }

    /// Accept every pending proposal into verified memory. Returns the number
    /// of proposals removed from the queue.
    pub fn accept_all_proposals(&self, now: u64) -> std::io::Result<usize> {
        let proposals = self.proposals();
        if proposals.is_empty() {
            return Ok(0);
        }
        for (i, proposal) in proposals.iter().enumerate() {
            let _ = self.remember(&proposal.text, &proposal.tags, now + i as u64)?;
        }
        self.write_proposals(&[])?;
        Ok(proposals.len())
    }

    /// Reject a pending proposal and remove it from the review queue. Returns
    /// `Ok(false)` when no proposal has that id.
    pub fn reject_proposal(&self, id: &str) -> std::io::Result<bool> {
        let mut proposals = self.proposals();
        let before = proposals.len();
        proposals.retain(|p| p.id != id.trim());
        if proposals.len() == before {
            return Ok(false);
        }
        self.write_proposals(&proposals)?;
        Ok(true)
    }

    /// Reject every pending proposal. Returns how many were removed.
    pub fn reject_all_proposals(&self) -> std::io::Result<usize> {
        let proposals = self.proposals();
        if proposals.is_empty() {
            return Ok(0);
        }
        let count = proposals.len();
        self.write_proposals(&[])?;
        Ok(count)
    }

    /// Parse all stored entries (empty if the file is absent / unreadable).
    pub fn entries(&self) -> Vec<MemoryEntry> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        parse_entries(&text)
    }

    /// Build a recall block for the system prompt: entries scored by keyword
    /// overlap with `query` (tie-break recency), capped by count and chars.
    /// Empty string when nothing is stored.
    pub fn recall(&self, query: &str, max_entries: usize, max_chars: usize) -> String {
        let entries = self.entries();
        if entries.is_empty() {
            return String::new();
        }
        let qwords = expanded_keywords(query);
        let qset = word_set(query);
        let qphrases = phrases(query);
        let mut scored: Vec<(i64, &MemoryEntry)> = entries
            .iter()
            .map(|e| {
                let overlap = semantic_score(e, &qwords, &qset, &qphrases);
                // Pack recency into the low bits so higher ts wins ties.
                let s = overlap * 1_000_000 + (e.ts.min(999_999) as i64);
                (s, e)
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));

        let mut out = String::from(RECALL_HEADER);
        let mut used = out.len();
        let mut shown = 0;
        for (_, e) in scored {
            if shown >= max_entries {
                break;
            }
            let line = format!("\n- {}", e.text.replace('\n', " "));
            if used + line.len() > max_chars {
                break;
            }
            out.push_str(&line);
            used += line.len();
            shown += 1;
        }
        if shown == 0 {
            String::new()
        } else {
            out
        }
    }

    /// Periodic consolidation: drop NEAR-duplicates (not just exact), keeping
    /// the newest of each similar cluster, then re-cap. Idempotent (running it
    /// again removes nothing). Returns how many entries were removed. Cheap to
    /// call on every session start.
    pub fn consolidate(&self, similarity_threshold: f64) -> std::io::Result<usize> {
        let mut entries = self.entries();
        if entries.len() < 2 {
            return Ok(0);
        }
        // Newest first, so the kept representative of a cluster is the latest.
        entries.sort_by(|a, b| b.ts.cmp(&a.ts));
        let mut kept: Vec<(MemoryEntry, std::collections::HashSet<String>)> = Vec::new();
        let mut removed = 0usize;
        for e in entries {
            let ws = word_set(&e.text);
            let dup = kept
                .iter()
                .any(|(_, kws)| jaccard(kws, &ws) >= similarity_threshold);
            if dup {
                removed += 1;
            } else {
                kept.push((e, ws));
            }
        }
        let mut out: Vec<MemoryEntry> = kept.into_iter().map(|(e, _)| e).collect();
        out.sort_by_key(|e| e.ts);
        if out.len() > MAX_ENTRIES {
            let drop = out.len() - MAX_ENTRIES;
            out.drain(0..drop);
            removed += drop;
        }
        if removed > 0 {
            self.write_all(&out)?;
        }
        Ok(removed)
    }

    /// LLM-backed consolidation: cluster near-duplicates (Jaccard ≥ threshold)
    /// and, for each cluster of >1, ask `summarizer` to fold them into ONE note
    /// (keeping the newest timestamp + the union of tags). If the summarizer
    /// returns `None`, fall back to keeping the cluster's newest entry (same as
    /// the heuristic [`consolidate`]). Returns how many entries were removed.
    pub async fn summarize_consolidate(
        &self,
        summarizer: &dyn Summarizer,
        threshold: f64,
    ) -> std::io::Result<usize> {
        let entries = self.entries();
        if entries.len() < 2 {
            return Ok(0);
        }
        let before = entries.len();
        let mut sorted = entries;
        sorted.sort_by(|a, b| b.ts.cmp(&a.ts)); // newest first

        // Greedy single-link clustering by word-set similarity.
        let mut clusters: Vec<Vec<MemoryEntry>> = Vec::new();
        let mut reps: Vec<std::collections::HashSet<String>> = Vec::new();
        for e in sorted {
            let ws = word_set(&e.text);
            let mut placed = false;
            for (i, rep) in reps.iter().enumerate() {
                if jaccard(rep, &ws) >= threshold {
                    clusters[i].push(e.clone());
                    placed = true;
                    break;
                }
            }
            if !placed {
                reps.push(ws);
                clusters.push(vec![e]);
            }
        }

        let mut out: Vec<MemoryEntry> = Vec::new();
        for cluster in clusters {
            if cluster.len() == 1 {
                out.push(cluster.into_iter().next().unwrap());
                continue;
            }
            let ts = cluster.iter().map(|e| e.ts).max().unwrap_or(0);
            let mut tags: Vec<String> = Vec::new();
            for e in &cluster {
                for t in &e.tags {
                    if !tags.contains(t) {
                        tags.push(t.clone());
                    }
                }
            }
            let texts: Vec<String> = cluster.iter().map(|e| e.text.clone()).collect();
            match summarizer.merge(&texts).await {
                Some(m) if !m.trim().is_empty() => {
                    out.push(MemoryEntry {
                        ts,
                        tags,
                        text: m.trim().to_string(),
                    });
                }
                _ => {
                    // Summarizer unavailable → keep the newest (heuristic behavior).
                    let newest = cluster.into_iter().max_by_key(|e| e.ts).unwrap();
                    out.push(newest);
                }
            }
        }

        out.sort_by_key(|e| e.ts);
        if out.len() > MAX_ENTRIES {
            let drop = out.len() - MAX_ENTRIES;
            out.drain(0..drop);
        }
        let removed = before.saturating_sub(out.len());
        if removed > 0 {
            self.write_all(&out)?;
        }
        Ok(removed)
    }

    fn write_all(&self, entries: &[MemoryEntry]) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut s = String::from("# Project memory (nanocodex)\n\n");
        for e in entries {
            s.push_str(&format!(
                "<!-- ts:{} tags:{} -->\n{}\n\n",
                e.ts,
                e.tags.join(","),
                e.text
            ));
        }
        std::fs::write(&self.path, s)
    }

    fn write_proposals(&self, proposals: &[MemoryProposal]) -> std::io::Result<()> {
        if let Some(parent) = self.proposal_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut s = String::from("# Project memory proposals (nanocodex)\n\n");
        for p in proposals {
            s.push_str(&format!(
                "<!-- id:{} ts:{} source:{} tags:{} -->\n{}\n\n",
                p.id,
                p.ts,
                p.source,
                p.tags.join(","),
                p.text
            ));
        }
        std::fs::write(&self.proposal_path, s)
    }
}

fn proposal_id(now: u64, source: &str, tags: &[String], text: &str) -> String {
    let mut h = DefaultHasher::new();
    now.hash(&mut h);
    source.hash(&mut h);
    tags.hash(&mut h);
    normalize(text).hash(&mut h);
    format!("p{now:x}-{:x}", h.finish())
}

fn clean_source(source: &str) -> String {
    let source = source.trim();
    if source.is_empty() {
        "manual".into()
    } else {
        source.split_whitespace().collect::<Vec<_>>().join("_")
    }
}

fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim();
        if !tag.is_empty() && !out.iter().any(|t: &String| t == tag) {
            out.push(tag.to_string());
        }
    }
    out
}

fn extract_memory_candidates(source: &str, text: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut section_tags: Vec<String> = Vec::new();
    let mut in_code = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            section_tags = proposal_tags_for(source, line);
            continue;
        }
        let is_checklist = is_markdown_checklist(line);
        let Some(note) = clean_candidate_line(line) else {
            continue;
        };
        let len = note.chars().count();
        if !(18..=220).contains(&len) {
            continue;
        }
        if !is_checklist && !line_worth_proposing(&note) {
            continue;
        }
        if out.iter().any(|(existing, _)| normalize(existing) == normalize(&note)) {
            continue;
        }
        let mut tags = proposal_tags_for(source, &note);
        for tag in &section_tags {
            if !tags.contains(tag) {
                tags.push(tag.clone());
            }
        }
        out.push((note, tags));
        if out.len() >= MAX_HARVEST_PROPOSALS {
            break;
        }
    }
    out
}

fn clean_candidate_line(line: &str) -> Option<String> {
    let mut s = line.trim();
    if s.contains('|') && s.matches('|').count() > 1 {
        return None;
    }
    while let Some(rest) = s.strip_prefix('>') {
        s = rest.trim();
    }
    for prefix in [
        "- [ ]", "- [x]", "- [X]", "* [ ]", "* [x]", "* [X]", "+ [ ]", "+ [x]", "+ [X]",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
            break;
        }
    }
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
            break;
        }
    }
    if let Some((idx, ch)) = s
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
    {
        if idx > 0 && (ch == '.' || ch == ')') {
            s = s[idx + ch.len_utf8()..].trim();
        }
    }
    let s = s
        .trim_matches('`')
        .trim_matches('*')
        .trim_matches('_')
        .trim()
        .to_string();
    if s.starts_with("http://") || s.starts_with("https://") || s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn is_markdown_checklist(line: &str) -> bool {
    let s = line.trim_start();
    ["- [ ]", "- [x]", "- [X]", "* [ ]", "* [x]", "* [X]", "+ [ ]", "+ [x]", "+ [X]"]
        .iter()
        .any(|prefix| s.starts_with(prefix))
}

fn line_worth_proposing(note: &str) -> bool {
    let lower = note.to_lowercase();
    let markers = [
        "must",
        "should",
        "need to",
        "remember",
        "avoid",
        "prefer",
        "requires",
        "run ",
        "use ",
        "fix",
        "fails",
        "failure",
        "error",
        "gotcha",
        "release",
        "installer",
        "test",
        "cargo",
        "npm",
        "tauri",
        "mcp",
        "context",
        "memory",
        "budget",
        "sandbox",
        "approval",
        "connector",
        "windows",
        "gnu",
        "必须",
        "需要",
        "记得",
        "避免",
        "不要",
        "使用",
        "失败",
        "修复",
        "报错",
        "缺少",
        "运行",
        "测试",
        "发布",
        "打包",
        "安装",
        "配置",
        "审批",
        "沙箱",
        "上下文",
        "记忆",
    ];
    markers.iter().any(|m| lower.contains(m))
}

fn proposal_tags_for(source: &str, note: &str) -> Vec<String> {
    let hay = format!("{} {}", source.to_lowercase(), note.to_lowercase());
    let mut tags = vec!["harvested".to_string()];
    let mut add = |tag: &str| {
        let tag = tag.to_string();
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    };
    if hay.contains("release") || hay.contains("installer") || hay.contains("nsis") || hay.contains("发布") || hay.contains("打包") {
        add("release");
    }
    if hay.contains("build") || hay.contains("cargo") || hay.contains("npm") || hay.contains("test") || hay.contains("构建") || hay.contains("测试") {
        add("build");
    }
    if hay.contains("tauri") || hay.contains("gui") || hay.contains("desktop") || hay.contains("面板") {
        add("gui");
    }
    if hay.contains("mcp") || hay.contains("connector") || hay.contains("oauth") {
        add("mcp");
    }
    if hay.contains("context") || hay.contains("上下文") {
        add("context");
    }
    if hay.contains("memory") || hay.contains("记忆") {
        add("memory");
    }
    if hay.contains("budget") || hay.contains("预算") {
        add("budget");
    }
    if hay.contains("windows") || hay.contains("gnu") || hay.contains("mingw") {
        add("windows");
    }
    if hay.contains("failure") || hay.contains("fails") || hay.contains("error") || hay.contains("失败") || hay.contains("报错") {
        add("failure");
    }
    tags
}

/// Words worth matching on: lowercased, length ≥ 3, deduped.
fn keywords(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in s.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if w.len() >= 3 && !out.iter().any(|x| x == w) {
            out.push(w.to_string());
        }
    }
    out
}

fn expanded_keywords(s: &str) -> Vec<String> {
    let mut out = keywords(s);
    for w in out.clone() {
        for alias in semantic_aliases(&w) {
            if !out.iter().any(|x| x == alias) {
                out.push(alias.to_string());
            }
        }
    }
    out
}

fn semantic_aliases(w: &str) -> &'static [&'static str] {
    match w {
        "desktop" | "native" | "window" => &["gui", "tauri"],
        "package" | "packaging" | "installer" | "bundle" | "release" => {
            &["build", "tauri", "msi", "exe", "distribution"]
        }
        "memory" | "remember" => &["recall", "learning", "learnings"],
        "search" => &["web", "tavily", "duckduckgo", "grep", "glob"],
        "sandbox" => &["approval", "policy", "permission"],
        "parallel" | "worker" => &["orchestrator", "isolate", "verifier"],
        "rust" => &["cargo", "crate", "gnu"],
        _ => &[],
    }
}

/// Hybrid lexical-semantic score of an entry (text + tags) against the query.
fn semantic_score(
    e: &MemoryEntry,
    qwords: &[String],
    qset: &std::collections::HashSet<String>,
    qphrases: &[String],
) -> i64 {
    if qwords.is_empty() {
        return 0;
    }
    let hay = format!(
        "{} {}",
        e.text.to_lowercase(),
        e.tags.join(" ").to_lowercase()
    );
    let entry_set = word_set(&hay);
    let mut score = 0i64;
    for w in qwords {
        if e.tags.iter().any(|t| t.eq_ignore_ascii_case(w)) {
            score += 8;
        } else if hay.contains(w.as_str()) {
            score += 4;
        }
    }
    for p in qphrases {
        if hay.contains(p.as_str()) {
            score += 6;
        }
    }
    score += (jaccard(qset, &entry_set) * 20.0).round() as i64;
    score
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Significant words of an entry (lowercased, length ≥ 3), as a set.
fn word_set(text: &str) -> std::collections::HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect()
}

fn phrases(text: &str) -> Vec<String> {
    let words = keywords(text);
    let mut out = Vec::new();
    for pair in words.windows(2) {
        let p = format!("{} {}", pair[0], pair[1]);
        if !out.iter().any(|x| x == &p) {
            out.push(p);
        }
    }
    out
}

/// Jaccard similarity of two word sets (|∩| / |∪|); 0 when both empty.
fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn parse_entries(text: &str) -> Vec<MemoryEntry> {
    let mut out: Vec<MemoryEntry> = Vec::new();
    let mut cur: Option<(u64, Vec<String>)> = None;
    let mut body: Vec<String> = Vec::new();

    let flush = |cur: &mut Option<(u64, Vec<String>)>,
                 body: &mut Vec<String>,
                 out: &mut Vec<MemoryEntry>| {
        if let Some((ts, tags)) = cur.take() {
            let txt = body.join("\n").trim().to_string();
            if !txt.is_empty() {
                out.push(MemoryEntry {
                    ts,
                    tags,
                    text: txt,
                });
            }
        }
        body.clear();
    };

    for line in text.lines() {
        if let Some(header) = line
            .trim()
            .strip_prefix("<!-- ")
            .and_then(|s| s.strip_suffix(" -->"))
        {
            flush(&mut cur, &mut body, &mut out);
            // header = "ts:<n> tags:<a,b>"
            let mut ts = 0u64;
            let mut tags: Vec<String> = Vec::new();
            for tok in header.split_whitespace() {
                if let Some(v) = tok.strip_prefix("ts:") {
                    ts = v.parse().unwrap_or(0);
                } else if let Some(v) = tok.strip_prefix("tags:") {
                    tags = v
                        .split(',')
                        .filter(|t| !t.is_empty())
                        .map(|t| t.to_string())
                        .collect();
                }
            }
            cur = Some((ts, tags));
        } else if cur.is_some() {
            body.push(line.to_string());
        }
    }
    flush(&mut cur, &mut body, &mut out);
    out
}

fn parse_proposals(text: &str) -> Vec<MemoryProposal> {
    let mut out: Vec<MemoryProposal> = Vec::new();
    let mut cur: Option<(String, u64, String, Vec<String>)> = None;
    let mut body: Vec<String> = Vec::new();

    let flush = |cur: &mut Option<(String, u64, String, Vec<String>)>,
                 body: &mut Vec<String>,
                 out: &mut Vec<MemoryProposal>| {
        if let Some((id, ts, source, tags)) = cur.take() {
            let txt = body.join("\n").trim().to_string();
            if !id.is_empty() && !txt.is_empty() {
                out.push(MemoryProposal {
                    id,
                    ts,
                    source,
                    tags,
                    text: txt,
                });
            }
        }
        body.clear();
    };

    for line in text.lines() {
        if let Some(header) = line
            .trim()
            .strip_prefix("<!-- ")
            .and_then(|s| s.strip_suffix(" -->"))
        {
            flush(&mut cur, &mut body, &mut out);
            let mut id = String::new();
            let mut ts = 0u64;
            let mut source = String::from("manual");
            let mut tags: Vec<String> = Vec::new();
            for tok in header.split_whitespace() {
                if let Some(v) = tok.strip_prefix("id:") {
                    id = v.to_string();
                } else if let Some(v) = tok.strip_prefix("ts:") {
                    ts = v.parse().unwrap_or(0);
                } else if let Some(v) = tok.strip_prefix("source:") {
                    source = clean_source(v);
                } else if let Some(v) = tok.strip_prefix("tags:") {
                    tags = v
                        .split(',')
                        .filter(|t| !t.is_empty())
                        .map(|t| t.to_string())
                        .collect();
                }
            }
            cur = Some((id, ts, source, tags));
        } else if cur.is_some() {
            body.push(line.to_string());
        }
    }
    flush(&mut cur, &mut body, &mut out);
    out
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> MemoryStore {
        let d = std::env::temp_dir().join(format!("ncx_mem_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        MemoryStore::new(d)
    }

    #[test]
    fn remember_then_round_trips() {
        let s = store("rt");
        assert!(s
            .remember(
                "Use crate-type=lib on the gnu toolchain",
                &["build".into()],
                100
            )
            .unwrap());
        let es = s.entries();
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].ts, 100);
        assert_eq!(es[0].tags, vec!["build"]);
        assert!(es[0].text.contains("crate-type"));
    }

    #[test]
    fn dedup_skips_identical() {
        let s = store("dedup");
        assert!(s.remember("same fact", &[], 1).unwrap());
        // different whitespace/case -> still a duplicate
        assert!(!s.remember("  SAME   fact ", &[], 2).unwrap());
        assert_eq!(s.entries().len(), 1);
    }

    #[test]
    fn empty_is_not_stored() {
        let s = store("empty");
        assert!(!s.remember("   ", &[], 1).unwrap());
        assert!(s.entries().is_empty());
    }

    #[test]
    fn proposal_queue_round_trips_and_is_not_recalled() {
        let s = store("proposal_rt");
        let p = s
            .propose(
                "Use the memory review queue before trusting auto-learned facts",
                &["memory".into(), "governance".into()],
                "auto_fix",
                10,
            )
            .unwrap()
            .expect("new proposal");
        assert!(p.id.starts_with("pa-"));
        assert_eq!(p.source, "auto_fix");
        assert_eq!(s.entries().len(), 0);
        assert!(s.recall("memory review", 5, 4000).is_empty());

        let proposals = s.proposals();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].id, p.id);
        assert_eq!(
            proposals[0].text,
            "Use the memory review queue before trusting auto-learned facts"
        );
    }

    #[test]
    fn accepting_proposal_moves_it_to_verified_memory() {
        let s = store("proposal_accept");
        let p = s
            .propose("Keep connector allow-lists tight", &["mcp".into()], "test", 10)
            .unwrap()
            .unwrap();
        assert!(s.accept_proposal(&p.id, 20).unwrap());
        assert!(s.proposals().is_empty());
        let entries = s.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ts, 20);
        assert_eq!(entries[0].text, "Keep connector allow-lists tight");
        assert!(s
            .recall("connector permissions", 5, 4000)
            .contains("allow-lists"));
    }

    #[test]
    fn rejecting_proposal_removes_without_trusting() {
        let s = store("proposal_reject");
        let p = s
            .propose("Speculative memory should stay pending", &[], "test", 10)
            .unwrap()
            .unwrap();
        assert!(s.reject_proposal(&p.id).unwrap());
        assert!(s.proposals().is_empty());
        assert!(s.entries().is_empty());
    }

    #[test]
    fn update_proposal_edits_text_and_tags() {
        let s = store("proposal_update");
        let p = s
            .propose("Draft memory needs cleanup", &["draft".into()], "test", 10)
            .unwrap()
            .unwrap();
        assert!(s
            .update_proposal(
                &p.id,
                "Cleaned memory proposal",
                &["memory".into(), "review".into()],
            )
            .unwrap());
        let proposals = s.proposals();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].text, "Cleaned memory proposal");
        assert_eq!(proposals[0].tags, vec!["memory", "review"]);
        assert_eq!(proposals[0].source, "test");
    }

    #[test]
    fn update_proposal_rejects_duplicates() {
        let s = store("proposal_update_dedup");
        s.remember("Trusted memory", &[], 1).unwrap();
        let p = s
            .propose("Pending memory", &[], "test", 10)
            .unwrap()
            .unwrap();
        assert!(!s.update_proposal(&p.id, "trusted MEMORY", &[]).unwrap());

        let other = s
            .propose("Another pending memory", &[], "test", 11)
            .unwrap()
            .unwrap();
        assert!(!s
            .update_proposal(&other.id, "pending memory", &[])
            .unwrap());
    }

    #[test]
    fn batch_accept_and_reject_clear_queue() {
        let s = store("proposal_batch");
        s.propose("First pending fact", &["one".into()], "test", 10)
            .unwrap();
        s.propose("Second pending fact", &["two".into()], "test", 11)
            .unwrap();
        assert_eq!(s.accept_all_proposals(20).unwrap(), 2);
        assert!(s.proposals().is_empty());
        assert_eq!(s.entries().len(), 2);

        s.propose("Third pending fact", &[], "test", 12).unwrap();
        s.propose("Fourth pending fact", &[], "test", 13).unwrap();
        assert_eq!(s.reject_all_proposals().unwrap(), 2);
        assert!(s.proposals().is_empty());
        assert_eq!(s.entries().len(), 2);
    }

    #[test]
    fn proposals_dedup_against_pending_and_verified() {
        let s = store("proposal_dedup");
        assert!(s.propose("same fact", &[], "test", 1).unwrap().is_some());
        assert!(s.propose("  SAME   fact ", &[], "test", 2).unwrap().is_none());
        assert_eq!(s.proposals().len(), 1);

        assert!(s.remember("trusted fact", &[], 3).unwrap());
        assert!(s.propose("TRUSTED fact", &[], "test", 4).unwrap().is_none());
    }

    #[test]
    fn harvests_release_checklist_into_pending_proposals() {
        let s = store("harvest_release");
        let created = s
            .harvest_proposals_from_text(
                "RELEASE_TASK.md",
                r#"
# Release checklist

- [ ] Run `cmd /c npm run build` before packaging the Tauri installer.
- [ ] Verify the NSIS installer starts without a black console window.
- Ordinary project prose without an action marker.
                "#,
                100,
            )
            .unwrap();
        assert_eq!(created.len(), 2);
        assert!(s.entries().is_empty(), "harvested notes stay pending");
        let proposals = s.proposals();
        assert_eq!(proposals.len(), 2);
        assert!(proposals[0].tags.contains(&"release".to_string()));
        assert!(proposals[0].tags.contains(&"build".to_string()));
        assert!(proposals.iter().any(|p| p.text.contains("NSIS installer")));
    }

    #[test]
    fn harvest_skips_code_tables_and_duplicate_lines() {
        let s = store("harvest_skip");
        let created = s
            .harvest_proposals_from_text(
                "HANDOFF.md",
                r#"
| thing | value |
| --- | --- |
```
- [ ] Do not learn this code block item.
```
- [ ] Keep MCP connector allow-lists tight during release.
- [ ]   KEEP   MCP connector allow-lists tight during release.
- Useful note but no marker here.
                "#,
                10,
            )
            .unwrap();
        assert_eq!(created.len(), 1);
        let proposals = s.proposals();
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].text.contains("MCP connector"));
        assert!(!proposals[0].text.contains("code block"));
        assert!(proposals[0].tags.contains(&"mcp".to_string()));
    }

    #[test]
    fn cap_drops_oldest() {
        let s = store("cap");
        for i in 0..(MAX_ENTRIES as u64 + 5) {
            s.remember(&format!("fact number {i}"), &[], i).unwrap();
        }
        let es = s.entries();
        assert_eq!(es.len(), MAX_ENTRIES);
        // oldest (ts 0..4) dropped; newest kept.
        assert!(es.iter().all(|e| e.ts >= 5));
    }

    #[test]
    fn recall_scores_by_keyword_overlap() {
        let s = store("recall");
        s.remember(
            "Windows GNU linker overflows on cdylib",
            &["build".into()],
            1,
        )
        .unwrap();
        s.remember(
            "The storyboard panel renders thumbnails",
            &["gui".into()],
            2,
        )
        .unwrap();
        let block = s.recall("how do I fix the linker on windows", 5, 4000);
        assert!(block.contains("treat as leads"));
        assert!(block.contains("GNU linker"));
        // The unrelated entry ranks below; with both fitting it still appears,
        // but the linker note must be present and first after the header.
        let first_bullet = block.lines().nth(1).unwrap_or("");
        assert!(first_bullet.contains("linker"), "got: {first_bullet}");
    }

    #[test]
    fn recall_uses_semantic_aliases_and_tags() {
        let s = store("semantic");
        s.remember(
            "Tauri desktop shell builds compact Windows bundles",
            &["gui".into()],
            1,
        )
        .unwrap();
        s.remember("Use cargo test for the Rust workspace", &["test".into()], 2)
            .unwrap();
        let block = s.recall("native installer release package", 1, 4000);
        assert!(block.contains("Tauri desktop"), "got: {block}");
        assert!(!block.contains("cargo test"), "got: {block}");
    }

    #[test]
    fn recall_empty_store_is_blank() {
        let s = store("blank");
        assert_eq!(s.recall("anything", 5, 4000), "");
    }

    #[test]
    fn consolidate_merges_near_duplicates() {
        let s = store("consolidate");
        // Two near-identical notes (≈0.86 Jaccard) + one distinct.
        s.remember("the gnu toolchain is used for the build", &[], 1)
            .unwrap();
        s.remember("the gnu toolchain used for the build here", &[], 2)
            .unwrap();
        s.remember("the storyboard panel renders thumbnails", &[], 3)
            .unwrap();
        let removed = s.consolidate(0.8).unwrap();
        assert_eq!(removed, 1, "one near-dup should be merged away");
        let es = s.entries();
        assert_eq!(es.len(), 2);
        // The newer of the near-dup cluster (ts 2) is the one kept.
        assert!(es.iter().any(|e| e.ts == 2));
        assert!(es.iter().any(|e| e.text.contains("storyboard")));
    }

    struct FixedMerger(&'static str);
    #[async_trait(?Send)]
    impl Summarizer for FixedMerger {
        async fn merge(&self, _facts: &[String]) -> Option<String> {
            if self.0.is_empty() {
                None
            } else {
                Some(self.0.to_string())
            }
        }
    }

    #[tokio::test]
    async fn summarize_merges_cluster_into_one() {
        let s = store("llm_merge");
        s.remember(
            "the gnu toolchain is used for the build",
            &["build".into()],
            1,
        )
        .unwrap();
        s.remember("the gnu toolchain used for the build here", &[], 2)
            .unwrap();
        s.remember("the storyboard panel renders thumbnails", &[], 3)
            .unwrap();
        let removed = s
            .summarize_consolidate(&FixedMerger("gnu toolchain, no MSVC"), 0.8)
            .await
            .unwrap();
        assert_eq!(removed, 1); // the 2-entry cluster folds to one
        let es = s.entries();
        assert_eq!(es.len(), 2);
        let merged = es
            .iter()
            .find(|e| e.text == "gnu toolchain, no MSVC")
            .unwrap();
        assert_eq!(merged.ts, 2, "keeps newest ts of the cluster");
        assert!(
            merged.tags.contains(&"build".to_string()),
            "keeps union of tags"
        );
        assert!(es.iter().any(|e| e.text.contains("storyboard")));
    }

    #[tokio::test]
    async fn summarize_falls_back_to_newest_when_merge_none() {
        let s = store("llm_none");
        s.remember("the gnu toolchain is used for the build", &[], 1)
            .unwrap();
        s.remember("the gnu toolchain used for the build here", &[], 2)
            .unwrap();
        // empty merger → None → fallback keeps the newest of the cluster.
        let removed = s
            .summarize_consolidate(&FixedMerger(""), 0.8)
            .await
            .unwrap();
        assert_eq!(removed, 1);
        let es = s.entries();
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].ts, 2);
    }

    #[test]
    fn consolidate_is_idempotent() {
        let s = store("idem");
        s.remember("alpha beta gamma delta", &[], 1).unwrap();
        s.remember("alpha beta gamma delta epsilon", &[], 2)
            .unwrap();
        let first = s.consolidate(0.8).unwrap();
        assert!(first >= 1);
        let second = s.consolidate(0.8).unwrap();
        assert_eq!(second, 0, "re-running removes nothing");
    }

    #[test]
    fn recall_respects_entry_cap() {
        let s = store("reccap");
        for i in 0..10 {
            s.remember(&format!("note {i} about widgets"), &[], i)
                .unwrap();
        }
        let block = s.recall("widgets", 3, 4000);
        assert_eq!(block.lines().filter(|l| l.starts_with("- ")).count(), 3);
    }
}
