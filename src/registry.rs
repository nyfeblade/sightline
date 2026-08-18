//! Claude Code keeps a live-session registry at ~/.claude/sessions/<pid>.json.
//! It is the authoritative answer to "is this session still open, and is it
//! working right now" — the transcript alone can only guess from timestamps.
//!
//! Older Claude Code versions do not write it. When it is absent the caller
//! falls back to inferring liveness from transcript recency.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

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

/// Decides whether a pid from the registry is still the process that wrote it.
///
/// On Linux that is a cheap procfs read per pid. Elsewhere there is no procfs,
/// so one `ps` call per scan collects the live claude pids instead.
enum Prober {
    Procfs,
    Snapshot(HashSet<i64>),
}

impl Prober {
    fn new() -> Self {
        if Path::new("/proc/self/stat").exists() {
            return Prober::Procfs;
        }
        let mut pids = HashSet::new();
        if let Ok(out) = Command::new("ps").args(["-eo", "pid=,comm="]).output() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let mut parts = line.split_whitespace();
                if let (Some(pid), Some(comm)) = (parts.next(), parts.next()) {
                    if comm.contains("claude") {
                        if let Ok(p) = pid.parse::<i64>() {
                            pids.insert(p);
                        }
                    }
                }
            }
        }
        Prober::Snapshot(pids)
    }

    /// Field 22 of /proc/<pid>/stat is the process start time. Comparing it to
    /// the registry's `procStart` rules out a recycled pid pointing at some
    /// unrelated process.
    fn alive(&self, pid: i64, want: &str) -> bool {
        match self {
            Prober::Procfs => {
                let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                    return false;
                };
                let Some(close) = stat.rfind(')') else { return false };
                match stat[close + 1..].split_whitespace().nth(19) {
                    // No procStart recorded (older client): existence is all we have.
                    Some(got) => want.is_empty() || got == want,
                    None => false,
                }
            }
            Prober::Snapshot(pids) => pids.contains(&pid),
        }
    }
}

/// Live sessions keyed by session id. Empty when the registry is absent.
pub fn scan(dir: &Path) -> HashMap<String, Live> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    let prober = Prober::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let pid = v.get("pid").and_then(Value::as_i64).unwrap_or(0);
        let want = v.get("procStart").and_then(Value::as_str).unwrap_or("");
        if pid == 0 || !prober.alive(pid, want) {
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

/// Whether this Claude Code install maintains the registry at all.
pub fn available(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut e| e.any(|f| {
            f.map(|f| f.path().extension().and_then(|x| x.to_str()) == Some("json"))
                .unwrap_or(false)
        }))
        .unwrap_or(false)
}
