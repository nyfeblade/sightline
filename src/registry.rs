//! Claude Code keeps a live-session registry at ~/.claude/sessions/<pid>.json.
//! It is the authoritative answer to "is this session still open, and is it
//! working right now" — the transcript alone can only guess from timestamps.

use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Live {
    pub pid: i64,
    pub session_id: String,
    pub cwd: String,
    pub name: String,
    pub status: String,
    pub kind: String,
    pub version: String,
    pub status_updated_ms: i64,
}

/// Field 22 of /proc/<pid>/stat is the process start time. Comparing it to the
/// registry's `procStart` rules out a recycled pid pointing at some unrelated
/// process.
fn proc_start(pid: i64) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail = &stat[stat.rfind(')')? + 1..];
    tail.split_whitespace().nth(19).map(str::to_string)
}

fn alive(pid: i64, want: &str) -> bool {
    match proc_start(pid) {
        Some(got) => got == want,
        // No procfs (not Linux): fall back to existence of the pid directory.
        None => Path::new(&format!("/proc/{pid}")).exists(),
    }
}

pub fn scan(dir: &Path) -> HashMap<String, Live> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let pid = v.get("pid").and_then(Value::as_i64).unwrap_or(0);
        let want = v.get("procStart").and_then(Value::as_str).unwrap_or("");
        if pid == 0 || !alive(pid, want) {
            continue;
        }
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let session_id = s("sessionId");
        if session_id.is_empty() {
            continue;
        }
        out.insert(
            session_id.clone(),
            Live {
                pid,
                session_id,
                cwd: s("cwd"),
                name: s("name"),
                status: s("status"),
                kind: s("kind"),
                version: s("version"),
                status_updated_ms: v.get("statusUpdatedAt").and_then(Value::as_i64).unwrap_or(0),
            },
        );
    }
    out
}
