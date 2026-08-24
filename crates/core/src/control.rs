//! What Ironsight can do to a session, whichever way it reaches one.
//!
//! Steering needs the terminal a session is running inside. Unix has tmux,
//! which already holds sessions that outlive Ironsight, so that is the backend
//! there. Windows has neither tmux nor any way to reach into a console another
//! process owns, so Ironsight hosts the pseudo-terminal itself — see `host`. Both
//! backends offer the same functions under the same names, and the rest of
//! Ironsight is written against these rather than against either one.

/// Where sessions live.
///
/// This used to be settled at compile time — tmux on Unix, Ironsight's own
/// pseudo-terminals on Windows — which made "self-contained" a thing you could
/// only have by not having tmux. It is now one decision, made once at startup
/// and asked for by name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    /// tmux holds them. Outlives Ironsight, and needs tmux installed.
    Tmux,
    /// This process holds them. Nothing else to install, and they end when it
    /// does — which is why it is not the default anywhere a person is watching.
    Hosted,
    /// A daemon of Ironsight's own holds them. Nothing else to install, and they
    /// outlive every window.
    Daemon,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Tmux => "tmux",
            Backend::Hosted => "in-process",
            Backend::Daemon => "daemon",
        }
    }
}

static CHOSEN: std::sync::OnceLock<Backend> = std::sync::OnceLock::new();

/// Which backend this run is using. Decided once; asking again is free.
pub fn backend() -> Backend {
    *CHOSEN.get_or_init(choose)
}

/// Pick one, for a person who has said nothing about it.
///
/// tmux still wins by default where it exists. It is what any sessions already
/// running are in, and switching underneath them would make a fleet vanish from
/// the list. `IRONSIGHT_BACKEND=daemon` opts in; when the daemon has been lived
/// with, that becomes the default and this comment is the thing to delete.
fn choose() -> Backend {
    let asked = std::env::var("IRONSIGHT_BACKEND").ok();
    #[cfg(windows)]
    let has_tmux = false;
    #[cfg(not(windows))]
    let has_tmux = crate::tmux::available();
    chosen_from(asked.as_deref(), has_tmux, cfg!(windows))
}

/// The rule, separated from the world so it can be checked.
///
/// Reading the environment and deciding are two different jobs, and only one of
/// them has anything worth getting wrong.
fn chosen_from(asked: Option<&str>, has_tmux: bool, windows: bool) -> Backend {
    if let Some(asked) = asked {
        match asked.trim().to_lowercase().as_str() {
            "daemon" | "self" | "ironsight" => return Backend::Daemon,
            "hosted" | "process" => return Backend::Hosted,
            // Asking for tmux where there is none would leave every session
            // unreachable, so it is honoured only if it can be.
            "tmux" if has_tmux => return Backend::Tmux,
            _ => {}
        }
    }
    if windows {
        return Backend::Hosted;
    }
    if has_tmux {
        Backend::Tmux
    } else {
        Backend::Daemon
    }
}

/// Call the same function on whichever backend is in charge.
macro_rules! on_backend {
    ($name:ident ( $($arg:expr),* )) => {{
        #[cfg(windows)]
        {
            crate::host::$name($($arg),*)
        }
        #[cfg(not(windows))]
        {
            match backend() {
                Backend::Tmux => crate::tmux::$name($($arg),*),
                Backend::Hosted => crate::host::$name($($arg),*),
                Backend::Daemon => crate::daemon::backend::$name($($arg),*),
            }
        }
    }};
}

/// Whether sessions survive Ironsight exiting.
pub fn outlives_ironsight() -> bool {
    on_backend!(outlives_ironsight())
}

/// What holds them, for saying so to a person.
pub fn where_backend() -> &'static str {
    on_backend!(where_name())
}

pub fn available() -> bool {
    on_backend!(available())
}

pub fn panes() -> Vec<Pane> {
    on_backend!(panes())
}

pub fn pane_for(pid: i64, cwd: &str, panes: &[Pane]) -> Option<Pane> {
    on_backend!(pane_for(pid, cwd, panes))
}

