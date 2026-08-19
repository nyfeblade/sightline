//! One transcript record in, one or more feed events out.

use chrono::{DateTime, Utc};
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Prompt,
    Thinking,
    Text,
    Tool,
    Result,
    System,
}

#[derive(Clone, Debug)]
pub struct Ev {
    pub ts: Option<DateTime<Utc>>,
    pub kind: Kind,
    /// tool name, for Tool and Result events
    pub tool: Option<String>,
    /// single-line summary shown in the feed
    pub head: String,
    /// full text, shown in the detail popup
    pub body: String,
    pub ok: bool,
    /// a file holding the untruncated output, when the harness spilled it
    pub spill: Option<String>,
}

impl Ev {
    pub fn new(ts: Option<DateTime<Utc>>, kind: Kind, head: String, body: String) -> Self {
        Ev {
            ts,
            kind,
            tool: None,
            head,
            body,
            ok: true,
            spill: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Tools,
    Bash,
    Files,
    Talk,
}

impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Tools => "tools",
            Filter::Bash => "bash",
            Filter::Files => "files",
            Filter::Talk => "talk",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Filter::All => Filter::Tools,
            Filter::Tools => Filter::Bash,
            Filter::Bash => Filter::Files,
            Filter::Files => Filter::Talk,
            Filter::Talk => Filter::All,
        }
    }

    pub fn keeps(self, ev: &Ev) -> bool {
        let tool = ev.tool.as_deref().unwrap_or("");
        match self {
            Filter::All => true,
            Filter::Tools => matches!(ev.kind, Kind::Tool | Kind::Result),
            Filter::Bash => matches!(ev.kind, Kind::Tool | Kind::Result) && tool == "Bash",
            Filter::Files => {
                matches!(ev.kind, Kind::Tool | Kind::Result)
                    && matches!(tool, "Edit" | "Write" | "Read" | "NotebookEdit")
            }
            Filter::Talk => matches!(ev.kind, Kind::Prompt | Kind::Text),
        }
    }
}

/// Large tool output is written to a file and replaced with a pointer. Find it
/// so the detail view can show what was actually produced, not the preview.
pub fn spill_path(text: &str) -> Option<String> {
    let at = text.find("saved to: ")? + "saved to: ".len();
    let rest = &text[at..];
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let path = rest[..end].trim();
    if path.starts_with('/') {
        Some(path.to_string())
    } else {
        None
    }
}

