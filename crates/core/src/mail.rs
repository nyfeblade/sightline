//! Messages for a session that is not holding a pipe.
//!
//! Claude Code in owned mode listens on stdin across turns, so another message
//! is a write. Cursor does not exist between turns: the chat is what survives,
//! and `--resume` reopens it. Grok Bot is the Cursor desktop assistant — there
//! is no process to write to and no `--resume` that starts one. What survives a
//! turn is a file, and the next turn reads it.
//!
//! The file lives in Sightline's data directory, not in the worktree, because a
//! mailbox an assigned worker can edit is not a mailbox. `tell` appends; `inbox`
//! takes. A process that is not the one holding the fleet can still deliver and
//! collect, which is the whole of this existing: the MCP door is a different
//! process, and that is how a later turn actually sees the message.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a connected session is working, and who it is.
///
/// Written when the session is connected rather than spawned, so a later
/// process — `sightline mcp --as SESSION` is the one that matters — can still
/// find the directory and the agent without asking a fleet it is not in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connected {
    pub name: String,
    pub cwd: String,
    pub agent: String,
    #[serde(default)]
    pub model: String,
    pub started: i64,
}

fn root() -> PathBuf {
    crate::app::data_dir().join("connected")
}

fn meta_path(name: &str) -> PathBuf {
    root().join(format!("{name}.json"))
}

fn mail_path(name: &str) -> PathBuf {
    root().join(format!("{name}.mail"))
}

/// Remember a connected session. Overwrites: a name is only connected once.
pub fn remember(it: &Connected) -> Result<(), String> {
    let dir = root();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = meta_path(&it.name);
    let text = serde_json::to_string_pretty(it).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Forget one, mail and all. Missing is success: stop should not fail because
/// the files were already gone.
pub fn forget(name: &str) {
    let _ = std::fs::remove_file(meta_path(name));
    let _ = std::fs::remove_file(mail_path(name));
}

pub fn get(name: &str) -> Option<Connected> {
    let text = std::fs::read_to_string(meta_path(name)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn cwd(name: &str) -> Option<PathBuf> {
    get(name).map(|c| PathBuf::from(c.cwd))
}

pub fn exists(name: &str) -> bool {
    meta_path(name).is_file()
}

/// Every connected session still on disk.
pub fn list() -> Vec<Connected> {
    let Ok(entries) = std::fs::read_dir(root()) else {
        return Vec::new();
    };
    let mut all: Vec<Connected> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str(&text).ok())
        .collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all
}

/// Leave a message. Creates the mailbox if this is the first.
pub fn push(name: &str, text: &str) -> Result<(), String> {
    if !exists(name) {
        return Err(format!("no connected session called {name}"));
    }
    let path = mail_path(name);
    let mut body = std::fs::read_to_string(&path).unwrap_or_default();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    // One message per record, JSON, so a message that contains a newline is
    // still one message when it is read back.
    let line = serde_json::to_string(text).map_err(|e| e.to_string())?;
    body.push_str(&line);
    body.push('\n');
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))
}

/// Take everything waiting, and empty the mailbox.
///
/// Destructive on purpose, the way draining events is: handing the same
/// assignment to two turns is how work gets done twice. An empty mailbox is
/// `nothing waiting`, not an error — a worker that checks and finds nothing
/// has learned something, and should not be told the tool failed.
pub fn take(name: &str) -> Result<Vec<String>, String> {
    let path = mail_path(name);
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let _ = std::fs::remove_file(&path);
    let mut out = Vec::new();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<String>(line) {
            Ok(text) => out.push(text),
            // A mailbox written by an older shape, or by a person: the line
            // itself is the message rather than a refusal to read any of it.
            Err(_) => out.push(line.to_string()),
        }
    }
    Ok(out)
}

/// Peek without taking. Tests, and anyone who wants to know whether tell
/// landed before the worker has picked it up.
pub fn waiting(name: &str) -> Vec<String> {
    let Ok(body) = std::fs::read_to_string(mail_path(name)) else {
        return Vec::new();
    };
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| serde_json::from_str::<String>(line).unwrap_or_else(|_| line.to_string()))
        .collect()
}

/// The directory a test can point at, so a suite that talks to the mailbox
/// does not write into the machine's real state. Not used in production.
#[cfg(test)]
pub fn root_is(path: &std::path::Path) -> PathBuf {
    path.join("connected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sightline-mail-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Production helpers talk to `data_dir()`. These tests talk to a scratch
    /// copy of the same shape, so they prove the format rather than the path.
    fn remember_in(dir: &Path, it: &Connected) {
        let root = root_is(dir);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(format!("{}.json", it.name)),
            serde_json::to_string_pretty(it).unwrap(),
        )
        .unwrap();
    }

    fn push_in(dir: &Path, name: &str, text: &str) {
        let path = root_is(dir).join(format!("{name}.mail"));
        let mut body = std::fs::read_to_string(&path).unwrap_or_default();
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&serde_json::to_string(text).unwrap());
        body.push('\n');
        std::fs::write(path, body).unwrap();
    }

    fn take_in(dir: &Path, name: &str) -> Vec<String> {
        let path = root_is(dir).join(format!("{name}.mail"));
        let Ok(body) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let _ = std::fs::remove_file(&path);
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| serde_json::from_str::<String>(line).unwrap_or_else(|_| line.to_string()))
            .collect()
    }

    #[test]
    fn a_message_is_still_there_after_the_process_that_wrote_it_has_gone() {
        // The defect this exists for: Cursor's first tell wrote down a pipe
        // the agent was no longer reading. A session that is not a long-lived
        // pipe has to leave the message somewhere a later turn can find it.
        let dir = scratch("survive");
        remember_in(
            &dir,
            &Connected {
                name: "owned-3".into(),
                cwd: "/tmp/work".into(),
                agent: "grok".into(),
                model: String::new(),
                started: 1,
            },
        );
        push_in(&dir, "owned-3", "the assignment");
        push_in(&dir, "owned-3", "and a correction");
        let got = take_in(&dir, "owned-3");
        assert_eq!(got, vec!["the assignment", "and a correction"]);
        assert!(
            take_in(&dir, "owned-3").is_empty(),
            "taking is how a later turn does not see the same work twice"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_message_that_contains_a_newline_is_still_one_message() {
        let dir = scratch("newline");
        remember_in(
            &dir,
            &Connected {
                name: "owned-1".into(),
                cwd: "/tmp".into(),
                agent: "grok".into(),
                model: String::new(),
                started: 1,
            },
        );
        push_in(&dir, "owned-1", "first line\nsecond line");
        let got = take_in(&dir, "owned-1");
        assert_eq!(got, vec!["first line\nsecond line"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Every session recorded as connected.
///
/// On disk rather than in a fleet map, because a connected session is not held
/// by any process: the whole point is that the agent is somewhere else. That
/// makes this the only record of the name, and the only way another process can
/// avoid handing the same one out twice.
pub fn connected_names() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "json")
                .then(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .flatten()
        })
        .collect()
}