pub fn send_text(pane: &str, text: &str) -> Result<(), String> {
    on_backend!(send_text(pane, text))
}

pub fn send_key(pane: &str, key: &str) -> Result<(), String> {
    on_backend!(send_key(pane, key))
}

pub fn forward_key(pane: &str, code: crossterm::event::KeyCode, ctrl: bool) -> Result<(), String> {
    on_backend!(forward_key(pane, code, ctrl))
}

pub fn inside_tmux() -> bool {
    on_backend!(inside_tmux())
}

pub fn hold_way_back() -> bool {
    on_backend!(hold_way_back())
}

pub fn drop_way_back(held: bool) {
    on_backend!(drop_way_back(held))
}

pub fn attach(session: &str) -> Result<bool, String> {
    on_backend!(attach(session))
}

pub fn release_frame(pane: &str) {
    on_backend!(release_frame(pane))
}

pub fn frame(pane: &str, cols: u16, rows: u16) -> Option<crate::screen::Frame> {
    on_backend!(frame(pane, cols, rows))
}

pub fn capture(pane: &str) -> Option<String> {
    on_backend!(capture(pane))
}

pub fn prune() -> Vec<String> {
    on_backend!(prune())
}

pub fn kill_session(session: &str) -> Result<(), String> {
    on_backend!(kill_session(session))
}

pub fn new_session_with(
    cwd: &std::path::Path,
    argv: &[String],
    opening: &[String],
) -> Result<String, String> {
    on_backend!(new_session_with(cwd, argv, opening))
}

pub fn adopt(cwd: &std::path::Path, session_id: &str) -> Result<String, String> {
    on_backend!(adopt(cwd, session_id))
}

pub fn end_process(pid: i64) -> bool {
    on_backend!(end_process(pid))
}

pub fn open_window(session: &str) -> Result<String, String> {
    on_backend!(open_window(session))
}

/// End every session Ironsight can end: the terminals, and the ones it holds by
/// pipe. A person who says "close everything" does not mean "close the ones
/// that happen to have a terminal".
pub fn stop_all() -> Vec<String> {
    let mut names: Vec<String> = on_backend!(stop_all());
    names.extend(owned_stop_all());
    names
}

/// End every owned session. Separate from [`stop_all`] so that the pieces can
/// be tested apart from a terminal backend.
pub fn owned_stop_all() -> Vec<String> {
    match owned_home() {
        Home::Here => crate::owned::stop_all(),
        Home::Daemon => owned_all()
            .into_iter()
            .filter(|o| owned_stop(&o.name).is_ok())
            .map(|o| o.name)
            .collect(),
    }
}

pub fn attach_hint(session: &str) -> String {
    on_backend!(attach_hint(session))
}

pub fn steer_hint(name: &str) -> String {
    on_backend!(steer_hint(name))
}

pub fn unavailable_hint() -> &'static str {
    on_backend!(unavailable_hint())
}

pub fn where_hint(session: &str) -> String {
    on_backend!(where_hint(session))
}

pub fn hosted_count() -> usize {
    on_backend!(hosted_count())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Pane {
    pub id: String,
    pub pid: i64,
    pub session: String,
    /// the command the pane was started with, e.g. "claude --resume <id>"
    pub cmd: String,
    pub cwd: String,
}

/// What a session Ironsight started is called. Sessions made before the rename
/// carry the old one and are still ours: a rename must not orphan work that is
/// already running.
pub const PREFIX: &str = "ironsight-";
pub const FORMER_PREFIX: &str = "scope-";

/// Whether a session is one of ours, under either name.
pub fn is_ours(session: &str) -> bool {
    session.starts_with(PREFIX) || session.starts_with(FORMER_PREFIX)
}

/// The next free ironsight-N. Counting up from the highest existing name rather
/// than searching a fixed range means the pool can never be "full" — an early
/// version scanned scope-1..scope-98 and refused to start anything once those
/// were taken.
pub fn next_name_after(existing: &str) -> String {
    let highest = existing
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix(PREFIX)
                .or_else(|| l.strip_prefix(FORMER_PREFIX))
        })
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{PREFIX}{}", highest + 1)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub question: String,
    pub options: Vec<String>,
    /// what to type for each option. Claude Code draws a numbered list and
    /// takes the number; Aider writes `(Y)es/(N)o` and takes the letter. The
    /// answer is not always the position, so it is carried rather than assumed.
    pub keys: Vec<String>,
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
    if let Some(a) = letter_prompt(text) {
        return Some(a);
    }
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
    let keys = (1..=options.len()).map(|n| n.to_string()).collect();
    Some(Approval {
        question,
        options,
        keys,
    })
}