pub fn ts_of(rec: &Value) -> Option<DateTime<Utc>> {
    let s = rec.get("timestamp")?.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Collapse whitespace and cut to `n` display characters.
pub fn clip(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        let mut out: String = flat.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub fn short_path(p: &str) -> String {
    // USERPROFILE as well as HOME: Windows sets only the former.
    let home = crate::app::home().to_string_lossy().to_string();
    if home.len() > 1 && p.starts_with(&home) {
        return format!("~{}", &p[home.len()..]);
    }
    p.to_string()
}

fn bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// What a tool call is about to do, in one line.
pub fn tool_summary(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    let flag = |k: &str| input.get(k).and_then(Value::as_bool).unwrap_or(false);
    match name {
        "Bash" => {
            let bg = if flag("run_in_background") {
                "[bg] "
            } else {
                ""
            };
            format!("{bg}{}", s("command"))
        }
        "Read" => {
            let mut out = short_path(s("file_path"));
            if let Some(off) = input.get("offset").and_then(Value::as_u64) {
                out.push_str(&format!(" @{off}"));
            }
            out
        }
        "Edit" => {
            let all = if flag("replace_all") { " (all)" } else { "" };
            format!("{}{all}", short_path(s("file_path")))
        }
        "Write" => {
            let n = input
                .get("content")
                .and_then(Value::as_str)
                .map_or(0, str::len);
            format!("{} ({})", short_path(s("file_path")), bytes(n))
        }
        "Agent" => {
            let kind = input
                .get("subagent_type")
                .and_then(Value::as_str)
                .unwrap_or("general-purpose");
            format!("{kind} · {}", s("description"))
        }
        "Skill" => format!("{} {}", s("skill"), s("args"))
            .trim_end()
            .to_string(),
        "WebSearch" => s("query").to_string(),
        "WebFetch" => s("url").to_string(),
        "Grep" => format!("{} {}", s("pattern"), s("path"))
            .trim_end()
            .to_string(),
        "Glob" => s("pattern").to_string(),
        "TodoWrite" => {
            let n = input
                .get("todos")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            format!("{n} items")
        }
        "Task" | "TaskCreate" | "TaskUpdate" => format!("{} {}", s("description"), s("prompt"))
            .trim()
            .to_string(),
        "ToolSearch" => s("query").to_string(),
        "Workflow" => s("name").to_string(),
        "Artifact" => format!("{} {}", s("action"), s("file_path"))
            .trim()
            .to_string(),
        "SendMessage" => format!("→ {} {}", s("to"), s("message")),
        _ => input.to_string(),
    }
}

/// What came back. Reads `toolUseResult`, which carries more than the
/// `tool_result` block does (exit output, patches, agent status).
pub fn result_summary(tur: Option<&Value>, is_error: bool, fallback: &str) -> (String, bool) {
    let Some(v) = tur else {
        return (clip(fallback, 400), !is_error);
    };
    if let Some(text) = v.as_str() {
        return (
            format!("{} · {}", bytes(text.len()), clip(text, 300)),
            !is_error,
        );
    }
    let obj = match v.as_object() {
        Some(o) => o,
        None => return (clip(&v.to_string(), 300), !is_error),
    };
    let get_str = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or("");
    let interrupted = obj
        .get("interrupted")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Bash
    if obj.contains_key("stdout") || obj.contains_key("stderr") {
        let out = get_str("stdout");
        let err = get_str("stderr");
        let mut head = String::new();
        if let Some(id) = obj.get("backgroundTaskId").and_then(Value::as_str) {
            head.push_str(&format!("background {id} · "));
        }
        head.push_str(&bytes(out.len()));
        if !err.is_empty() {
            head.push_str(&format!(" · stderr {}", bytes(err.len())));
        }
        if interrupted {
            head.push_str(" · interrupted");
        }
        let first = if out.is_empty() { err } else { out };
        if !first.is_empty() {
            head.push_str(&format!(" · {}", clip(first, 200)));
        }
        let ok = !is_error && !interrupted && err.is_empty();
        return (head, ok);
    }
    // Edit / Write
    if let Some(patch) = obj.get("structuredPatch").and_then(Value::as_array) {
        let (mut add, mut del) = (0usize, 0usize);
        for hunk in patch {
            for line in hunk
                .get("lines")
                .and_then(Value::as_array)
                .unwrap_or(&vec![])
            {
                match line.as_str().unwrap_or("").chars().next() {
                    Some('+') => add += 1,
                    Some('-') => del += 1,
                    _ => {}
                }
            }
        }
        let path = short_path(get_str("filePath"));
        return (format!("{path} · +{add}/-{del}"), !is_error);
    }
    // Read
    if let Some(file) = obj.get("file") {
        let path = short_path(file.get("filePath").and_then(Value::as_str).unwrap_or(""));
        let kind = file.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "image" || file.get("numLines").is_none() {
            let src = obj.get("type").and_then(Value::as_str).unwrap_or(kind);
            let path = if path.is_empty() {
                short_path(get_str("filePath"))
            } else {
                path
            };
            return (
                format!("{path} · {}", if src.is_empty() { "binary" } else { src }),
                !is_error,
            );
        }
        let lines = file.get("numLines").and_then(Value::as_u64).unwrap_or(0);
        let total = file
            .get("totalLines")
            .and_then(Value::as_u64)
            .unwrap_or(lines);
        return (format!("{path} · {lines}/{total} lines"), !is_error);
    }
    // Agent
    if let Some(id) = obj.get("agentId").and_then(Value::as_str) {
        let status = get_str("status");
        let model = get_str("resolvedModel");
        return (format!("agent {id} · {status} · {model}"), !is_error);
    }
    // WebSearch
    if let Some(n) = obj.get("searchCount").and_then(Value::as_u64) {
        let secs = obj
            .get("durationSeconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        return (
            format!("{n} searches · {secs:.1}s · {}", get_str("query")),
            !is_error,
        );
    }
    if obj.contains_key("newTodos") {
        let n = obj
            .get("newTodos")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        return (format!("{n} todos"), !is_error);
    }
    (clip(&v.to_string(), 300), !is_error)
}

/// Rebuild a unified diff from a tool result's structuredPatch.
/// Returns (text, added, removed).
pub fn patch_text(tur: &Value) -> Option<(String, usize, usize)> {
    let hunks = tur.get("structuredPatch")?.as_array()?;
    let (mut add, mut del) = (0usize, 0usize);
    let mut out = String::new();
    for h in hunks {
        let n = |k: &str| h.get(k).and_then(Value::as_u64).unwrap_or(0);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            n("oldStart"),
            n("oldLines"),
            n("newStart"),
            n("newLines")
        ));
        for line in h.get("lines").and_then(Value::as_array).unwrap_or(&vec![]) {
            let l = line.as_str().unwrap_or("");
            match l.chars().next() {
                Some('+') => add += 1,
                Some('-') => del += 1,
                _ => {}
            }
            out.push_str(l);
            out.push('\n');
        }
    }
    if out.is_empty() {
        None
    } else {
        Some((out, add, del))
    }
}

/// Full text for the detail popup.
pub fn result_body(tur: Option<&Value>, fallback: &str) -> String {
    let Some(v) = tur else {
        return fallback.to_string();
    };
    if let Some(text) = v.as_str() {
        return text.to_string();
    }
    if let Some((text, _, _)) = patch_text(v) {
        return text;
    }
    if let Some(obj) = v.as_object() {
        if obj.contains_key("stdout") || obj.contains_key("stderr") {
            let out = obj.get("stdout").and_then(Value::as_str).unwrap_or("");
            let err = obj.get("stderr").and_then(Value::as_str).unwrap_or("");
            let mut s = out.to_string();
            if !err.is_empty() {
                s.push_str("\n--- stderr ---\n");
                s.push_str(err);
            }
            return s;
        }
        if let Some(file) = obj
            .get("file")
            .and_then(|f| f.get("content"))
            .and_then(Value::as_str)
        {
            return file.to_string();
        }
    }
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}
