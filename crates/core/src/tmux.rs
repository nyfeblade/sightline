//! Steering sessions through tmux, the backend everywhere but Windows.
//!
//! Claude Code's own cross-session channel is a token-authenticated private
//! socket, so scope drives the terminal instead: a session running in a tmux
//! pane can be typed into exactly as a person would type into it. That keeps
//! permission prompts, slash commands and every other interactive affordance
//! working, and it does not depend on Claude Code internals. Sessions outlive
//! scope, because tmux holds them, not scope.
//!
//! Sessions started outside tmux are observable but not steerable. Nothing here
//! fails loudly when tmux is missing — the control keys simply say so.

use crate::control::{Pane, adopted_pane, is_claude};
use std::path::Path;
use std::process::{Command, Stdio};

fn next_name() -> String {
    crate::control::next_name_after(
        &tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap_or_default(),
    )
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
pub fn pane_for(pid: i64, _cwd: &str, panes: &[Pane]) -> Option<Pane> {
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

/// The key that always means "back to scope", wherever you are: out of
/// passthrough, and out of a session shown full-screen or in its own window.
/// One key with one meaning, rather than tmux's prefix-then-letter, which is a
/// thing you have to know tmux to know.
/// A desktop or a terminal can take a key before tmux ever sees it — F12 is a
/// drop-down console in more than one setup — so it can be named.
pub fn way_back() -> String {
    std::env::var("SCOPE_WAY_BACK")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .unwrap_or_else(|| "F12".into())
}

/// What it should do depends on how you got to the session, and tmux can work
/// that out itself: a client that has a previous session came from scope and
/// switches back to it, and a client that was made by attaching has nowhere to
/// switch to, so it detaches.
const WAY_BACK_ACTION: [&str; 4] = [
    "if-shell",
    "-F",
    "#{client_last_session}",
    "switch-client -l",
];

/// Whether the key is free. tmux key tables belong to the whole server rather
/// than to one session, so a key someone has already bound is theirs, and scope
/// says the tmux way out instead of quietly taking it.
fn way_back_is_free() -> bool {
    let Some(table) = tmux(&["list-keys", "-T", "root"]) else {
        return false;
    };
    // `list-keys -T root F12` prints nothing even when F12 is bound, so the
    // table is read whole and the line looked for here.
    let key = way_back();
    !table.lines().any(|l| {
        let mut words = l.split_whitespace();
        words.any(|w| w == "root") && words.next() == Some(key.as_str())
    })
}

/// Take the key for as long as scope is running.
///
/// It used to be taken for the length of one attach, which meant the key worked
/// from `a` and from nowhere else — while the hint scope had written on the
/// session's status line stayed there, promising a key that was no longer
/// bound. Returns whether it was taken, which is also whether to promise it.
pub fn hold_way_back() -> bool {
    if !available() || !way_back_is_free() {
        return false;
    }
    let key = way_back();
    let mut args = vec!["bind-key", "-n", key.as_str()];
    args.extend(WAY_BACK_ACTION);
    args.push("detach-client");
    tmux(&args).is_some()
}

/// Give it back.
pub fn drop_way_back(held: bool) {
    if held {
        tmux(&["unbind-key", "-n", &way_back()]);
    }
}

/// Whether scope currently holds it, for anything that wants to say so.
pub fn holds_way_back() -> bool {
    tmux(&["list-keys", "-T", "root"])
        .map(|table| {
            let key = way_back();
            table.lines().any(|l| {
                let mut words = l.split_whitespace();
                words.any(|w| w == "root")
                    && words.next() == Some(key.as_str())
                    && l.contains("client_last_session")
            })
        })
        .unwrap_or(false)
}

/// What to tell someone about getting back, which depends on whether scope was
/// able to give them one key for it.
fn hint(taken: bool, tmux_way: &str) -> String {
    if taken {
        format!(" {} → back to scope ", way_back())
    } else {
        format!(" {tmux_way} → back to scope ")
    }
}

/// Show a session full-screen. Returns true when scope's own terminal was
/// handed over and must be taken back afterwards; false when the tmux client
/// was switched instead, which leaves scope running where it is.
pub fn attach(session: &str) -> Result<bool, String> {
    let held = holds_way_back();
    if inside_tmux() {
        show_way_back(session, &hint(held, "ctrl+b L"));
        tmux(&["switch-client", "-t", session]).ok_or("tmux switch-client failed")?;
        return Ok(false);
    }
    show_way_back(session, &hint(held, "ctrl+b d"));
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

/// Stop holding a session at the window's size. A session pinned to whatever
/// the app was showing it at would stay that shape long after the app closed,
/// so the size goes back to being tmux's business.
pub fn release_frame(pane: &str) {
    let session = pane.split(':').next().unwrap_or(pane);
    tmux(&["set-option", "-t", session, "window-size", "latest"]);
}

/// A session's screen at a given size, ready to be drawn by something that is
/// not a terminal.
///
/// The size matters: a session draws to whatever it is told the terminal is, so
/// showing one in a window means telling tmux the window's size, or its own
/// framing wraps at the wrong column. A detached session has no client to take
/// a size from, so it has to be set by hand.
pub fn frame(pane: &str, cols: u16, rows: u16) -> Option<crate::screen::Frame> {
    let session = pane.split(':').next().unwrap_or(pane);
    // One invocation, not three. tmux takes several commands at once, and each
    // separate call is a process spawn — which is most of what a frame costs
    // when it is being fetched many times a second.
    let out = tmux(&[
        "capture-pane",
        "-p",
        "-e",
        "-t",
        pane,
        ";",
        "display-message",
        "-p",
        "-t",
        pane,
        "#{window_width} #{window_height} #{cursor_y} #{cursor_x}",
    ])?;
    // The size line is last, and everything before it is the screen.
    let (render, size) = out
        .rsplit_once('\n')
        .map(|(a, b)| (a, b))
        .unwrap_or((&out, ""));
    let mut n = size
        .split_whitespace()
        .filter_map(|v| v.parse::<u16>().ok());
    let (have_cols, have_rows, cursor) = match (n.next(), n.next(), n.next(), n.next()) {
        (Some(w), Some(h), Some(y), Some(x)) => (w, h, (y, x)),
        _ => (cols, rows, (0, 0)),
    };
    // A session repaints when it feels like it, so its buffer is parsed at the
    // size it actually is; asking for a different one wraps every line in the
    // wrong place until it catches up.
    let mut frame = crate::screen::frame_from_render(render.as_bytes(), have_cols, have_rows);
    frame.cursor = cursor;
    if (have_cols, have_rows) != (cols, rows) {
        // Ask for the size the window wants; the next frame will have it.
        tmux(&["set-option", "-t", session, "window-size", "manual"]);
        tmux(&[
            "resize-window",
            "-t",
            session,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
        ]);
    }
    Some(frame)
}

/// What a session's pane currently shows. Used both for the live mirror and
/// for spotting a session that is blocked on a question.
pub fn capture(pane: &str) -> Option<String> {
    tmux(&["capture-pane", "-p", "-t", pane])
}

/// Translate a key press into the name tmux expects, so a whole keyboard can
/// be forwarded to a session.
fn tmux_key(code: crossterm::event::KeyCode, ctrl: bool) -> Option<String> {
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

/// Send one key press to a session, literal or named.
pub fn forward_key(pane: &str, code: crossterm::event::KeyCode, ctrl: bool) -> Result<(), String> {
    let Some(key) = tmux_key(code, ctrl) else {
        return Ok(());
    };
    if let Some(literal) = key.strip_prefix('\u{0}') {
        tmux(&["send-keys", "-t", pane, "-l", "--", literal]).ok_or("send failed")?;
    } else {
        tmux(&["send-keys", "-t", pane, &key]).ok_or("send failed")?;
    }
    Ok(())
}

/// One row of the process table: pid, parent pid, and the command line.
type Proc = (i64, i64, String);

fn parse_proc(line: &str) -> Option<Proc> {
    let mut f = line.split_whitespace();
    let pid = f.next()?.parse().ok()?;
    let ppid = f.next()?.parse().ok()?;
    Some((pid, ppid, f.collect::<Vec<_>>().join(" ")))
}

/// Every process on the machine, or None when it cannot be read. None means
/// "no idea", and nothing is treated as finished on no idea.
fn process_table() -> Option<Vec<Proc>> {
    // -ww: macOS ps cuts the command line to the terminal width, which can
    // drop the part that identifies Claude Code and make a working session
    // look finished.
    let out = Command::new("ps")
        .args(["-ww", "-eo", "pid=,ppid=,args="])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let table: Vec<Proc> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_proc)
        .collect();
    (!table.is_empty()).then_some(table)
}

/// True when `pid` or anything below it is a Claude Code process.
///
/// This is what decides whether a session is finished. The name tmux shows for
/// a pane says what is in the foreground at this instant, not whether the
/// session is alive, so it must never be the test on its own.
fn claude_in_tree(pid: i64, table: &[Proc]) -> bool {
    let mut frontier = vec![pid];
    let mut seen = std::collections::HashSet::new();
    while let Some(cur) = frontier.pop() {
        if !seen.insert(cur) {
            continue;
        }
        for (id, ppid, args) in table {
            if *id == cur && is_claude(args) {
                return true;
            }
            if *ppid == cur {
                frontier.push(*id);
            }
        }
    }
    false
}

/// Scope sessions with nothing running in them any more, given tmux's pane
/// list and a process table.
///
/// A pane counts as finished only on evidence: tmux marks it dead, or no Claude
/// Code process is left anywhere below it. A session with several panes is
/// finished only when every one of them is. An earlier version asked what the
/// pane was *showing* and called a shell finished, which reported live sessions
/// as over and then closed them — work nobody had asked to stop.
fn finished_in(rows: &str, table: Option<&[Proc]>) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut alive: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in rows.lines() {
        let mut f = line.split('\t');
        let (Some(session), Some(dead), pid) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if !session.starts_with("scope-") {
            continue;
        }
        if !order.iter().any(|s| s == session) {
            order.push(session.to_string());
        }
        let done = if dead == "1" {
            true
        } else {
            match (table, pid.and_then(|p| p.parse::<i64>().ok())) {
                (Some(t), Some(pid)) => !claude_in_tree(pid, t),
                // Cannot tell: assume it is working.
                _ => false,
            }
        };
        if !done {
            alive.insert(session.to_string());
        }
    }
    order.into_iter().filter(|s| !alive.contains(s)).collect()
}

/// Scope sessions whose process has exited, in tmux's order.
fn finished() -> Vec<String> {
    let Some(rows) = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\t#{pane_dead}\t#{pane_pid}",
    ]) else {
        return Vec::new();
    };
    finished_in(&rows, process_table().as_deref())
}

