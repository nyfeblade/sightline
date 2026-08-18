//! One Claude Code session: its transcript, running totals, and current state.

use crate::event::{self, Ev, Kind};
use crate::pricing;
use crate::registry::Live;
use crate::tail::Tail;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

/// Feed events kept per session. Totals are accumulated over the whole file;
/// only the tail is kept for display.
pub const MAX_EVENTS: usize = 4000;

#[derive(Default, Clone)]
pub struct Totals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
    /// assistant messages whose model has no known rate
    pub unpriced: usize,
    /// prompt size of the most recent request — how full the context window is
    pub ctx: u64,
    /// assistant messages that carried usage
    pub requests: usize,
}

impl Totals {
    /// Every input token ever billed, cache reads included. Useful for cost,
    /// misleading as a size — a long session re-reads its cached prefix each
    /// turn, so this runs to hundreds of millions.
    pub fn billed_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

/// Everything one file had done to it during the session.
#[derive(Clone, Default)]
pub struct FileTouch {
    pub reads: usize,
    pub writes: usize,
    pub edits: usize,
    pub added: usize,
    pub removed: usize,
    pub last: Option<DateTime<Utc>>,
    /// index into `events` for each change, newest last
    pub changes: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// registry says busy and a tool call is outstanding
    Running(String),
    /// registry says busy, between tool calls
    Working,
    /// session is open, waiting on the user
    Waiting,
    /// no live process for this transcript
    Ended,
}

pub struct Session {
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub branch: String,
    pub title: String,
    pub first_prompt: String,
    pub model: String,
    pub version: String,
    pub mode: String,
    pub agent_name: String,
    pub started: Option<DateTime<Utc>>,
    pub last: Option<DateTime<Utc>>,
    pub events: VecDeque<Ev>,
    /// events trimmed off the front of the ring buffer
    pub dropped: usize,
    pub totals: Totals,
    pub tools: BTreeMap<String, usize>,
    pub turns: usize,
    pub live: Option<Live>,
    /// every file the session touched, keyed by path
    pub files: BTreeMap<String, FileTouch>,
    pub errors: usize,
    /// (tool, milliseconds) for every call that returned
    pub latencies: Vec<(String, i64)>,
    pub turn_ms: Vec<u64>,
    /// requests per model id
    pub models: BTreeMap<String, usize>,
    /// tool_use id -> (tool name, when it was issued)
    pending: HashMap<String, (String, Option<DateTime<Utc>>)>,
    tail: Tail,
}

impl Session {
    pub fn open(path: PathBuf) -> Self {
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        Session {
            id,
            tail: Tail::new(path.clone()),
            path,
            cwd: String::new(),
            branch: String::new(),
            title: String::new(),
            first_prompt: String::new(),
            model: String::new(),
            version: String::new(),
            mode: String::new(),
            agent_name: String::new(),
            started: None,
            last: None,
            events: VecDeque::new(),
            dropped: 0,
            totals: Totals::default(),
            tools: BTreeMap::new(),
            turns: 0,
            live: None,
            files: BTreeMap::new(),
            errors: 0,
            latencies: Vec::new(),
            turn_ms: Vec::new(),
            models: BTreeMap::new(),
            pending: HashMap::new(),
        }
    }

    /// Read whatever has been appended since the last call.
    pub fn pump(&mut self) -> usize {
        let lines = self.tail.poll().unwrap_or_default();
        let mut n = 0;
        for line in lines {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                self.apply(&v);
                n += 1;
            }
        }
        n
    }

    pub fn bytes_read(&self) -> u64 {
        self.tail.offset()
    }

