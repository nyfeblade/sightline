//! Sessions Ironsight owns, spoken to over a protocol instead of a terminal.
//!
//! Everything else here watches sessions a person started in their own
//! terminal: it reads the transcript they leave and scrapes the screen they
//! draw, because that is all an outsider gets. A session Ironsight *starts* has
//! a better option. Claude Code will speak structured JSON — one object per
//! line in, one per line out — with no pseudo-terminal in the middle:
//!
//! ```text
//! claude -p --verbose --input-format stream-json --output-format stream-json
//! ```
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
//! allowed or refused by the session's configured settings, so an owned session
//! runs under whatever permissions it was started with.
//!
//! That is a gap in this module, not in the tool. Claude Code does have the
//! seam — `--permission-prompt-tool mcp__host__approve` with `sdkMcpServers`
//! declared at `initialize` — and `docs/probes/control_protocol.py` proves a
//! host can allow, deny and rewrite each call over it, plus interrupt a turn and
//! change permission mode without a restart. Wiring it here is the first move in
//! `docs/ARCHITECTURE.md`, and this is where it lands.

use crate::bus::{Event, Kind};
use serde_json::Value;

/// One line of the output stream, parsed into the events it means.
///
/// Pure and total: an unknown or half-written line yields nothing rather than
/// failing, because the far end is a process whose last line may be truncated
/// and whose vocabulary may grow.
/// Turns the output stream into events, remembering just enough to attribute a
/// result to the call it answers.
///
/// A `tool_result` names the call it belongs to by id, not by tool — so to say
/// *which* tool failed, the parser has to remember the id→name pairs it saw go
/// out. That is the only state it keeps, and it is bounded by the calls in
/// flight. Everything else is a pure function of the line.
#[derive(Default)]
pub struct Parser {
    /// tool_use_id → the tool's name, for the calls not yet answered
    pending: std::collections::HashMap<String, String>,
    /// Claude Code's own id for this conversation, off the wire.
    ///
    /// Every line carries it, and it is the same for every turn of a session
    /// held open across messages — which makes it the thing that binds an owned
    /// session to the transcript at
    /// `~/.claude/projects/<slug>/<session_id>.jsonl` that every other view
    /// already reads. Without it an owned session would need a second
    /// implementation of everything Ironsight shows about a session.
    claude_session: Option<String>,
    /// The permission mode the agent says it is running under, off the init
    /// line. It is what decides every tool call in this mode, and it is the
    /// only honest name to put on a denial.
    mode: Option<String>,
    /// Whether the opening `system/init` has been seen.
    ///
    /// Claude Code writes one at the start of *every* turn, not once per
    /// session. Emitting `SessionStarted` for each would tell the fleet a new
    /// session began every time someone spoke to this one.
    announced: bool,
}

impl Parser {
    pub fn new() -> Self {
        Parser::default()
    }

    /// Claude Code's id for the conversation, once a line has carried it.
    pub fn claude_session(&self) -> Option<&str> {
        self.claude_session.as_deref()
    }

    /// The permission mode the agent reported at the start of a turn.
    pub fn mode(&self) -> Option<&str> {
        self.mode.as_deref()
    }