/// Answer a prompt by choosing option `n`, counting from one.
pub fn answer(pane: &str, n: usize) -> Result<(), String> {
    answer_with(pane, n, None)
}

/// What choosing option `n` of a prompt actually types.
///
/// A numbered prompt is answered by the number; a letter prompt by the letter
/// at that position. Pulled out of `answer_with` so the mapping can be tested
/// without a live pane — it is the part that, if it broke, would answer the
/// wrong thing while everything still looked fine.
pub fn keystroke_for(approval: &Approval, n: usize) -> String {
    approval
        .keys
        .get(n.saturating_sub(1))
        .cloned()
        .unwrap_or_else(|| n.to_string())
}

/// Answer with whatever that option is actually typed as.
pub fn answer_with(pane: &str, n: usize, approval: Option<&Approval>) -> Result<(), String> {
    let key = match approval {
        Some(a) => keystroke_for(a, n),
        None => n.to_string(),
    };
    send_text(pane, &key)
}

/// A prompt written as letters in brackets — `(Y)es/(N)o [Yes]:` — which is
/// how several agents ask, Aider among them. The bracketed letter is what it
/// wants typed.
fn letter_prompt(text: &str) -> Option<Approval> {
    // Only the last thing on screen can be what is being waited on.
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let line = line.trim();
    if !line.ends_with(':') && !line.ends_with("] ") && !line.ends_with(']') {
        return None;
    }
    let mut options = Vec::new();
    let mut keys = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else { break };
        let key = &after[..close];
        // A single letter in brackets, followed by the rest of the word.
        if key.chars().count() == 1 && key.chars().all(char::is_alphabetic) {
            let tail: String = after[close + 1..]
                .chars()
                .take_while(|c| c.is_alphabetic() || *c == '\'')
                .collect();
            options.push(format!("{key}{tail}"));
            keys.push(key.to_lowercase());
        }
        rest = &after[close + 1..];
    }
    if options.len() < 2 {
        return None;
    }
    // The question is everything before the choices begin — and "(recommended)"
    // is part of a question, not a choice, so the cut is at the first bracket
    // holding a single letter.
    let cut = line
        .char_indices()
        .find(|(i, c)| {
            *c == '(' && {
                let rest: Vec<char> = line[i + 1..].chars().take(2).collect();
                matches!(rest.as_slice(), [l, ')'] if l.is_alphabetic())
            }
        })
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let question = line[..cut].trim().trim_end_matches(':').trim().to_string();
    Some(Approval {
        question: if question.is_empty() {
            "waiting for an answer".into()
        } else {
            question
        },
        options,
        keys,
    })
}

// ── owned sessions ─────────────────────────────────────────────────────────
//
// A session held by pipe rather than by pseudo-terminal. Where it *lives* is a
// separate question from where pseudo-terminals live, because the two have
// nothing to do with each other: tmux cannot hold a pipe, and an owned session
// needs no terminal. So the rule here is its own, and it is a short one.

/// Where owned sessions are held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Home {
    /// The daemon holds them, so they outlive every window. What a person
    /// almost always wants: an agent that stops when you close a window is not
    /// a fleet.
    Daemon,
    /// This process holds them, and they end with it. Where there is no daemon
    /// to be had, or where the run has asked for nothing but itself.
    Here,
}

