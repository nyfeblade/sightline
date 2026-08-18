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
    /// the command the pane was started with, e.g. "claude --resume <id>"
    pub cmd: String,
    pub cwd: String,
}

/// The next free scope-N. Counting up from the highest existing name rather
/// than searching a fixed range means the pool can never be "full" — an early
/// version scanned scope-1..scope-98 and refused to start anything once those
/// were taken.
fn next_name_after(existing: &str) -> String {
    let highest = existing
        .lines()
        .filter_map(|l| l.trim().strip_prefix("scope-"))
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("scope-{}", highest + 1)
}

fn next_name() -> String {
    next_name_after(&tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap_or_default())
}

fn tmux(args: &[&str]) -> Option<String> {
    let out = Command::new("tmux")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
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
        "#{pane_id}\t#{pane_pid}\t#{session_name}\t#{pane_start_command}\t#{pane_current_path}",
    ]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let id = f.next()?.to_string();
            let pid = f.next()?.parse().ok()?;
            let session = f.next()?.to_string();
            let cmd = f.next().unwrap_or("").to_string();
            let cwd = f.next().unwrap_or("").to_string();
            Some(Pane {
                id,
                pid,
                session,
                cmd,
                cwd,
            })
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
    tmux(&["send-keys", "-t", pane, key])
        .ok_or_else(|| "tmux send-keys failed".into())
        .map(|_| ())
}

/// Start a fresh Claude Code session in its own tmux session.
pub fn new_session(cwd: &Path, prompt: Option<&str>) -> Result<String, String> {
    if !available() {
        return Err("tmux is not installed".into());
    }
    let name = next_name();
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

/// True when scope is itself running inside tmux. Attaching from there is
/// refused by tmux — the client has to be switched instead.
pub fn inside_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Put the way back on screen. A session drawn full-screen gives no clue how
/// to leave it, so the hint lives in the target's own status line.
fn show_way_back(session: &str, hint: &str) {
    tmux(&["set-option", "-t", session, "status", "on"]);
    tmux(&["set-option", "-t", session, "status-right", hint]);
}

/// Show a session full-screen. Returns true when scope's own terminal was
/// handed over and must be taken back afterwards; false when the tmux client
/// was switched instead, which leaves scope running where it is.
pub fn attach(session: &str) -> Result<bool, String> {
    if inside_tmux() {
        show_way_back(session, " ctrl+b L → back to scope ");
        tmux(&["switch-client", "-t", session]).ok_or("tmux switch-client failed")?;
        return Ok(false);
    }
    show_way_back(session, " ctrl+b d → back to scope ");
    let status = Command::new("tmux")
        .args(["attach", "-t", session])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(true)
    } else {
        Err("tmux attach failed".into())
    }
}

/// What a session's pane currently shows. Used both for the live mirror and
/// for spotting a session that is blocked on a question.
pub fn capture(pane: &str) -> Option<String> {
    tmux(&["capture-pane", "-p", "-t", pane])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub question: String,
    pub options: Vec<String>,
}

fn strip_box(line: &str) -> String {
    line.trim_matches(|c: char| {
        c.is_whitespace() || ('\u{2500}'..='\u{257F}').contains(&c) || c == '│' || c == '❯'
    })
    .trim()
    .to_string()
}

fn is_option(line: &str) -> bool {
    let t = strip_box(line);
    let mut chars = t.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_digit()) && t.contains(". ")
}

/// A permission prompt, trust prompt, or any other numbered choice waiting for
/// an answer. Read off the rendered pane, so it works for every prompt shape
/// Claude Code draws without knowing anything about its internals.
pub fn pending_approval(text: &str) -> Option<Approval> {
    let lines: Vec<&str> = text.lines().collect();
    // The cursor marker is what distinguishes a live choice from transcript
    // text that merely happens to contain a numbered list.
    let cursor = lines
        .iter()
        .rposition(|l| l.contains('❯') && is_option(l))?;
    let mut start = cursor;
    while start > 0 && is_option(lines[start - 1]) {
        start -= 1;
    }
    let mut options = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let text = strip_box(lines[i]);
        let low = text.to_lowercase();
        if low.starts_with("esc to") || low.starts_with("enter to confirm") {
            break;
        }
        if is_option(lines[i]) {
            options.push(text);
        } else if text.is_empty() {
            // blank line inside the block: keep looking
        } else if !options.is_empty() && lines[i].starts_with(' ') {
            // an option whose text wrapped onto the next line
            if let Some(last) = options.last_mut() {
                last.push(' ');
                last.push_str(&text);
            }
        } else {
            break;
        }
        i += 1;
    }
    if options.is_empty() {
        return None;
    }
    // The question is usually the nearest line above the options that asks
    // something; decorative lines ("Security guide", key hints) sit between.
    let above: Vec<String> = (start.saturating_sub(8)..start)
        .rev()
        .map(|j| strip_box(lines[j]))
        .filter(|l| l.chars().any(char::is_alphabetic))
        .collect();
    let boilerplate = |l: &String| {
        let low = l.to_lowercase();
        low.starts_with("enter to confirm")
            || low.starts_with("esc to")
            || low == "security guide"
            || low.starts_with("press ")
    };
    let question = above
        .iter()
        .find(|l| l.contains('?'))
        .or_else(|| above.iter().find(|l| !boilerplate(l)))
        .cloned()
        .unwrap_or_else(|| "waiting for an answer".into());
    Some(Approval { question, options })
}

