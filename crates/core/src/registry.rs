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
    pub cwd: String,
    pub name: String,
    pub status: String,
    pub kind: String,
    pub version: String,
}

impl Live {
    /// The same answer for a session Ironsight holds itself.
    ///
    /// A session driven over pipes never writes a registry entry — there is no
    /// terminal for it to register from — so Ironsight is the only thing that
    /// knows it is running. Saying so in the registry's own shape means every
    /// judgement downstream (is it working, is it waiting, has it ended) is
    /// made by the same code for both kinds.
    pub fn owned(o: &crate::owned::Owned) -> Live {
        Live {
            pid: o.pid as i64,
            cwd: o.cwd.clone(),
            // Deliberately empty. The registry's name wins over the title a
            // conversation gave itself, and `owned-3` is a handle rather than a
            // name worth showing in its place.
            name: String::new(),
            status: if o.busy { "busy" } else { "idle" }.to_string(),
            kind: "owned".to_string(),
            version: String::new(),
        }
    }

    /// The same record, but working. Used the instant a message is sent, so a
    /// session that has just been asked something never looks idle enough to be
    /// asked a second thing before the first has been noticed.
    pub fn into_busy(mut self) -> Live {
        self.status = "busy".to_string();
        self
    }
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
        if cfg!(windows) {
            // Windows has no command line in tasklist, only the image name, so
            // the test is looser: claude.exe for a native install, node.exe for
            // an npm one. It only ever judges pids the registry already claims
            // are Claude Code, so the looseness costs nothing.
            let out = Command::new("tasklist")
                .args(["/FO", "CSV", "/NH"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            return Prober::Snapshot(claude_pids_in_tasklist(&out));
        }
        // -ww stops macOS ps cutting the command line short of the part that
        // identifies Claude Code.
        let out = Command::new("ps")
            .args(["-ww", "-eo", "pid=,args="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        Prober::Snapshot(claude_pids_in(&out))
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
                let Some(close) = stat.rfind(')') else {
                    return false;
                };
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

/// Pids of Claude Code processes in `ps -eo pid=,args=` output.
///
/// The whole command line decides, not the executable name: an npm install
/// runs as `node`, and matching `comm` alone reported every session on such a
/// machine as ended.
fn claude_pids_in(out: &str) -> HashSet<i64> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(2, char::is_whitespace);
            let pid = parts.next()?.parse::<i64>().ok()?;
            crate::control::is_claude(parts.next()?).then_some(pid)
        })
        .collect()
}

/// Pids of Claude Code processes in `tasklist /FO CSV /NH` output, whose rows
/// look like `"claude.exe","1234","Console","1","98,765 K"`.
fn claude_pids_in_tasklist(out: &str) -> HashSet<i64> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split("\",\"").map(|f| f.trim_matches('"'));
            let image = fields.next()?.to_lowercase();
            let pid = fields.next()?.trim().parse::<i64>().ok()?;
            (image.starts_with("claude") || image.starts_with("node")).then_some(pid)
        })
        .collect()
}

/// Live sessions keyed by session id. Empty when the registry is absent.
pub fn scan(dir: &Path) -> HashMap<String, Live> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let prober = Prober::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
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
                cwd: s("cwd"),
                name: s("name"),
                status: s("status"),
                kind: s("kind"),
                version: s("version"),
            },
        );
    }
    out
}

/// Whether this Claude Code install maintains the registry at all.
pub fn available(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut e| {
            e.any(|f| {
                f.map(|f| f.path().extension().and_then(|x| x.to_str()) == Some("json"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // What `ps -ww -eo pid=,args=` looks like on a machine with no procfs.
    const PS: &str = "\
 4821 node /Users/x/.npm-global/lib/node_modules/@anthropic-ai/claude-code/cli.js
 4822 -zsh
 4823 /Users/x/.local/bin/claude --resume 8f2c
 4824 /Applications/Firefox.app/Contents/MacOS/firefox
";

    // What `tasklist /FO CSV /NH` prints on Windows.
    const TASKLIST: &str = "\
\"claude.exe\",\"7312\",\"Console\",\"1\",\"184,204 K\"
\"node.exe\",\"9004\",\"Console\",\"1\",\"96,120 K\"
\"explorer.exe\",\"4188\",\"Console\",\"1\",\"140,996 K\"
";

    #[test]
    fn reads_windows_process_names() {
        let pids = claude_pids_in_tasklist(TASKLIST);
        assert!(pids.contains(&7312), "native install");
        assert!(pids.contains(&9004), "npm install runs as node.exe");
        assert_eq!(pids.len(), 2, "nothing else should be counted");
    }

    #[test]
    fn finds_claude_however_it_was_installed() {
        let pids = claude_pids_in(PS);
        assert!(pids.contains(&4821), "npm install runs as node");
        assert!(pids.contains(&4823), "native install");
        assert_eq!(pids.len(), 2, "nothing else should be counted");
    }
}