/// The rule, kept away from the world so it can be checked.
fn owned_home_from(asked: Option<&str>, unix: bool) -> Home {
    // No Unix socket, no daemon — the sessions have to live here.
    if !unix {
        return Home::Here;
    }
    // Someone who asked for everything in this process meant this too.
    if matches!(
        asked.map(|a| a.trim().to_lowercase()).as_deref(),
        Some("hosted") | Some("process")
    ) {
        return Home::Here;
    }
    Home::Daemon
}

/// Where this run holds owned sessions.
pub fn owned_home() -> Home {
    owned_home_from(
        std::env::var("IRONSIGHT_BACKEND").ok().as_deref(),
        cfg!(unix),
    )
}

/// Start an owned session, and return what it is.
///
/// Under a daemon this starts one if there is not one already: an owned session
/// is the first thing that needs a process outliving the window, so this is
/// where the daemon earns its keep even for someone whose terminals are in
/// tmux.
pub fn own(
    cwd: &std::path::Path,
    model: Option<&str>,
    mode: Option<&str>,
    opening: Option<&str>,
) -> Result<crate::owned::Owned, String> {
    match owned_home() {
        Home::Here => crate::owned::start(
            &claude_program(),
            cwd,
            model,
            mode,
            opening,
            std::time::Duration::from_secs(20),
        ),
        Home::Daemon => {
            crate::daemon::ensure_running()?;
            match crate::daemon::ask(&crate::daemon::Request::Own {
                cwd: cwd.to_string_lossy().into_owned(),
                model: model.map(str::to_string),
                mode: mode.map(str::to_string),
                opening: opening.map(str::to_string),
            })? {
                crate::daemon::Reply::Owned { it } => Ok(it),
                crate::daemon::Reply::Failed { why } => Err(why),
                other => Err(format!("the daemon answered {other:?}")),
            }
        }
    }
}

/// Every owned session there is.
///
/// Deliberately does not start a daemon. This is asked on every refresh, and a
/// question about what exists must not bring something into existence.
pub fn owned_all() -> Vec<crate::owned::Owned> {
    match owned_home() {
        Home::Here => crate::owned::list(),
        // Asked, not first checked whether there is anyone to ask: no daemon
        // and a daemon holding nothing are the same answer, and two round
        // trips leave a window where it dies between them.
        Home::Daemon => match crate::daemon::ask(&crate::daemon::Request::OwnedAll) {
            Ok(crate::daemon::Reply::OwnedAll { all }) => all,
            _ => Vec::new(),
        },
    }
}

/// Say something to one, by Ironsight's name for it or by its transcript id.
pub fn owned_say(who: &str, text: &str) -> Result<(), String> {
    match owned_home() {
        Home::Here => crate::owned::say(who, text),
        Home::Daemon => match crate::daemon::ask(&crate::daemon::Request::Say {
            who: who.to_string(),
            text: text.to_string(),
        })? {
            crate::daemon::Reply::Done => Ok(()),
            crate::daemon::Reply::Failed { why } => Err(why),
            other => Err(format!("the daemon answered {other:?}")),
        },
    }
}

/// End one.
pub fn owned_stop(who: &str) -> Result<(), String> {
    match owned_home() {
        Home::Here => crate::owned::stop(who),
        Home::Daemon => match crate::daemon::ask(&crate::daemon::Request::OwnedStop {
            who: who.to_string(),
        })? {
            crate::daemon::Reply::Done => Ok(()),
            crate::daemon::Reply::Failed { why } => Err(why),
            other => Err(format!("the daemon answered {other:?}")),
        },
    }
}

/// Forget the owned sessions that have exited, and say which.
pub fn owned_reap() -> Vec<String> {
    match owned_home() {
        Home::Here => crate::owned::reap(),
        Home::Daemon => match crate::daemon::ask(&crate::daemon::Request::OwnedReap) {
            Ok(crate::daemon::Reply::Names { names }) => names,
            _ => Vec::new(),
        },
    }
}