/// Close scope-created tmux sessions that no longer have a live process in
/// them. Returns the names that were closed.
pub fn prune() -> Vec<String> {
    finished()
        .into_iter()
        .filter(|s| kill_session(s).is_ok())
        .collect()
}

pub fn kill_session(session: &str) -> Result<(), String> {
    tmux(&["kill-session", "-t", session])
        .ok_or_else(|| "tmux kill-session failed".into())
        .map(|_| ())
}

/// Start a session with explicit Claude Code options.
/// Start a session in its own tmux session, running whatever agent was asked
/// for, and type the opening lines into it once it is up.
pub fn new_session_with(cwd: &Path, argv: &[String], opening: &[String]) -> Result<String, String> {
    if !available() {
        return Err("tmux is not installed".into());
    }
    let program = argv.first().cloned().unwrap_or_default();
    let name = crate::control::next_name_after(
        &tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap_or_default(),
    );
    let cwd = cwd.to_string_lossy().to_string();
    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", &name, "-c", &cwd, "--"];
    args.extend(argv.iter().map(String::as_str));
    tmux(&args).ok_or(format!(
        "tmux could not start the session (is {program} on PATH?)"
    ))?;
    // tmux reports success for a session whose command it could not run: the
    // session exists for as long as it takes the shell to fail. Saying
    // "started" to that is worse than saying nothing, so this waits and looks.
    std::thread::sleep(std::time::Duration::from_millis(400));
    if tmux(&["has-session", "-t", &name]).is_none() {
        return Err(format!(
            "{program} stopped as soon as it started — is it installed and on PATH?"
        ));
    }
    // Give the agent a moment to draw its prompt before typing into it.
    if !opening.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        for line in opening {
            send_text(&format!("{name}:0.0"), line)?;
        }
    }
    Ok(name)
}

