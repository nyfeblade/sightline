//! What scope can do to a session, whichever way it reaches one.
//!
//! Steering needs the terminal a session is running inside. Unix has tmux,
//! which already holds sessions that outlive scope, so that is the backend
//! there. Windows has neither tmux nor any way to reach into a console another
//! process owns, so scope hosts the pseudo-terminal itself — see `host`. Both
//! backends offer the same functions under the same names, and the rest of
//! scope is written against these rather than against either one.

#[cfg(not(windows))]
pub use crate::tmux::{
    OUTLIVES_SCOPE, WHERE, adopt, attach, attach_hint, available, capture, end_process,
    forward_key, hosted_count, inside_tmux, kill_session, new_session_with, open_window, pane_for,
    panes, prune, send_key, send_text, steer_hint, stop_all, unavailable_hint, where_hint,
};

#[cfg(windows)]
pub use crate::host::{
    OUTLIVES_SCOPE, WHERE, adopt, attach, attach_hint, available, capture, end_process,
    forward_key, hosted_count, inside_tmux, kill_session, new_session_with, open_window, pane_for,
    panes, prune, send_key, send_text, steer_hint, stop_all, unavailable_hint, where_hint,
};

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
pub fn next_name_after(existing: &str) -> String {
    let highest = existing
        .lines()
        .filter_map(|l| l.trim().strip_prefix("scope-"))
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("scope-{}", highest + 1)
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
/// outside scope, and to hand the whole thing over to the terminal view from
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
mod tests {
    use super::*;

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