/// The command that runs Claude Code. On PATH by the name it installs as; an
/// owned session runs this in stream-json mode.
pub fn claude_program() -> String {
    std::env::var("IRONSIGHT_CLAUDE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude".to_string())
}

/// Whether a command line belongs to Claude Code. A native install runs as
/// `claude`, an npm one as `node .../claude-code/cli.js`, so the whole line is
/// searched rather than the executable name alone.
pub fn is_claude(args: &str) -> bool {
    args.contains("claude")
}

/// Terminals to try, in order, when opening a window of our own.
/// `$TERMINAL` wins if it is set, so anyone with a preference gets it.
const TERMINALS: [&str; 8] = [
    "kitty",
    "wezterm",
    "alacritty",
    "ghostty",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "xterm",
];

/// Run a command in a terminal window of its own. Used to watch a session
/// outside Ironsight, and to hand the whole thing over to the terminal view from
/// the desktop app.
pub fn open_terminal_with(command: &str) -> Result<String, String> {
    use std::process::{Command, Stdio};
    if cfg!(target_os = "macos") {
        // Terminal.app takes a script rather than an argv.
        let script = format!("tell app \"Terminal\" to do script \"{command}\"");
        return Command::new("osascript")
            .args(["-e", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| "Terminal".to_string())
            .map_err(|e| e.to_string());
    }
    if cfg!(windows) {
        return Command::new("cmd")
            .args(["/c", "start", "", "cmd", "/k", command])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| "cmd".to_string())
            .map_err(|e| e.to_string());
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    let preferred = std::env::var("TERMINAL").ok().filter(|t| !t.is_empty());
    let candidates: Vec<String> = preferred
        .into_iter()
        .chain(TERMINALS.iter().map(|t| t.to_string()))
        .collect();
    for term in candidates {
        // All of these accept `-e <command>`; gnome-terminal wants `--`.
        let mut cmd = Command::new(&term);
        if term.contains("gnome-terminal") {
            cmd.arg("--");
        } else {
            cmd.arg("-e");
        }
        cmd.args(&words);
        if cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return Ok(term);
        }
    }
    Err(format!("no terminal to open — run: {command}"))
}