/// Answer a numbered prompt by choosing option `n`.
pub fn answer(pane: &str, n: usize) -> Result<(), String> {
    send_text(pane, &n.to_string())
}

/// Translate a key press into the name tmux expects, so a whole keyboard can
/// be forwarded to a session.
pub fn tmux_key(code: crossterm::event::KeyCode, ctrl: bool) -> Option<String> {
    use crossterm::event::KeyCode as K;
    Some(match code {
        K::Char(c) if ctrl => format!("C-{c}"),
        K::Char(c) => return Some(format!("\u{0}{c}")), // literal, marked for send-keys -l
        K::Enter => "Enter".into(),
        K::Esc => "Escape".into(),
        K::Tab => "Tab".into(),
        K::BackTab => "BTab".into(),
        K::Backspace => "BSpace".into(),
        K::Delete => "DC".into(),
        K::Up => "Up".into(),
        K::Down => "Down".into(),
        K::Left => "Left".into(),
        K::Right => "Right".into(),
        K::Home => "Home".into(),
        K::End => "End".into(),
        K::PageUp => "PPage".into(),
        K::PageDown => "NPage".into(),
        _ => return None,
    })
}

/// Send a key produced by `tmux_key`, literal or named.
pub fn forward(pane: &str, key: &str) -> Result<(), String> {
    if let Some(literal) = key.strip_prefix('\u{0}') {
        tmux(&["send-keys", "-t", pane, "-l", "--", literal]).ok_or("send failed")?;
    } else {
        tmux(&["send-keys", "-t", pane, key]).ok_or("send failed")?;
    }
    Ok(())
}