    /// One line in, the events it means out. Never fails: an unknown or
    /// half-written line yields nothing.
    pub fn feed(&mut self, line: &str, session: &str, agent: &str) -> Vec<Event> {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let ev = |kind: Kind| Event::new(session, agent, kind);
        let mut out = Vec::new();

        // Every line names the conversation it belongs to, whatever else it is
        // saying, so this is learned from the first line rather than only from
        // the init the session happens to open with.
        if self.claude_session.is_none() {
            if let Some(id) = msg.get("session_id").and_then(Value::as_str) {
                if !id.is_empty() {
                    self.claude_session = Some(id.to_string());
                }
            }
        }

        match msg.get("type").and_then(Value::as_str) {
            // The session announces itself: model, tools, where it is working.
            // Once only: a session held open across turns is announced again at
            // the start of each, and a fleet told a session started four times
            // has been told something untrue three times.
            Some("system") if msg.get("subtype").and_then(Value::as_str) == Some("init") => {
                if let Some(mode) = msg.get("permissionMode").and_then(Value::as_str) {
                    self.mode = Some(mode.to_string());
                }
                if !self.announced {
                    self.announced = true;
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
            }
            // A tool refused before it ran.
            //
            // In this mode nobody is asked: the session's permission settings
            // decide, and a call they do not allow is denied outright. That is
            // a decision made on the person's behalf, which is what
            // `PermissionAnswered` by a policy has always meant — this is the
            // first thing to produce one. Without it the refusal reaches the
            // fleet only as a tool that failed, and the reason a session is
            // getting nothing done stays invisible.
            Some("system")
                if msg.get("subtype").and_then(Value::as_str) == Some("permission_denied") =>
            {
                let tool = msg
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("a tool");
                out.push(ev(Kind::PermissionAnswered {
                    option: format!("denied · {tool}"),
                    by: crate::bus::By::Policy {
                        name: self
                            .mode
                            .clone()
                            .unwrap_or_else(|| "the session's permissions".into()),
                    },
                }));
            }
            // What the agent did and said, block by block.
            Some("assistant") => {
                let mut last_tool: Option<String> = None;
                for block in blocks(&msg) {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let tool = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        // Remember which call this is, so its result can be
                        // attributed to the right tool when it comes back.
                        if let Some(id) = block.get("id").and_then(Value::as_str) {
                            self.pending.insert(id.to_string(), tool.clone());
                        }
                        let summary = block
                            .get("input")
                            .map(|i| crate::event::tool_summary(&tool, i))
                            .unwrap_or_default();
                        out.push(ev(Kind::ToolCalled {
                            tool: tool.clone(),
                            summary: crate::redact::text(&crate::event::clip(&summary, 200)),
                        }));
                        last_tool = Some(tool);
                    }
                    // Text and thinking are conversation, not fleet events.
                }
                // Carry the tool it is running, the way the watched path does,
                // so the human render says "working · Bash" rather than a bare
                // "working" for the same turn.
                out.push(ev(Kind::SessionWorking { tool: last_tool }));
            }
            // Tool results come back inside a user message.
            Some("user") => {
                for block in blocks(&msg) {
                    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let failed = block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        // The result names its call; retire it from the pending
                        // set either way, so the map cannot grow without bound.
                        let tool = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .and_then(|id| self.pending.remove(id))
                            .unwrap_or_else(|| "tool".into());
                        if failed {
                            let summary = block.get("content").map(value_text).unwrap_or_default();
                            out.push(ev(Kind::ToolFailed {
                                tool,
                                // Redacted like every other thing that leaves —
                                // a failed result is exactly where a token in a
                                // curl error or an auth dump ends up.
                                summary: crate::redact::text(&crate::event::clip(&summary, 200)),
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
}

/// Parse a single line with no memory of the ones around it.
///
/// Fine for a line whose meaning is self-contained — an init, a call, a result.
/// A stream where a failure must name the tool that produced it wants a
/// [`Parser`] that persists across lines instead.
pub fn parse_line(line: &str, session: &str, agent: &str) -> Vec<Event> {
    Parser::new().feed(line, session, agent)
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
pub fn argv(spec: &Spec) -> Vec<String> {
    let mut v = vec![
        "-p".to_string(),
        "--verbose".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
    ];
    if let Some(m) = &spec.model {
        v.push("--model".into());
        v.push(m.clone());
    }
    // Nothing can be asked mid-run in this mode, so a session that is going to
    // need to edit files has to be started knowing that — otherwise every such
    // call is refused and the session spends its turn explaining why.
    if let Some(m) = &spec.mode {
        v.push("--permission-mode".into());
        v.push(m.clone());
    }
    // The two lists do different jobs, and it is worth being exact about which.
    //
    // `--allowedTools` *grants*: it does not narrow anything down. A session
    // started with `--allowedTools "Bash(echo *)"` ran `ls /tmp` quite happily,
    // because everything the machine's own settings already permit stays
    // permitted. What it is for is the opposite problem — a headless session
    // cannot be asked, so a command it needs and the settings do not cover is
    // simply refused, and the session spends its turn saying so. That is not
    // hypothetical: a chief with no grant could not run a single `ironsight`
    // command and correctly reported itself blocked.
    if !spec.allow.is_empty() {
        v.push("--allowedTools".into());
        for tool in &spec.allow {
            v.push(tool.clone());
        }
    }
    // `--disallowedTools` restricts, and is therefore the only tool-level
    // guarantee available here.
    if !spec.deny.is_empty() {
        v.push("--disallowedTools".into());
        for tool in &spec.deny {
            v.push(tool.clone());
        }
    }
    v
}

/// How an owned session is to be started.
///
/// A struct rather than four more arguments: every one of these is settled once
/// and cannot be changed while the session runs, so they belong together and
/// they travel together — through the daemon's wire, into `argv`, and into the
/// record the fleet keeps.
/// Every field is optional on the way in, so an older client can still be
/// understood — but an *unknown* field is refused rather than dropped. That is
/// not fussiness. This struct carries what a session is allowed to do, and a
/// daemon that quietly ignored a field it did not recognise would start a
/// session with fewer grants or fewer restrictions than the caller asked for,
/// and nothing would say so. It cost an afternoon once: a chief started through
/// a daemon built before `allow` existed could not run a single command, and
/// the fault was looked for everywhere except in the process holding it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Spec {
    /// The model, or the agent's own default.
    #[serde(default)]
    pub model: Option<String>,
    /// The permission mode for the life of the session.
    #[serde(default)]
    pub mode: Option<String>,
    /// Tools it may use without being asked. Grants; does not restrict.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Tools it may not use, whatever else it is allowed. Restricts.
    #[serde(default)]
    pub deny: Vec<String>,
    /// The message it begins on. Without one the agent says nothing at all —
    /// no init line, so no conversation id, so nothing to see.
    #[serde(default)]
    pub opening: Option<String>,
}

impl Spec {
    pub fn with_model(mut self, model: Option<&str>) -> Self {
        self.model = model.map(str::to_string);
        self
    }

    pub fn with_mode(mut self, mode: Option<&str>) -> Self {
        self.mode = mode.map(str::to_string);
        self
    }

    pub fn allowing(mut self, tools: &[&str]) -> Self {
        self.allow = tools.iter().map(|t| t.to_string()).collect();
        self
    }

    pub fn denying(mut self, tools: &[&str]) -> Self {
        self.deny = tools.iter().map(|t| t.to_string()).collect();
        self
    }

    pub fn opening(mut self, text: Option<&str>) -> Self {
        self.opening = text.map(str::to_string);
        self
    }
}

/// One user message, framed the way the input stream expects it.
pub fn user_message(text: &str) -> String {
    let msg = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text }
    });
    msg.to_string()
}

/// What an owned session looks like from outside it.
///
/// Deliberately thin. It is liveness and identity — enough for a front end to
/// list the session, know whether it can be spoken to, and find the transcript
/// that answers everything else. What the session *did* is read from that
/// transcript by the same code that reads every other session's, because an
/// owned session writes an ordinary one.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Owned {
    /// What Ironsight calls it: the handle a person or a foreman uses.
    pub name: String,
    pub cwd: String,
    /// The model it was asked for; empty means the agent's own default.
    pub model: String,
    /// The permission mode it was started under. Fixed for the life of the
    /// session: nothing can be asked once it is running.
    pub mode: String,
    /// Claude Code's id for the conversation — the transcript's name. Empty
    /// for the second or two before the agent's first line arrives.
    pub session_id: String,
    /// The agent's process id, so what it is costing the machine can be
    /// measured the same way a terminal session's is.
    pub pid: u32,
    /// Whether the process is still there.
    pub alive: bool,
    /// Whether it is mid-turn. True from the moment a message is sent until the
    /// turn's result line comes back, which is what a person means by "busy" —
    /// not whether a tool happens to be running this instant.
    pub busy: bool,
    /// The tool it is running, when it is running one.
    pub tool: String,
    /// Unix seconds: when it was started, and when it last said anything.
    pub started: i64,
    pub last: i64,
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// What becomes of the agent's stderr.
#[derive(Clone, Copy, Debug)]
pub enum Stderr {
    /// Discarded — a fleet backend that does not want it in a terminal.
    Quiet,
    /// Passed through to ours — a one-shot command that needs to see failures.
    Inherit,
}

/// A running owned session: the process, its pipes, and a thread turning its
/// output into events.
///
/// Deliberately small. It holds the child and a writer to its stdin; the
/// reading happens on a thread that hands each event to a callback, so the
/// caller is never blocked on the process and the process is never blocked on
/// the caller.
pub struct OwnedSession {
    /// The process, and the input to it, behind locks of their own.
    ///
    /// Two locks rather than one because the two things they protect fail
    /// differently. Writing can block for as long as a turn takes: the agent is
    /// thinking, it is not reading its input, and a pipe holds only so much.
    /// Killing must never wait for that — a session you cannot stop while it is
    /// busy is a session you cannot stop. With one lock, stopping waited behind
    /// the write it was trying to abandon; with two, the kill closes the pipe
    /// and the blocked write ends with it.
    child: std::sync::Mutex<std::process::Child>,
    stdin: std::sync::Mutex<Option<std::process::ChildStdin>>,
    session: String,
    /// Kept current by the reader thread, so asking what a session is doing
    /// costs neither of the locks above.
    state: std::sync::Arc<std::sync::Mutex<Owned>>,
}

/// Lock, and take the lock even if the last holder panicked. Nothing here is
/// left half-written by a panic — a poisoned lock would only make a session
/// unreachable for the life of the process.
fn take<T>(lock: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match lock.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    }
}

impl OwnedSession {
    /// Start one. `program` is the agent's command (usually `claude`), so a
    /// test can point it at a shim and prove the plumbing without an API call.
    pub fn start(
        program: &str,
        cwd: &std::path::Path,
        spec: &Spec,
        session: &str,
        agent: &str,
        on_event: impl FnMut(Event) + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::start_with(program, cwd, spec, session, agent, Stderr::Quiet, on_event)
    }

    /// Start one, choosing what happens to the agent's stderr.
    ///
    /// A one-shot command wants to see it: the tool's single failure mode —
    /// missing binary, unknown flag after a release, not logged in — writes
    /// only there, and swallowing it turns every such failure into a blank line
    /// or a hang. A fleet backend running many at once wants it quiet, so it
    /// does not scatter across a terminal. The caller decides.
    pub fn start_with(
        program: &str,
        cwd: &std::path::Path,
        spec: &Spec,
        session: &str,
        agent: &str,
        stderr: Stderr,
        mut on_event: impl FnMut(Event) + Send + 'static,
    ) -> std::io::Result<Self> {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let mut child = Command::new(program)
            .args(argv(spec))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(match stderr {
                Stderr::Quiet => Stdio::null(),
                Stderr::Inherit => Stdio::inherit(),
            })
            .spawn()?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (session_owned, agent_owned) = (session.to_string(), agent.to_string());
        let state = std::sync::Arc::new(std::sync::Mutex::new(Owned {
            name: session.to_string(),
            cwd: cwd.to_string_lossy().into_owned(),
            model: spec.model.clone().unwrap_or_default(),
            mode: spec.mode.clone().unwrap_or_default(),
            session_id: String::new(),
            pid: child.id(),
            alive: true,
            busy: false,
            tool: String::new(),
            started: now_secs(),
            last: now_secs(),
        }));
        let watched = state.clone();
        if let Some(stdout) = stdout {
            let spawned = std::thread::Builder::new()
                .name("ironsight-owned-read".into())
                .spawn(move || {
                    // One parser for the whole session, so a failed result can
                    // name the tool that produced it.
                    let mut parser = Parser::new();
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        let events = parser.feed(&line, &session_owned, &agent_owned);
                        if let Ok(mut st) = watched.lock() {
                            st.last = now_secs();
                            if st.session_id.is_empty() {
                                if let Some(id) = parser.claude_session() {
                                    st.session_id = id.to_string();
                                }
                            }
                            for ev in &events {
                                match &ev.kind {
                                    Kind::ToolCalled { tool, .. } => {
                                        st.busy = true;
                                        st.tool = tool.clone();
                                    }
                                    Kind::SessionWorking { tool } => {
                                        st.busy = true;
                                        st.tool = tool.clone().unwrap_or_default();
                                    }
                                    Kind::SessionWaiting => {
                                        st.busy = false;
                                        st.tool.clear();
                                    }
                                    _ => {}
                                }
                            }
                        }
                        for ev in events {
                            on_event(ev);
                        }
                    }
                    // stdout closing is the process ending: the most precise
                    // answer available, and it costs no poll.
                    if let Ok(mut st) = watched.lock() {
                        st.alive = false;
                        st.busy = false;
                        st.tool.clear();
                    }
                });
            // A thread that would not start must not leave the agent running as
            // an orphan: Child has no Drop that kills, and the OwnedSession that
            // would clean up is never constructed here.
            if let Err(e) = spawned {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }

        Ok(OwnedSession {
            child: std::sync::Mutex::new(child),
            stdin: std::sync::Mutex::new(stdin),
            session: session.to_string(),
            state,
        })
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    /// The live state itself, to be read without holding the session.
    ///
    /// The fleet keeps one of these beside each session so that asking what
    /// every session is doing never waits behind a write to one of them.
    pub fn shared_state(&self) -> std::sync::Arc<std::sync::Mutex<Owned>> {
        self.state.clone()
    }

    /// What it is: identity and liveness, as of the last line it wrote.
    pub fn state(&self) -> Owned {
        take(&self.state).clone()
    }

    /// Wait, briefly, for the agent to say which conversation this is.
    ///
    /// The id arrives on the first line the agent writes, a second or two after
    /// spawn. A caller that wants to hand back a session already bound to its
    /// transcript waits for it; one that would rather return immediately does
    /// not have to.
    pub fn settle(&self, wait: std::time::Duration) -> Option<String> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            let st = self.state();
            if !st.session_id.is_empty() {
                return Some(st.session_id);
            }
            if !st.alive || std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Send a message, the way a person typing would — but as a line of JSON.
    ///
    /// May block for as long as the agent takes to read it. Only the input is
    /// held while it does, so the session can still be asked about and still be
    /// stopped.
    pub fn send(&self, text: &str) -> std::io::Result<()> {
        use std::io::Write;
        let mut held = take(&self.stdin);
        let Some(stdin) = held.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the session's input is closed",
            ));
        };
        writeln!(stdin, "{}", user_message(text))?;
        stdin.flush()?;
        // Busy from the moment it is asked, not from the moment it answers.
        // The gap between the two is exactly when a second message would be
        // sent by someone who had been told the session was idle.
        if let Ok(mut st) = self.state.lock() {
            st.busy = true;
            st.last = now_secs();
        }
        Ok(())
    }

    /// Close the input, which tells the session no more is coming.
    pub fn close_input(&self) {
        take(&self.stdin).take();
    }

    /// Whether the process is still running.
    pub fn alive(&self) -> bool {
        matches!(take(&self.child).try_wait(), Ok(None))
    }

    /// End it. Takes only the process lock, so it works while a write to the
    /// same session is blocked — and closing the pipe is what lets that write
    /// end.
    pub fn stop(&self) {
        let mut child = take(&self.child);
        let _ = child.kill();
        let _ = child.wait();
        drop(child);
        let mut st = take(&self.state);
        st.alive = false;
        st.busy = false;
        st.tool.clear();
    }
}

impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── the fleet: owned sessions this process is holding ──────────────────────
//
// One process holds them and the rest ask it. In the ordinary case that
// process is the daemon, so an owned session outlives every window — which is
// the whole reason for owning one rather than shelling out. Where there is no
// daemon (Windows, or a fleet deliberately kept in-process) this is the same
// store in the front end, and the sessions end with it. Both are the same code
// under `control`, the way tmux and hosted pseudo-terminals already are.
//
// What is kept here is a process and a pipe. What the session *means* is read
// from the transcript it writes, by the code that reads every other session's,
// because an owned session writes an ordinary transcript. A store that also
// held meaning would be a second implementation of the whole program.

/// The name space for sessions Ironsight holds by pipe rather than by terminal.
///
/// Distinct from `ironsight-N` on purpose: those are terminal sessions, and a
/// person who types the name of one should not reach the other.
pub const PREFIX: &str = "owned-";

/// One session in the fleet: the session itself, and its state beside it.
///
/// The state is kept out of the session on purpose. Writing to a session can
/// block — the agent is mid-turn and not reading its input, and a pipe holds
/// only so much — and if listing the fleet had to touch each session to ask
/// what it was doing, one slow write would stall every window. Reading the
/// state never waits on a write; writing never waits on a reader.
/// How many events one session may hold before the oldest are dropped.
///
/// The front end drains these on its tick. A session that produced more than
/// this between two drains has lost the oldest, and the count says so rather
/// than the gap being silent — the same bargain the event socket already makes
/// with a slow consumer.
const HELD_EVENTS: usize = 2048;

struct Held {
    session: std::sync::Arc<OwnedSession>,
    state: std::sync::Arc<std::sync::Mutex<Owned>>,
    /// What the session has said since it was last drained, and how much was
    /// dropped because nobody drained in time.
    pending: std::sync::Arc<std::sync::Mutex<(std::collections::VecDeque<Event>, u64)>>,
}

impl Held {
    fn state(&self) -> Owned {
        take(&self.state).clone()
    }
}

type Fleet = std::collections::HashMap<String, Held>;

fn fleet() -> &'static std::sync::Mutex<Fleet> {
    static FLEET: std::sync::OnceLock<std::sync::Mutex<Fleet>> = std::sync::OnceLock::new();
    FLEET.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn locked() -> std::sync::MutexGuard<'static, Fleet> {
    match fleet().lock() {
        Ok(g) => g,
        // A panic in one session's caller must not make every other session
        // unreachable for the life of the process.
        Err(e) => e.into_inner(),
    }
}

fn hold(session: OwnedSession, pending: Pending) -> Held {
    let state = session.shared_state();
    Held {
        session: std::sync::Arc::new(session),
        state,
        pending,
    }
}

/// The buffer a session's reader thread fills and a front end drains.
type Pending = std::sync::Arc<std::sync::Mutex<(std::collections::VecDeque<Event>, u64)>>;

/// The next free `owned-N`, counting up from the highest taken.
///
/// Counting up rather than filling gaps means a name is never reused while
/// anything still remembers the session that had it — a reused name is how a
/// message ends up in the wrong conversation.
pub fn next_name(taken: &[String]) -> String {
    let highest = taken
        .iter()
        .filter_map(|n| n.strip_prefix(PREFIX))
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{PREFIX}{}", highest + 1)
}

/// Whether a name is one of this fleet's.
pub fn is_owned_name(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// Start one and keep it. Returns what it is, including its transcript id if
/// the agent named it within `settle`.
///
/// `program` is the agent's command, so a test can point it at a shim.
///
/// `opening` is sent as the session's first message. It is not a convenience:
/// Claude Code in this mode says nothing at all until it is spoken to — no
/// init line, so no conversation id, so nothing binding the session to a
/// transcript. A session started with nothing to do is a process holding a
/// pipe, and everything that would make it visible arrives only once someone
/// speaks to it.
pub fn start(
    program: &str,
    cwd: &std::path::Path,
    spec: &Spec,
    settle: std::time::Duration,
) -> Result<Owned, String> {
    let mut held = locked();
    let taken: Vec<String> = held.keys().cloned().collect();
    let name = next_name(&taken);
    let pending: Pending = std::sync::Arc::new(std::sync::Mutex::new((
        std::collections::VecDeque::new(),
        0,
    )));
    let session = OwnedSession::start_with(
        program,
        cwd,
        spec,
        &name,
        "claude",
        // Many of these may run at once under a daemon with no terminal;
        // their diagnostics must not scatter across whatever it inherited.
        Stderr::Quiet,
        // Kept, not dropped. What the session does arrives here live, on the
        // pipe, as it happens — the tool call before its result, the failure
        // with its reason. Ironsight used to learn the same things by re-reading
        // the transcript on a poll, which is archaeology: later, lossier, and
        // reconstructed rather than witnessed.
        //
        // Buffered rather than published, because this may be running inside
        // the daemon and the journal has exactly one writer. The front end
        // drains.
        {
            let mine = pending.clone();
            move |ev| {
                if let Ok(mut held) = mine.lock() {
                    if held.0.len() >= HELD_EVENTS {
                        held.0.pop_front();
                        held.1 += 1;
                    }
                    held.0.push_back(ev);
                }
            }
        },
    )
    .map_err(|e| format!("could not start an owned session: {e}"))?;
    if let Some(first) = &spec.opening {
        // A failure here is the session failing to start in the only sense that
        // matters: the process is up but nothing will ever come out of it.
        session
            .send(first)
            .map_err(|e| format!("{name} started but would not take its first message: {e}"))?;
        // Bound to its transcript before it is handed back, when the agent is
        // quick enough to say so. A caller that gets an empty id is not broken:
        // the reader thread fills it in and `list` will have it moments later.
        session.settle(settle);
    }
    let state = session.state();
    held.insert(name, hold(session, pending));
    Ok(state)
}

/// Everything the held sessions have said since the last drain, in order, with
/// the count of anything lost to a slow drain.
///
/// Draining is destructive on purpose: these are handed to whoever journals
/// them, and handing the same event to two front ends would number it twice.
pub fn drain() -> (Vec<Event>, u64) {
    let held = locked();
    let mut out: Vec<Event> = Vec::new();
    let mut lost = 0;
    for entry in held.values() {
        let mut pending = match entry.pending.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        out.extend(pending.0.drain(..));
        lost += pending.1;
        pending.1 = 0;
    }
    // The order within one session is the order it said things; across sessions
    // it is whatever the map gave, so time settles it.
    out.sort_by_key(|e| e.at);
    (out, lost)
}

/// Every owned session this process holds, dead ones included — a session that
/// has exited is still worth showing until someone clears it.
pub fn list() -> Vec<Owned> {
    let held = locked();
    let mut all: Vec<Owned> = held.values().map(Held::state).collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all
}

/// One of them, by the name Ironsight gave it or by the transcript id the agent
/// gave it. Both are how a caller might know it: a person says `owned-3`, a
/// front end that has matched it to a transcript says the session id.
pub fn get(who: &str) -> Option<Owned> {
    locked()
        .values()
        .map(Held::state)
        .find(|s| s.name == who || (!s.session_id.is_empty() && s.session_id == who))
}

fn find_key(fleet: &Fleet, who: &str) -> Option<String> {
    fleet
        .iter()
        .find(|(name, held)| {
            let st = held.state();
            name.as_str() == who || (!st.session_id.is_empty() && st.session_id == who)
        })
        .map(|(name, _)| name.clone())
}

/// Say something to one. The message goes down its stdin as a line of JSON.
///
/// The fleet is unlocked before the write. A session mid-turn may not be
/// reading its input, and a pipe holds only so much, so this can block for as
/// long as the turn takes — with the fleet held, that would stall every other
/// session and every window asking what the fleet is doing.
pub fn say(who: &str, text: &str) -> Result<(), String> {
    let (key, session) = {
        let fleet = locked();
        let key = find_key(&fleet, who).ok_or_else(|| format!("no owned session called {who}"))?;
        let held = fleet.get(&key).expect("just found");
        if !held.state().alive {
            return Err(format!("{key} has ended"));
        }
        (key.clone(), held.session.clone())
    };
    // Checked again with the session in hand: it may have exited between the
    // two, and writing to a dead pipe is a broken-pipe error rather than a
    // sentence saying what happened.
    if !session.state().alive {
        return Err(format!("{key} has ended"));
    }
    session.send(text).map_err(|e| e.to_string())
}

/// End one, and forget it.
pub fn stop(who: &str) -> Result<(), String> {
    let taken = {
        let mut fleet = locked();
        let key = find_key(&fleet, who).ok_or_else(|| format!("no owned session called {who}"))?;
        fleet.remove(&key)
    };
    // Killing it happens with the fleet unlocked, for the same reason writing
    // does: it waits on the process, and nothing else should wait on that.
    if let Some(held) = taken {
        end(&held);
    }
    Ok(())
}

/// Kill the process behind one, having already taken it out of the fleet.
fn end(held: &Held) {
    held.session.stop();
}

/// Forget the ones that have exited, and say which. What a person means by
/// "prune": nothing running is touched.
pub fn reap() -> Vec<String> {
    let mut fleet = locked();
    let dead: Vec<String> = fleet
        .iter()
        .filter(|(_, held)| !held.state().alive)
        .map(|(name, _)| name.clone())
        .collect();
    for name in &dead {
        fleet.remove(name);
    }
    dead
}

/// End every one of them. Returns the names that were stopped.
pub fn stop_all() -> Vec<String> {
    let taken: Vec<(String, Held)> = {
        let mut fleet = locked();
        fleet.drain().collect()
    };
    let mut names: Vec<String> = Vec::new();
    for (name, held) in taken {
        end(&held);
        names.push(name);
    }
    names.sort();
    names
}

/// How many are held here, running or not.
pub fn count() -> usize {
    locked().len()
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
        let mut parser = Parser::new();
        fixture()
            .lines()
            .flat_map(|l| parser.feed(l, "sess", "claude"))
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

    #[test]
    fn a_failed_result_names_the_tool_that_produced_it() {
        // A call goes out with an id; its failing result names that id, and the
        // parser remembers the pairing so the failure is attributed to Bash
        // rather than a generic "tool".
        let mut parser = Parser::new();
        parser.feed(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"false"}}]}}"#,
            "s",
            "claude",
        );
        let events = parser.feed(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_9","is_error":true,"content":"exit 1"}]}}"#,
            "s",
            "claude",
        );
        let tool = events.iter().find_map(|e| match &e.kind {
            Kind::ToolFailed { tool, .. } => Some(tool.clone()),
            _ => None,
        });
        assert_eq!(
            tool.as_deref(),
            Some("Bash"),
            "the failure is attributed to the tool that failed, not to \"tool\""
        );
    }

    #[test]
    fn a_credential_in_a_failed_result_is_redacted() {
        // This is where tokens actually leak: a curl error echoing a header, an
        // auth dump. It must be masked the same as a call.
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"curl error, sent Authorization: Bearer ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"}]}}"#;
        let summary = parse_line(line, "s", "claude")
            .into_iter()
            .find_map(|e| match e.kind {
                Kind::ToolFailed { summary, .. } => Some(summary),
                _ => None,
            })
            .expect("a failure event");
        assert!(
            !summary.contains("ghp_"),
            "a failed result reaches the journal and socket, so it is redacted: {summary}"
        );
    }

    #[test]
    fn working_carries_the_tool_it_is_running() {
        let events = parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"x.rs"}}]}}"#,
            "s",
            "claude",
        );
        let working = events.iter().find_map(|e| match &e.kind {
            Kind::SessionWorking { tool } => Some(tool.clone()),
            _ => None,
        });
        assert_eq!(
            working,
            Some(Some("Edit".to_string())),
            "working names the tool, matching the watched path rather than a bare \"working\""
        );
    }

    #[test]
    fn the_pending_set_does_not_grow_without_bound() {
        // Every result retires its call, failed or not, so the id→name map is
        // bounded by the calls actually in flight.
        let mut parser = Parser::new();
        for i in 0..100 {
            parser.feed(
                &format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"id{i}","name":"Bash","input":{{}}}}]}}}}"#),
                "s",
                "claude",
            );
            parser.feed(
                &format!(r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"id{i}","is_error":false,"content":"ok"}}]}}}}"#),
                "s",
                "claude",
            );
        }
        assert_eq!(
            parser.pending.len(),
            0,
            "successful results retire their calls too"
        );
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
        let session = OwnedSession::start(
            shim.to_str().unwrap(),
            &dir,
            &Spec::default(),
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
    fn the_conversation_id_is_learned_from_the_wire() {
        let mut parser = Parser::new();
        assert_eq!(parser.claude_session(), None, "nothing has been said yet");
        parser.feed(
            r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"abc-123"}"#,
            "owned-1",
            "claude",
        );
        assert_eq!(
            parser.claude_session(),
            Some("abc-123"),
            "the id that names the transcript is read off the stream"
        );
    }

    #[test]
    fn the_id_is_read_from_whatever_line_arrives_first() {
        // Not every stream opens with the init: a rate-limit notice can beat it
        // out, and it carries the id too.
        let mut parser = Parser::new();
        parser.feed(
            r#"{"type":"rate_limit_event","session_id":"xyz-9"}"#,
            "owned-1",
            "claude",
        );
        assert_eq!(parser.claude_session(), Some("xyz-9"));
    }

    #[test]
    fn a_session_is_announced_once_however_many_turns_it_runs() {
        // Claude Code writes a system/init at the start of every turn, not once
        // per session. Before this, a session held open for four messages told
        // the fleet it had started four times.
        let mut parser = Parser::new();
        let init = r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"s1"}"#;
        let mut started = 0;
        for _ in 0..4 {
            started += parser
                .feed(init, "owned-1", "claude")
                .iter()
                .filter(|e| matches!(e.kind, Kind::SessionStarted { .. }))
                .count();
            parser.feed(
                r#"{"type":"result","subtype":"success","usage":{"output_tokens":1},"total_cost_usd":0.001}"#,
                "owned-1",
                "claude",
            );
        }
        assert_eq!(
            started, 1,
            "four turns of one session are one session starting"
        );
    }

    #[test]
    fn owned_names_count_up_rather_than_filling_gaps() {
        assert_eq!(next_name(&[]), "owned-1");
        assert_eq!(
            next_name(&["owned-1".into(), "owned-2".into()]),
            "owned-3",
            "the next one after the highest"
        );
        assert_eq!(
            next_name(&["owned-1".into(), "owned-7".into()]),
            "owned-8",
            "a gap left by a stopped session is not reused: a reused name is \
             how a message reaches the wrong conversation"
        );
        assert!(is_owned_name("owned-3"));
        assert!(
            !is_owned_name("ironsight-3"),
            "a terminal session is not one of these"
        );
    }

    /// A stand-in for `claude -p --input-format stream-json`, which answers
    /// every message it is given and stays alive until its input closes —
    /// which is the property that makes an owned session a session rather than
    /// a command. Named per test so two running at once cannot share a file.
    #[cfg(unix)]
    fn multi_turn_shim(tag: &str, id: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ironsight-fleet-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("fake-claude");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\n\
                 while IFS= read -r _line; do\n\
                 printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"cwd\":\"/tmp\",\"session_id\":\"{id}\"}}'\n\
                 printf '%s\\n' '{{\"type\":\"assistant\",\"session_id\":\"{id}\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{{\"command\":\"echo hi\"}}}}]}}}}'\n\
                 printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"{id}\",\"usage\":{{\"output_tokens\":5}},\"total_cost_usd\":0.01}}'\n\
                 done\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        shim
    }

    /// The fleet is one store for the whole process, which is the point of it —
    /// so its tests take turns rather than racing each other for names.
    #[cfg(unix)]
    fn fleet_turn() -> std::sync::MutexGuard<'static, ()> {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        match TURN.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_fleet_holds_a_session_bound_to_its_transcript_and_speaks_to_it() {
        let _turn = fleet_turn();
        let shim = multi_turn_shim("held", "transcript-id-1");
        let dir = shim.parent().unwrap().to_path_buf();

        let started = start(
            shim.to_str().unwrap(),
            &dir,
            &Spec::default().opening(Some("do the thing")),
            std::time::Duration::from_secs(5),
        )
        .expect("the shim starts");

        assert!(
            is_owned_name(&started.name),
            "it is named: {}",
            started.name
        );
        assert_eq!(
            started.session_id, "transcript-id-1",
            "starting it binds it to the transcript it will write — without \
             that the session is invisible to every view that reads one"
        );
        assert!(started.alive);

        // It is in the list, and findable by either name.
        let listed = list();
        assert!(
            listed.iter().any(|o| o.name == started.name),
            "it is in the fleet: {listed:?}"
        );
        assert_eq!(
            get("transcript-id-1").map(|o| o.name.clone()),
            Some(started.name.clone()),
            "a front end that knows only the transcript id can still reach it"
        );

        // A second message goes to the same session, not a new one.
        say(&started.name, "and again").expect("it takes another message");
        let settled = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < settled {
            if get(&started.name).map(|o| !o.busy).unwrap_or(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let after = get(&started.name).expect("still held");
        assert_eq!(
            after.session_id, "transcript-id-1",
            "a second turn is the same conversation, not a second one"
        );
        assert!(!after.busy, "the turn finished, so it is waiting again");
        assert!(after.alive, "and it is still there between turns");

        stop(&started.name).expect("it stops");
        assert!(
            get(&started.name).is_none(),
            "a stopped session is forgotten"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_session_that_exits_is_shown_dead_and_will_not_take_messages() {
        let _turn = fleet_turn();
        // A shim that answers nothing and exits at once: the failure mode of a
        // missing binary, a bad flag, or an agent that is not logged in.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("ironsight-fleet-dead");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("fake-claude");
        std::fs::write(&shim, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = start(
            shim.to_str().unwrap(),
            &dir,
            &Spec::default(),
            std::time::Duration::from_millis(100),
        )
        .expect("it spawns, even though it will not last");

        let gone = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < gone {
            if get(&started.name).map(|o| !o.alive).unwrap_or(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let state = get(&started.name).expect("still listed");
        assert!(
            !state.alive,
            "the output stream closing is the process ending, and it is noticed"
        );
        assert!(
            say(&started.name, "hello").is_err(),
            "a dead session refuses a message rather than swallowing it"
        );

        let reaped = reap();
        assert!(
            reaped.contains(&started.name),
            "reaping clears the dead: {reaped:?}"
        );
        assert!(get(&started.name).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn one_session_that_will_not_read_does_not_stall_the_fleet() {
        // The wedge this guards against: a session mid-turn is not reading its
        // input, a pipe holds only so much, and the write blocks. If asking
        // what the fleet is doing had to wait behind that write, one busy
        // session would freeze every window on the machine.
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let _turn = fleet_turn();
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("ironsight-fleet-deaf");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Reads nothing, ever, and stays alive. Its input pipe fills and the
        // next write blocks — exactly an agent that is busy thinking.
        let shim = dir.join("fake-claude");
        // `exec`, so the shell does not linger as a second holder of the read
        // end of the pipe: killing it would then leave the write blocked on a
        // process nobody asked about, and the test would measure the shim
        // rather than the fleet.
        std::fs::write(&shim, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = start(
            shim.to_str().unwrap(),
            &dir,
            &Spec::default(),
            Duration::from_millis(50),
        )
        .expect("the shim starts");
        let name = started.name.clone();

        // Enough to overrun any pipe buffer, so the write is certainly still
        // in progress when the fleet is asked below.
        let writer = {
            let name = name.clone();
            std::thread::spawn(move || {
                let _ = say(&name, &"x".repeat(4 * 1024 * 1024));
            })
        };
        std::thread::sleep(Duration::from_millis(200));

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let at = Instant::now();
            let all = list();
            let _ = tx.send((all, at.elapsed()));
        });
        // Fifteen seconds, not five. What this separates is "did not block" from
        // "blocked until the shim exits", and the shim sleeps for thirty — so
        // the bound only has to sit between the two, and the wide gap is what
        // keeps it from failing on a machine that happens to be compiling.
        let answered = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("listing the fleet answered while a write to one session was blocked");
        assert!(
            answered.0.iter().any(|o| o.name == name),
            "and it is still the same fleet: {:?}",
            answered.0
        );

        // Stopping closes the pipe, which is what lets the blocked write end.
        // It must not wait for that write: a session you cannot stop while it
        // is busy is a session you cannot stop.
        let at = Instant::now();
        stop(&name).expect("it stops");
        assert!(
            at.elapsed() < Duration::from_secs(15),
            "stopping waited for the blocked write to finish: {:?}",
            at.elapsed()
        );
        let _ = writer.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_message_to_a_session_that_was_never_started_is_an_error() {
        let _turn = fleet_turn();
        assert!(
            say("owned-does-not-exist", "hello").is_err(),
            "no such session, said out loud"
        );
        assert!(stop("owned-does-not-exist").is_err());
    }

    #[test]
    fn a_refused_tool_is_a_decision_somebody_made_on_your_behalf() {
        // The real shape, off a live run: the stream says which tool was
        // refused and under which mode, and nobody was asked. Before this the
        // refusal reached the fleet only as a tool that failed, so a session
        // achieving nothing looked like a session with bad luck.
        let mut parser = Parser::new();
        parser.feed(
            r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"s1","permissionMode":"manual"}"#,
            "owned-1",
            "claude",
        );
        let events = parser.feed(
            r#"{"type":"system","subtype":"permission_denied","tool_name":"Write","tool_use_id":"toolu_1","message":"Claude requested permissions to write to /tmp/x, but you haven't granted it yet.","session_id":"s1"}"#,
            "owned-1",
            "claude",
        );
        let answered = events
            .iter()
            .find_map(|e| match &e.kind {
                Kind::PermissionAnswered { option, by } => Some((option.clone(), by.clone())),
                _ => None,
            })
            .expect("a denial is an answered permission: {events:?}");
        assert!(
            answered.0.contains("denied") && answered.0.contains("Write"),
            "it says what was refused: {}",
            answered.0
        );
        assert_eq!(
            answered.1,
            crate::bus::By::Policy {
                name: "manual".into()
            },
            "and that the session's own permission mode decided it, not a person"
        );
    }

    #[test]
    fn a_denial_before_any_mode_is_known_still_says_what_happened() {
        // A stream that opens with the denial — nothing has said which mode is
        // in force. Naming no policy at all would be worse than naming a vague
        // one: the event would read as though a person refused.
        let events = parse_line(
            r#"{"type":"system","subtype":"permission_denied","tool_name":"Bash"}"#,
            "owned-1",
            "claude",
        );
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                Kind::PermissionAnswered {
                    by: crate::bus::By::Policy { .. },
                    ..
                }
            )),
            "still a policy, still not a person: {events:?}"
        );
    }

    #[test]
    fn what_an_owned_session_may_do_is_settled_when_it_starts() {
        // Nothing can be asked of a session in this mode, so the permission
        // mode is the one lever there is — and a flag that was accepted and
        // then dropped would be the worst of both.
        let v = argv(&Spec::default().with_mode(Some("acceptEdits")));
        assert!(
            v.windows(2)
                .any(|w| w[0] == "--permission-mode" && w[1] == "acceptEdits"),
            "the mode reaches the agent: {v:?}"
        );
        assert!(
            !argv(&Spec::default())
                .iter()
                .any(|a| a == "--permission-mode"),
            "and nothing is invented when none was asked for"
        );
    }

    #[test]
    fn what_a_session_may_do_without_being_asked_reaches_the_agent() {
        // The half that is not a guarantee but is still necessary: a headless
        // session cannot be asked, so a command it needs and the machine's
        // settings do not cover is refused outright.
        let v = argv(&Spec::default().allowing(&["Bash(ironsight *)", "Read"]));
        let at = v
            .iter()
            .position(|a| a == "--allowedTools")
            .unwrap_or_else(|| panic!("the grant is passed: {v:?}"));
        assert_eq!(v[at + 1], "Bash(ironsight *)");
        assert_eq!(v[at + 2], "Read");
    }

    #[test]
    fn a_spec_field_nobody_knows_is_refused_rather_than_dropped() {
        // The failure this guards: a daemon built before a field existed reads
        // the request, ignores what it does not recognise, and starts a session
        // with different permissions from the ones asked for — silently. That
        // is how a chief ends up unable to run a single command with nothing
        // anywhere saying why.
        let ok: Result<Spec, _> = serde_json::from_str(r#"{"model":"opus"}"#);
        assert!(ok.is_ok(), "a spec with fields missing is still readable");
        let surprise: Result<Spec, _> =
            serde_json::from_str(r#"{"model":"opus","somethingNewer":["x"]}"#);
        assert!(
            surprise.is_err(),
            "but one carrying something this build does not understand is refused"
        );
    }

    #[test]
    fn a_grant_and_a_restriction_are_separate_lists() {
        // Conflating them would be the worst kind of wrong: an allow list read
        // as a restriction looks like a sandbox and is not one.
        let v = argv(
            &Spec::default()
                .allowing(&["Bash(ironsight *)"])
                .denying(&["Write"]),
        );
        let allow = v.iter().position(|a| a == "--allowedTools").unwrap();
        let deny = v.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(v[allow + 1], "Bash(ironsight *)");
        assert_eq!(v[deny + 1], "Write");
        assert!(allow < deny, "and both survive being passed together");
    }

    #[test]
    fn what_a_session_may_not_do_reaches_the_agent() {
        // A deny list, because an allow list does not restrict — checked
        // against the real tool, not assumed. This is the only tool-level
        // guarantee a supervised session has.
        let v = argv(&Spec::default().denying(&["Write", "Edit"]));
        let at = v
            .iter()
            .position(|a| a == "--disallowedTools")
            .expect("the deny list is passed: {v:?}");
        assert_eq!(
            &v[at + 1..at + 3],
            ["Write".to_string(), "Edit".to_string()]
        );
        assert!(
            !argv(&Spec::default())
                .iter()
                .any(|a| a == "--disallowedTools"),
            "and nothing is denied when nothing was asked to be"
        );
    }

    #[test]
    fn the_argv_is_the_whole_contract() {
        let v = argv(&Spec::default().with_model(Some("claude-opus-5")));
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