/// A pane already running `claude --resume <session_id>`, if there is one.
pub fn adopted_pane(session_id: &str, panes: &[Pane]) -> Option<Pane> {
    panes.iter().find(|p| p.cmd.contains(session_id)).cloned()
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

#[cfg(test)]
mod backend_choice {
    use super::*;

    #[test]
    fn tmux_is_the_default_where_it_exists() {
        // Not because it is better, but because any sessions already running
        // are in it: switching underneath them would empty the list.
        assert_eq!(chosen_from(None, true, false), Backend::Tmux);
    }

    #[test]
    fn without_tmux_it_holds_them_itself() {
        assert_eq!(chosen_from(None, false, false), Backend::Daemon);
    }

    #[test]
    fn windows_has_no_choice_to_make() {
        assert_eq!(chosen_from(None, false, true), Backend::Hosted);
        assert_eq!(
            chosen_from(Some("tmux"), false, true),
            Backend::Hosted,
            "and asking for something that is not there does not conjure it"
        );
    }

    #[test]
    fn asking_is_honoured() {
        for word in ["daemon", "self", "ironsight", "DAEMON", " daemon "] {
            assert_eq!(
                chosen_from(Some(word), true, false),
                Backend::Daemon,
                "{word}"
            );
        }
        assert_eq!(chosen_from(Some("hosted"), true, false), Backend::Hosted);
    }

    #[test]
    fn asking_for_tmux_without_tmux_is_refused_rather_than_obeyed() {
        // Obeying would leave every session unreachable and nothing said.
        assert_eq!(chosen_from(Some("tmux"), false, false), Backend::Daemon);
    }

    #[test]
    fn nonsense_falls_back_rather_than_failing() {
        assert_eq!(chosen_from(Some("banana"), true, false), Backend::Tmux);
        assert_eq!(chosen_from(Some(""), false, false), Backend::Daemon);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn owned_sessions_live_in_the_daemon_unless_there_cannot_be_one() {
        use super::{Home, owned_home_from};
        assert_eq!(
            owned_home_from(None, true),
            Home::Daemon,
            "the default is the thing that outlives the window"
        );
        assert_eq!(
            owned_home_from(Some("tmux"), true),
            Home::Daemon,
            "where the terminals live says nothing about where a pipe lives"
        );
        assert_eq!(
            owned_home_from(Some("hosted"), true),
            Home::Here,
            "someone who asked for nothing but this process meant this too"
        );
        assert_eq!(
            owned_home_from(Some("HOSTED "), true),
            Home::Here,
            "asked for by name, however it was typed"
        );
        assert_eq!(
            owned_home_from(None, false),
            Home::Here,
            "no Unix socket, no daemon: they have to live here"
        );
    }

    use super::*;

    #[test]
    fn names_never_run_out() {
        assert_eq!(next_name_after(""), "ironsight-1");
        assert_eq!(next_name_after("work\nnotes"), "ironsight-1");
        assert_eq!(next_name_after("ironsight-1\nironsight-2"), "ironsight-3");
        // The pool used to stop at 98; it must simply keep counting.
        let many: String = (1..=98).map(|n| format!("ironsight-{n}\n")).collect();
        assert_eq!(next_name_after(&many), "ironsight-99");
        // Sessions started before the rename still count, so a new one cannot
        // be given a name that is already taken.
        assert_eq!(
            next_name_after("scope-7\nother\nironsight-3"),
            "ironsight-8"
        );
        assert!(is_ours("scope-4") && is_ours("ironsight-4") && !is_ours("work"));
    }

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

    // What Aider actually put on screen, captured from a session running on a
    // local model.
    const AIDER: &str = "\
────────────────────────────────────────────────────────
You can skip this check with --no-gitignore
Add .aider* to .gitignore (recommended)? (Y)es/(N)o [Yes]:";

    const AIDER_MANY: &str = "\
Add calc.py to the chat? (Y)es/(N)o/(A)ll/(S)kip all/(D)on't ask again [Yes]:";

    #[test]
    fn reads_a_prompt_that_wants_a_letter() {
        let a = pending_approval(AIDER).expect("aider is waiting on a person");
        assert_eq!(a.question, "Add .aider* to .gitignore (recommended)?");
        assert_eq!(a.options, vec!["Yes", "No"]);
        // The answer is the letter, not the position.
        assert_eq!(a.keys, vec!["y", "n"]);
    }

    #[test]
    fn reads_all_the_letters_offered() {
        let a = pending_approval(AIDER_MANY).expect("prompt should be seen");
        assert_eq!(a.options.len(), 5);
        assert_eq!(a.keys, vec!["y", "n", "a", "s", "d"]);
        assert_eq!(a.options[4], "Don't");
    }

    #[test]
    fn a_numbered_prompt_still_answers_with_its_number() {
        let a = pending_approval(PERMISSION).expect("permission prompt should be seen");
        assert_eq!(a.keys, vec!["1", "2", "3"]);
    }

    #[test]
    fn ordinary_output_is_not_a_letter_prompt() {
        assert!(pending_approval("see the (r)eadme for details").is_none());
        assert!(pending_approval("running tests (3 of 4):").is_none());
        assert!(pending_approval("").is_none());
    }

    #[test]
    fn ignores_an_ordinary_prompt_line() {
        assert!(pending_approval("❯ Try \"fix typecheck errors\"").is_none());
    }

    #[test]
    fn recognises_both_ways_claude_code_is_installed() {
        assert!(is_claude("claude"));
        assert!(is_claude("/Users/x/.local/bin/claude --resume abc"));
        assert!(is_claude(
            "node /Users/x/.npm-global/lib/node_modules/@anthropic-ai/claude-code/cli.js"
        ));
        assert!(!is_claude("node server.js"));
        assert!(!is_claude("-bash"));
    }
}