    fn push(&mut self, ev: Ev) {
        if let Some(ts) = ev.ts {
            if self.started.is_none() {
                self.started = Some(ts);
            }
            self.last = Some(ts.max(self.last.unwrap_or(ts)));
        }
        self.events.push_back(ev);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
            self.dropped += 1;
        }
    }

    fn apply(&mut self, rec: &Value) {
        let ts = event::ts_of(rec);
        let kind = rec.get("type").and_then(Value::as_str).unwrap_or("");
        let take = |k: &str| rec.get(k).and_then(Value::as_str).unwrap_or("").to_string();

        match kind {
            "user" => {
                if rec.get("isMeta").and_then(Value::as_bool).unwrap_or(false) {
                    return;
                }
                if self.cwd.is_empty() {
                    self.cwd = take("cwd");
                }
                if self.branch.is_empty() {
                    self.branch = take("gitBranch");
                }
                let content = rec.get("message").and_then(|m| m.get("content"));
                match content {
                    Some(Value::String(text)) => self.push_prompt(ts, text),
                    Some(Value::Array(blocks)) => {
                        for b in blocks {
                            match b.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                                    self.push_prompt(ts, t);
                                }
                                Some("tool_result") => {
                                    let id = b
                                        .get("tool_use_id")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string();
                                    let is_error =
                                        b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                                    let (name, issued) = self
                                        .pending
                                        .remove(&id)
                                        .unwrap_or_else(|| ("tool".into(), None));
                                    if let (Some(a), Some(b)) = (issued, ts) {
                                        let ms = (b - a).num_milliseconds().max(0);
                                        self.latencies.push((name.clone(), ms));
                                    }
                                    let raw = b
                                        .get("content")
                                        .map(|c| match c {
                                            Value::String(s) => s.clone(),
                                            other => other.to_string(),
                                        })
                                        .unwrap_or_default();
                                    let tur = rec.get("toolUseResult");
                                    let (head, ok) = event::result_summary(tur, is_error, &raw);
                                    let body = event::result_body(tur, &raw);
                                    if !ok {
                                        self.errors += 1;
                                    }
                                    let mut ev = Ev::new(ts, Kind::Result, head, body);
                                    ev.tool = Some(name.clone());
                                    ev.ok = ok;
                                    self.push(ev);
                                    self.record_file(&name, tur, ts);
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let msg = rec.get("message");
                if let Some(m) = msg {
                    if let Some(model) = m.get("model").and_then(Value::as_str) {
                        if model != "<synthetic>" {
                            self.model = model.to_string();
                        }
                        if let Some(usage) = m.get("usage") {
                            self.add_usage(model, usage);
                        }
                    }
                    for b in m.get("content").and_then(Value::as_array).unwrap_or(&vec![]) {
                        match b.get("type").and_then(Value::as_str) {
                            Some("thinking") => {
                                let t = b.get("thinking").and_then(Value::as_str).unwrap_or("");
                                if !t.trim().is_empty() {
                                    self.push(Ev::new(
                                        ts,
                                        Kind::Thinking,
                                        event::clip(t, 400),
                                        t.to_string(),
                                    ));
                                }
                            }
                            Some("text") => {
                                let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                                if !t.trim().is_empty() {
                                    self.push(Ev::new(
                                        ts,
                                        Kind::Text,
                                        event::clip(t, 400),
                                        t.to_string(),
                                    ));
                                }
                            }
                            Some("tool_use") => {
                                let name = b
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("tool")
                                    .to_string();
                                let id =
                                    b.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                                let input = b.get("input").cloned().unwrap_or(Value::Null);
                                let summary = event::tool_summary(&name, &input);
                                let body = serde_json::to_string_pretty(&input)
                                    .unwrap_or_else(|_| input.to_string());
                                *self.tools.entry(name.clone()).or_insert(0) += 1;
                                self.pending.insert(id, (name.clone(), ts));
                                let mut ev = Ev::new(ts, Kind::Tool, summary, body);
                                ev.tool = Some(name);
                                self.push(ev);
                            }
                            _ => {}
                        }
                    }
                }
                if rec.get("isApiErrorMessage").and_then(Value::as_bool).unwrap_or(false) {
                    let mut ev = Ev::new(ts, Kind::System, "api error".into(), take("error"));
                    ev.ok = false;
                    self.push(ev);
                }
            }
            "system" => {
                let sub = rec.get("subtype").and_then(Value::as_str).unwrap_or("");
                match sub {
                    "turn_duration" => {
                        self.turns += 1;
                        let ms = rec.get("durationMs").and_then(Value::as_u64).unwrap_or(0);
                        self.turn_ms.push(ms);
                        let msgs = rec.get("messageCount").and_then(Value::as_u64).unwrap_or(0);
                        let head =
                            format!("turn done · {:.1}s · {} messages", ms as f64 / 1000.0, msgs);
                        self.push(Ev::new(ts, Kind::System, head.clone(), head));
                    }
                    "compact_boundary" => {
                        self.push(Ev::new(
                            ts,
                            Kind::System,
                            "context compacted".into(),
                            "context compacted".into(),
                        ));
                    }
                    "api_error" => {
                        let mut ev = Ev::new(
                            ts,
                            Kind::System,
                            format!("api error · {}", take("error")),
                            take("error"),
                        );
                        ev.ok = false;
                        self.push(ev);
                    }
                    "local_command" => {
                        let c = take("content");
                        self.push(Ev::new(ts, Kind::System, event::clip(&c, 300), c));
                    }
                    _ => {}
                }
            }
            "ai-title" => {
                if self.title.is_empty() {
                    self.title = take("aiTitle");
                }
            }
            "custom-title" => self.title = take("customTitle"),
            "agent-name" => self.agent_name = take("agentName"),
            "permission-mode" => self.mode = take("permissionMode"),
            _ => {}
        }

        if self.version.is_empty() {
            self.version = take("version");
        }
    }

    /// Fold a finished Read/Edit/Write into the per-file record. The event was
    /// just pushed, so its index is the last one in the ring buffer.
    fn record_file(&mut self, tool: &str, tur: Option<&Value>, ts: Option<DateTime<Utc>>) {
        let Some(v) = tur else { return };
        let path = v
            .get("filePath")
            .and_then(Value::as_str)
            .or_else(|| v.get("file").and_then(|f| f.get("filePath")).and_then(Value::as_str))
            .unwrap_or("");
        if path.is_empty() {
            return;
        }
        let idx = self.events.len().saturating_sub(1) + self.dropped;
        let patch = event::patch_text(v);
        let entry = self.files.entry(path.to_string()).or_default();
        entry.last = ts.or(entry.last);
        match tool {
            "Read" => entry.reads += 1,
            "Write" => entry.writes += 1,
            "Edit" | "NotebookEdit" => entry.edits += 1,
            _ => {}
        }
        if let Some((_, add, del)) = patch {
            entry.added += add;
            entry.removed += del;
            entry.changes.push(idx);
        }
    }

    /// Absolute event index (survives ring-buffer trimming) -> current slot.
    pub fn slot_of(&self, abs: usize) -> Option<usize> {
        abs.checked_sub(self.dropped)
    }

    pub fn lines_changed(&self) -> (usize, usize) {
        self.files.values().fold((0, 0), |(a, d), f| (a + f.added, d + f.removed))
    }

    /// (mean, worst) tool round-trip in milliseconds.
    pub fn latency(&self) -> (i64, i64) {
        if self.latencies.is_empty() {
            return (0, 0);
        }
        let sum: i64 = self.latencies.iter().map(|(_, ms)| ms).sum();
        let max = self.latencies.iter().map(|(_, ms)| *ms).max().unwrap_or(0);
        (sum / self.latencies.len() as i64, max)
    }

    /// Events per minute over the last `n` minutes, oldest first.
    pub fn activity(&self, n: usize) -> Vec<u64> {
        let now = Utc::now();
        let mut buckets = vec![0u64; n];
        for ev in &self.events {
            let Some(ts) = ev.ts else { continue };
            let mins_ago = (now - ts).num_minutes();
            if mins_ago < 0 || mins_ago >= n as i64 {
                continue;
            }
            let i = n - 1 - mins_ago as usize;
            buckets[i] += 1;
        }
        buckets
    }

    fn push_prompt(&mut self, ts: Option<DateTime<Utc>>, text: &str) {
        let t = text.trim();
        if t.is_empty() || t.starts_with("<system-reminder>") || t.starts_with("<command-") {
            return;
        }
        if self.first_prompt.is_empty() {
            self.first_prompt = event::clip(t, 120);
        }
        self.push(Ev::new(ts, Kind::Prompt, event::clip(t, 400), t.to_string()));
    }

    fn add_usage(&mut self, model: &str, usage: &Value) {
        let g = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        let input = g("input_tokens");
        let output = g("output_tokens");
        let cache_read = g("cache_read_input_tokens");
        let (w5, w1) = match usage.get("cache_creation") {
            Some(c) => (
                c.get("ephemeral_5m_input_tokens").and_then(Value::as_u64).unwrap_or(0),
                c.get("ephemeral_1h_input_tokens").and_then(Value::as_u64).unwrap_or(0),
            ),
            None => (g("cache_creation_input_tokens"), 0),
        };
        let fast = usage.get("speed").and_then(Value::as_str) == Some("fast");

        self.totals.input += input;
        self.totals.output += output;
        self.totals.cache_read += cache_read;
        self.totals.cache_write += w5 + w1;
        self.totals.ctx = input + cache_read + w5 + w1;
        self.totals.requests += 1;
        *self.models.entry(model.to_string()).or_insert(0) += 1;

        match pricing::rates(model, fast) {
            Some(r) => {
                let dollars = (input as f64 * r.input
                    + output as f64 * r.output
                    + cache_read as f64 * r.input * pricing::CACHE_READ
                    + w5 as f64 * r.input * pricing::CACHE_WRITE_5M
                    + w1 as f64 * r.input * pricing::CACHE_WRITE_1H)
                    / 1_000_000.0;
                self.totals.cost += dollars;
            }
            None => self.totals.unpriced += 1,
        }
    }

    pub fn status(&self) -> Status {
        match &self.live {
            Some(live) if live.status == "busy" => {
                match self.events.iter().rev().find(|e| {
                    matches!(e.kind, Kind::Tool | Kind::Result)
                }) {
                    Some(ev) if ev.kind == Kind::Tool => {
                        Status::Running(ev.tool.clone().unwrap_or_default())
                    }
                    _ => Status::Working,
                }
            }
            Some(_) => Status::Waiting,
            None => Status::Ended,
        }
    }

    /// Name for the session list: registry name, else title, else first prompt.
    pub fn label(&self) -> String {
        if !self.agent_name.is_empty() {
            return self.agent_name.clone();
        }
        if let Some(live) = &self.live {
            if !live.name.is_empty() {
                return live.name.clone();
            }
        }
        if !self.title.is_empty() {
            return self.title.clone();
        }
        if !self.first_prompt.is_empty() {
            return self.first_prompt.clone();
        }
        self.id.chars().take(8).collect()
    }

    pub fn where_(&self) -> String {
        let cwd = if self.cwd.is_empty() {
            self.live.as_ref().map(|l| l.cwd.clone()).unwrap_or_default()
        } else {
            self.cwd.clone()
        };
        let short = event::short_path(&cwd);
        if self.branch.is_empty() {
            short
        } else {
            format!("{short} · {}", self.branch)
        }
    }

    /// Context window for the model in play, for the "how full" bar.
    pub fn window(&self) -> u64 {
        if self.model.contains("haiku") { 200_000 } else { 1_000_000 }
    }

    pub fn age_secs(&self) -> i64 {
        self.last.map_or(i64::MAX, |t| (Utc::now() - t).num_seconds().max(0))
    }
}
