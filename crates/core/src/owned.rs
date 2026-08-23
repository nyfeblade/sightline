//! Sessions Ironsight owns, spoken to over a protocol instead of a terminal.
//!
//! Everything else here watches sessions a person started in their own
//! terminal: it reads the transcript they leave and scrapes the screen they
//! draw, because that is all an outsider gets. A session Ironsight *starts* has
//! a better option. Claude Code will speak structured JSON — one object per
//! line in, one per line out — with no pseudo-terminal in the middle:
//!
//!     claude -p --verbose --input-format stream-json --output-format stream-json
//!
//! That turns three fragile things solid. Sending a message is a write to a
//! pipe rather than keystrokes a program has to render and echo. What came back
//! arrives as events with real fields — the tool, its input, the result, the
//! cost — rather than being reconstructed from a file on a poll. And a format
//! change here is a documented, versioned interface changing, not a screen
//! that quietly stops parsing.
//!
//! This module is the adapter: the protocol parsed into Ironsight's own event
//! model, and the process driven over its pipes. It does not replace watched
//! sessions — those are why Ironsight exists — it adds a second kind that a
//! foreman and a chief can drive without a terminal in the way.
//!
//! What it does not yet do: interactive permissions. In this mode a tool is
//! allowed or refused by the session's configured settings; routing each
//! decision to a person the way the terminal view does needs Claude Code's
//! `--permission-prompt-tool` seam, which is its own piece of work. Until then
//! an owned session runs under whatever permissions it was started with.

use crate::bus::{Event, Kind};
use serde_json::Value;

/// One line of the output stream, parsed into the events it means.
///
/// Pure and total: an unknown or half-written line yields nothing rather than
/// failing, because the far end is a process whose last line may be truncated
/// and whose vocabulary may grow.
pub fn parse_line(line: &str, session: &str, agent: &str) -> Vec<Event> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let ev = |kind: Kind| Event::new(session, agent, kind);
    let mut out = Vec::new();

    match msg.get("type").and_then(Value::as_str) {
        // The session announces itself: model, tools, where it is working.
        Some("system") if msg.get("subtype").and_then(Value::as_str) == Some("init") => {
            let cwd = msg
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.push(ev(Kind::SessionStarted {
                cwd,
                branch: String::new(),
            }));
        }
        // What the agent did and said, block by block.
        Some("assistant") => {
            for block in blocks(&msg) {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let tool = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        let summary = block
                            .get("input")
                            .map(|i| crate::event::tool_summary(&tool, i))
                            .unwrap_or_default();
                        out.push(ev(Kind::ToolCalled {
                            tool,
                            summary: crate::redact::text(&crate::event::clip(&summary, 200)),
                        }));
                    }
                    // Text and thinking are conversation, not fleet events —
                    // the same rule the transcript stream follows.
                    _ => {}
                }
            }
            out.push(ev(Kind::SessionWorking { tool: None }));
        }
        // Tool results come back inside a user message.
        Some("user") => {
            for block in blocks(&msg) {
                if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                    let failed = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if failed {
                        let summary = block.get("content").map(value_text).unwrap_or_default();
                        out.push(ev(Kind::ToolFailed {
                            tool: "tool".into(),
                            summary: crate::event::clip(&summary, 200),
                        }));
                    }
                }
            }
        }
        // A turn finished: what it cost, and back to waiting on the person.
        Some("result") => {
            let output = msg
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let estimate = msg
                .get("total_cost_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if output > 0 || estimate > 0.0 {
                out.push(ev(Kind::CostSpent { output, estimate }));
            }
            out.push(ev(Kind::SessionWaiting));
        }
        _ => {}
    }
    out
}

/// The content blocks of an assistant or user message, whatever shape it is in.
fn blocks(msg: &Value) -> Vec<Value> {
    msg.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A tool result's content, which may be a string or an array of blocks.
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

/// The command line that starts an owned session.
///
/// Kept in one place because it is the whole contract with Claude Code: change
/// a flag and the parser above is talking to something else.
pub fn argv(model: Option<&str>) -> Vec<String> {
    let mut v = vec![
        "-p".to_string(),
        "--verbose".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
    ];
    if let Some(m) = model {
        v.push("--model".into());
        v.push(m.to_string());
    }
    v
}

/// One user message, framed the way the input stream expects it.
pub fn user_message(text: &str) -> String {
    let msg = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text }
    });
    msg.to_string()
}

/// A running owned session: the process, its pipes, and a thread turning its
/// output into events.
///
/// Deliberately small. It holds the child and a writer to its stdin; the
/// reading happens on a thread that hands each event to a callback, so the
/// caller is never blocked on the process and the process is never blocked on
/// the caller.
pub struct OwnedSession {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    session: String,
}