/// Continue an existing conversation inside tmux, so a session that was
/// started in a plain terminal becomes steerable. The original window is left
/// alone; the user closes it once the adopted one is up.
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

/// Open a session in a new terminal window, attached to its tmux session, so
/// it can be watched without giving up the scope view.
pub fn open_window(session: &str) -> Result<String, String> {
    // A window opened this way is a client like any other, so it gets the same
    // way back — and is told the truth about which one.
    show_way_back(session, &hint(holds_way_back(), "ctrl+b d"));
    crate::control::open_terminal_with(&format!("tmux attach -t {session}"))
}

/// Close every session scope started or adopted, leaving any tmux session the
/// user made themselves alone. Returns the names that were closed.
pub fn stop_all() -> Vec<String> {
    let Some(out) = tmux(&["list-sessions", "-F", "#{session_name}"]) else {
        return Vec::new();
    };
    let mut closed = Vec::new();
    for name in out.lines().filter(|n| n.starts_with("scope-")) {
        if kill_session(name).is_ok() {
            closed.push(name.to_string());
        }
    }
    closed
}

/// Whether sessions outlive the scope process that started them. tmux holds
/// them, so they do; that is what makes the one-shot subcommands meaningful.
pub const OUTLIVES_SCOPE: bool = true;

/// What to call the place scope steers sessions from, in a sentence.
pub const WHERE: &str = "tmux";

