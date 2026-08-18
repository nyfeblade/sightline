//! Steering sessions through tmux.
//!
//! Claude Code's own cross-session channel is a token-authenticated private
//! socket, so scope drives the terminal instead: a session running in a tmux
//! pane can be typed into exactly as a person would type into it. That keeps
//! permission prompts, slash commands and every other interactive affordance
//! working, and it does not depend on Claude Code internals.
//!
//! Sessions started outside tmux are observable but not steerable. Nothing here
//! fails loudly when tmux is missing — the control keys simply say so.

use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: String,
    pub pid: i64,
    pub session: String,
}

fn tmux(args: &[&str]) -> Option<String> {
    let out = Command::new("tmux").args(args).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Every pane tmux currently knows about, with the pid of the process it runs.
pub fn panes() -> Vec<Pane> {
    let Some(out) = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{pane_pid}\t#{session_name}",
    ]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let id = f.next()?.to_string();
            let pid = f.next()?.parse().ok()?;
            let session = f.next()?.to_string();
            Some(Pane { id, pid, session })
        })
        .collect()
}

/// Parent pid, on Linux from procfs and elsewhere from ps.
fn parent_of(pid: i64) -> Option<i64> {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let close = stat.rfind(')')?;
        return stat[close + 1..].split_whitespace().nth(1)?.parse().ok();
    }
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The pane a session's process is running inside, if any. A claude process
/// sits a shell or two below the pane, so walk up the process tree.
pub fn pane_for(pid: i64, panes: &[Pane]) -> Option<Pane> {
    let mut cur = pid;
    for _ in 0..8 {
        if let Some(p) = panes.iter().find(|p| p.pid == cur) {
            return Some(p.clone());
        }
        match parent_of(cur) {
            Some(1) | Some(0) | None => return None,
            Some(next) => cur = next,
        }
    }
    None
}

/// Type a line into a session and submit it, the same as a person would.
pub fn send_text(pane: &str, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("nothing to send".into());
    }
    // -l sends the text literally, so it is never read as a key name.
    tmux(&["send-keys", "-t", pane, "-l", "--", text]).ok_or("tmux send-keys failed")?;
    tmux(&["send-keys", "-t", pane, "Enter"]).ok_or("tmux send-keys failed")?;
    Ok(())
}

/// Send a named key — Escape to interrupt a turn, Enter to accept a prompt.
pub fn send_key(pane: &str, key: &str) -> Result<(), String> {
    tmux(&["send-keys", "-t", pane, key]).ok_or_else(|| "tmux send-keys failed".into()).map(|_| ())
}

/// Start a fresh Claude Code session in its own tmux session.
pub fn new_session(cwd: &Path, prompt: Option<&str>) -> Result<String, String> {
    if !available() {
        return Err("tmux is not installed".into());
    }
    let existing = tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap_or_default();
    let name = (1..99)
        .map(|n| format!("scope-{n}"))
        .find(|n| !existing.lines().any(|l| l == n))
        .ok_or("no free session name")?;
    let cwd = cwd.to_string_lossy().to_string();
    tmux(&["new-session", "-d", "-s", &name, "-c", &cwd, "--", "claude"])
        .ok_or("tmux could not start the session (is claude on PATH?)")?;
    if let Some(p) = prompt {
        // Give Claude Code a moment to draw its prompt before typing into it.
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let pane = format!("{name}:0.0");
        send_text(&pane, p)?;
    }
    Ok(name)
}

/// Hand the terminal over to tmux until the user detaches.
pub fn attach(session: &str) -> Result<(), String> {
    let status = Command::new("tmux")
        .args(["attach", "-t", session])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err("tmux attach failed".into()) }
}