impl OwnedSession {
    /// Start one. `program` is the agent's command (usually `claude`), so a
    /// test can point it at a shim and prove the plumbing without an API call.
    pub fn start(
        program: &str,
        cwd: &std::path::Path,
        model: Option<&str>,
        session: &str,
        agent: &str,
        mut on_event: impl FnMut(Event) + Send + 'static,
    ) -> std::io::Result<Self> {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let mut child = Command::new(program)
            .args(argv(model))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (session_owned, agent_owned) = (session.to_string(), agent.to_string());
        if let Some(stdout) = stdout {
            std::thread::Builder::new()
                .name("ironsight-owned-read".into())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        for ev in parse_line(&line, &session_owned, &agent_owned) {
                            on_event(ev);
                        }
                    }
                })?;
        }

        Ok(OwnedSession {
            child,
            stdin,
            session: session.to_string(),
        })
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    /// Send a message, the way a person typing would — but as a line of JSON.
    pub fn send(&mut self, text: &str) -> std::io::Result<()> {
        use std::io::Write;
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the session's input is closed",
            ));
        };
        writeln!(stdin, "{}", user_message(text))?;
        stdin.flush()
    }

    /// Close the input, which tells the session no more is coming.
    pub fn close_input(&mut self) {
        self.stdin.take();
    }

    /// Whether the process is still running.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> String {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude-stream-json.jsonl");
        std::fs::read_to_string(p).expect("fixture should be readable")
    }

    fn all_events() -> Vec<Event> {
        fixture()
            .lines()
            .flat_map(|l| parse_line(l, "sess", "claude"))
            .collect()
    }

    #[test]
    fn a_real_stream_json_run_becomes_the_events_it_means() {
        let events = all_events();
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.name()).collect();

        assert!(
            kinds.contains(&"sessionStarted"),
            "the init line announces the session: {kinds:?}"
        );
        assert!(
            kinds.contains(&"toolCalled"),
            "the tool_use block is a call: {kinds:?}"
        );
        assert!(
            kinds.contains(&"costSpent"),
            "the result line carries what the turn cost: {kinds:?}"
        );
        assert_eq!(
            kinds.last(),
            Some(&"sessionWaiting"),
            "and the turn ends waiting on the person"
        );
    }

    #[test]
    fn the_tool_and_its_arguments_survive() {
        let call = all_events()
            .into_iter()
            .find_map(|e| match e.kind {
                Kind::ToolCalled { tool, summary } => Some((tool, summary)),
                _ => None,
            })
            .expect("there is a call in the fixture");
        assert_eq!(call.0, "Bash", "the tool is named");
        assert!(
            call.1.contains("echo forty-two"),
            "and its command is there: {}",
            call.1
        );
    }

    #[test]
    fn spend_is_read_from_the_result() {
        let spent = all_events().into_iter().find_map(|e| match e.kind {
            Kind::CostSpent { output, estimate } => Some((output, estimate)),
            _ => None,
        });
        let (output, estimate) = spent.expect("the result carries usage");
        assert!(output > 0, "output tokens are read from usage");
        assert!(estimate > 0.0, "and the cost estimate from total_cost_usd");
    }

    #[test]
    fn a_torn_or_unknown_line_yields_nothing_rather_than_failing() {
        assert!(parse_line("{ half written", "s", "a").is_empty());
        assert!(parse_line("", "s", "a").is_empty());
        assert!(
            parse_line(r#"{"type":"rate_limit_event"}"#, "s", "a").is_empty(),
            "a message the adapter does not model is ignored, not an error"
        );
    }

    #[test]
    fn a_failed_tool_result_is_distinguished() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"command not found"}]}}"#;
        let events = parse_line(line, "s", "claude");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0].kind, Kind::ToolFailed { .. }));
    }

    #[test]
    fn a_credential_in_a_tool_call_is_redacted() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"deploy --token ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"}}]}}"#;
        let events = parse_line(line, "s", "claude");
        let summary = events.iter().find_map(|e| match &e.kind {
            Kind::ToolCalled { summary, .. } => Some(summary.clone()),
            _ => None,
        });
        assert!(
            !summary.unwrap_or_default().contains("ghp_"),
            "an owned session redacts the same as a watched one"
        );
    }

    #[test]
    fn the_input_frame_is_what_claude_code_expects() {
        let framed = user_message("hello there");
        let back: Value = serde_json::from_str(&framed).unwrap();
        assert_eq!(back["type"], "user");
        assert_eq!(back["message"]["role"], "user");
        assert_eq!(back["message"]["content"], "hello there");
    }

    #[cfg(unix)]
    #[test]
    fn drives_a_process_over_its_pipes_end_to_end() {
        use std::io::Write;
        use std::sync::mpsc;
        use std::time::Duration;

        // A shim standing in for `claude -p --input-format stream-json ...`. It
        // reads one user message from stdin and answers with a real init line,
        // a tool call, and a result — proving spawn, write, read and parse
        // without an API call.
        let dir = std::env::temp_dir().join("ironsight-owned-shim");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("fake-claude");
        std::fs::write(
            &shim,
            "#!/bin/sh\n\
             read _line\n\
             printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"cwd\":\"/tmp\",\"model\":\"x\"}'\n\
             printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"echo hi\"}}]}}'\n\
             printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"usage\":{\"output_tokens\":5},\"total_cost_usd\":0.01}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (tx, rx) = mpsc::channel();
        let mut session = OwnedSession::start(
            shim.to_str().unwrap(),
            &dir,
            None,
            "sess-1",
            "claude",
            move |ev| {
                let _ = tx.send(ev);
            },
        )
        .expect("the shim starts");

        session.send("do the thing").expect("a message is written");

        // Collect what the reader thread produces, with a bound so a hang fails
        // rather than hangs.
        let mut kinds = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && kinds.len() < 4 {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                kinds.push(ev.kind.name());
            }
        }
        assert!(kinds.contains(&"sessionStarted"), "init parsed: {kinds:?}");
        assert!(kinds.contains(&"toolCalled"), "tool parsed: {kinds:?}");
        assert!(kinds.contains(&"costSpent"), "result parsed: {kinds:?}");

        let _ = writeln!(std::io::sink(), "");
        session.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_argv_is_the_whole_contract() {
        let v = argv(Some("claude-opus-5"));
        assert!(v.contains(&"stream-json".to_string()));
        assert_eq!(
            v.iter().filter(|s| *s == "stream-json").count(),
            2,
            "in and out"
        );
        assert!(
            v.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "claude-opus-5")
        );
    }
}