/// How to look at a session outside scope.
pub fn attach_hint(session: &str) -> String {
    format!("attach with: tmux attach -t {session}")
}

/// Why a session cannot be steered, and what to do about it. The answer is
/// backend-shaped: here a session has to be running inside tmux.
pub fn steer_hint(name: &str) -> String {
    format!("{name} is not running in tmux — press A to adopt it")
}

/// Why nothing at all can be steered, when that is the case.
pub fn unavailable_hint() -> &'static str {
    "tmux is not installed"
}

/// Where a session scope can steer is running, for the session card.
pub fn where_hint(session: &str) -> String {
    format!("steerable · tmux {session}")
}

/// Sessions that would end if scope exited now. tmux holds its own, so none.
pub fn hosted_count() -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    // pane pid 100 runs claude; pane pid 200 is a shell that has a claude
    // several levels down; pane pid 300 is a shell with nothing in it.
    fn table() -> Vec<Proc> {
        vec![
            (100, 1, "claude".into()),
            (101, 100, "bash -c cargo test".into()),
            (200, 1, "-bash".into()),
            (
                201,
                200,
                "sh -c exec node /home/x/.npm/claude-code/cli.js".into(),
            ),
            (202, 201, "node /home/x/.npm/claude-code/cli.js".into()),
            (300, 1, "-bash".into()),
            (301, 300, "vim notes.md".into()),
        ]
    }

    #[test]
    fn finds_claude_below_a_shell() {
        let t = table();
        assert!(claude_in_tree(100, &t), "the pane process itself");
        assert!(claude_in_tree(200, &t), "two levels down, under npm");
        assert!(!claude_in_tree(300, &t), "nothing of ours in this one");
        assert!(!claude_in_tree(999, &t), "a pid that is not there");
    }

    #[test]
    fn a_pane_showing_a_shell_is_not_finished() {
        // The bug this replaced: pane 200 shows a shell while claude runs a
        // command below it, and tidying up closed the session mid-turn.
        let rows = "scope-1\t0\t200\nscope-2\t0\t300\n";
        assert_eq!(
            finished_in(rows, Some(&table())),
            vec!["scope-2".to_string()]
        );
    }

    #[test]
    fn a_dead_pane_is_finished_whatever_the_process_table_says() {
        let rows = "scope-1\t1\t100\n";
        assert_eq!(
            finished_in(rows, Some(&table())),
            vec!["scope-1".to_string()]
        );
    }

    #[test]
    fn nothing_is_finished_when_the_process_table_is_unreadable() {
        let rows = "scope-1\t0\t300\nscope-2\t0\t999\n";
        assert!(
            finished_in(rows, None).is_empty(),
            "no idea means leave it alone"
        );
    }

    #[test]
    fn a_session_lives_while_any_of_its_panes_does() {
        // Split window: one pane finished, the other still working.
        let rows = "scope-1\t0\t300\nscope-1\t0\t100\n";
        assert!(finished_in(rows, Some(&table())).is_empty());
    }

    #[test]
    fn leaves_sessions_scope_did_not_start_alone() {
        let rows = "work\t1\t300\nnotes\t0\t999\n";
        assert!(finished_in(rows, Some(&table())).is_empty());
    }
}