/// Close scope-created tmux sessions that no longer have a live process in
/// them. Returns how many were removed.
pub fn prune() -> usize {
    let Some(out) = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\t#{pane_dead}\t#{pane_current_command}",
    ]) else {
        return 0;
    };
    let mut removed = 0;
    for line in out.lines() {
        let mut f = line.split('\t');
        let (Some(session), Some(dead), cmd) = (f.next(), f.next(), f.next().unwrap_or("")) else {
            continue;
        };
        if !session.starts_with("scope-") {
            continue;
        }
        let idle_shell = matches!(cmd, "bash" | "zsh" | "sh" | "fish");
        if dead == "1" || idle_shell {
            if kill_session(session).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

pub fn kill_session(session: &str) -> Result<(), String> {
    tmux(&["kill-session", "-t", session])
        .ok_or_else(|| "tmux kill-session failed".into())
        .map(|_| ())
}

/// Start a session with explicit Claude Code options.
pub fn new_session_with(
    cwd: &Path,
    model: Option<&str>,
    effort: Option<&str>,
    permission_mode: Option<&str>,
    prompt: Option<&str>,
) -> Result<String, String> {
    if !available() {
        return Err("tmux is not installed".into());
    }
    let name = next_name();
    let mut cmd = vec!["claude".to_string()];
    if let Some(m) = model {
        cmd.push("--model".into());
        cmd.push(m.into());
    }
    if let Some(e) = effort {
        cmd.push("--effort".into());
        cmd.push(e.into());
    }
    if let Some(p) = permission_mode {
        cmd.push("--permission-mode".into());
        cmd.push(p.into());
    }
    let cwd = cwd.to_string_lossy().to_string();
    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", &name, "-c", &cwd, "--"];
    args.extend(cmd.iter().map(String::as_str));
    tmux(&args).ok_or("tmux could not start the session (is claude on PATH?)")?;
    if let Some(p) = prompt {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        send_text(&format!("{name}:0.0"), p)?;
    }
    Ok(name)
}

/// Continue an existing conversation inside tmux, so a session that was
/// started in a plain terminal becomes steerable. The original window is left
/// alone; the user closes it once the adopted one is up.
/// A pane already running `claude --resume <session_id>`, if there is one.
pub fn adopted_pane(session_id: &str, panes: &[Pane]) -> Option<Pane> {
    panes.iter().find(|p| p.cmd.contains(session_id)).cloned()
}

pub fn adopt(cwd: &Path, session_id: &str) -> Result<String, String> {
    if !available() {
        return Err("tmux is not installed".into());
    }
    // Adopting the same conversation twice would leave two clients on one
    // session, so return the existing one instead of starting another.
    if let Some(p) = adopted_pane(session_id, &panes()) {
        return Ok(p.session);
    }
    let name = next_name();
    let cwd = cwd.to_string_lossy().to_string();
    tmux(&[
        "new-session",
        "-d",
        "-s",
        &name,
        "-c",
        &cwd,
        "--",
        "claude",
        "--resume",
        session_id,
    ])
    .ok_or("tmux could not start the session (is claude on PATH?)")?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUST: &str = "\
 Accessing workspace:
 /tmp
 Quick safety check: Is this a project you created or one you trust?
 Claude Code'll be able to read, edit, and execute files here.
 Security guide
 ❯ 1. Yes, I trust this folder
   2. No, exit
 Enter to confirm · Esc to cancel";

    const PERMISSION: &str = "\
│ Bash command                                        │
│   rm -rf build/                                     │
│   Remove the build directory                        │
│ Do you want to proceed?                             │
│ ❯ 1. Yes                                            │
│   2. Yes, and don't ask again for rm commands       │
│   3. No, and tell Claude what to do differently     │";

    const TRANSCRIPT: &str = "\
 Here are the steps:
   1. read the file
   2. change the import
   3. run the tests
 ❯ ";

    #[test]
    fn names_never_run_out() {
        assert_eq!(next_name_after(""), "scope-1");
        assert_eq!(next_name_after("work\nnotes"), "scope-1");
        assert_eq!(next_name_after("scope-1\nscope-2"), "scope-3");
        // The pool used to stop at 98; it must simply keep counting.
        let many: String = (1..=98).map(|n| format!("scope-{n}\n")).collect();
        assert_eq!(next_name_after(&many), "scope-99");
        assert_eq!(next_name_after("scope-7\nother\nscope-3"), "scope-8");
    }

    #[test]
    fn spots_a_conversation_already_adopted() {
        let panes = vec![
            Pane {
                id: "%1".into(),
                pid: 1,
                session: "scope-1".into(),
                cmd: "claude --resume abc-123".into(),
                cwd: "/tmp".into(),
            },
            Pane {
                id: "%2".into(),
                pid: 2,
                session: "work".into(),
                cmd: "bash".into(),
                cwd: "/tmp".into(),
            },
        ];
        assert_eq!(
            adopted_pane("abc-123", &panes).map(|p| p.session),
            Some("scope-1".into())
        );
        assert!(adopted_pane("def-456", &panes).is_none());
    }

    #[test]
    fn reads_a_trust_prompt() {
        let a = pending_approval(TRUST).expect("trust prompt should be seen");
        assert!(a.question.contains("trust"));
        assert_eq!(a.options.len(), 2);
        assert!(a.options[0].starts_with("1. Yes"));
    }

    #[test]
    fn reads_a_permission_prompt() {
        let a = pending_approval(PERMISSION).expect("permission prompt should be seen");
        assert_eq!(a.question, "Do you want to proceed?");
        assert_eq!(a.options.len(), 3);
    }

    const WRAPPED: &str = "\
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for rm -f /tmp/scope-trust-test/x3.txt
      and echo
   3. No
 Esc to cancel · Tab to amend";

    #[test]
    fn keeps_options_whose_text_wrapped() {
        let a = pending_approval(WRAPPED).expect("prompt should be seen");
        assert_eq!(
            a.options.len(),
            3,
            "wrapped text must not end the option list"
        );
        assert!(a.options[1].ends_with("and echo"));
        assert_eq!(a.options[2], "3. No");
    }

    #[test]
    fn ignores_a_numbered_list_that_is_not_a_prompt() {
        // No cursor on a numbered line, so nothing is waiting.
        assert!(pending_approval(TRANSCRIPT).is_none());
    }

    #[test]
    fn ignores_an_ordinary_prompt_line() {
        assert!(pending_approval("❯ Try \"fix typecheck errors\"").is_none());
    }
}

/// End the Claude Code process behind a session. It runs its terminal in raw
/// mode, so Ctrl-C never reaches it as a signal and SIGINT terminates it
/// outright — which is what is wanted when moving a conversation into tmux.
pub fn end_process(pid: i64) -> bool {
    Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Session ids that a pane is currently resuming. A session adopted into tmux
/// is running even when the registry has not caught up, and this is the only
/// evidence of that until it does.
pub fn adopted_ids(panes: &[Pane]) -> std::collections::HashSet<String> {
    panes
        .iter()
        .filter_map(|p| {
            let rest = p.cmd.split("--resume").nth(1)?;
            rest.split_whitespace().next().map(str::to_string)
        })
        .collect()
}
