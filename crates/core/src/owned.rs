//! Sessions Sightline owns, spoken to over a protocol instead of a terminal.
//!
//! Everything else here watches sessions a person started in their own
//! terminal: it reads the transcript they leave and scrapes the screen they
//! draw, because that is all an outsider gets. A session Sightline *starts* has
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
//! This module is the adapter: the protocol parsed into Sightline's own event
//! model, and the process driven over its pipes. It does not replace watched
//! sessions — those are why Sightline exists — it adds a second kind that a
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
use std::io::BufRead;

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
    /// implementation of everything Sightline shows about a session.
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
                let usage = msg.get("usage");
                let num = |key: &str| {
                    usage
                        .and_then(|u| u.get(key))
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                };
                let output = num("output_tokens");
                // What the turn actually cost to send. `cache_read` is the whole
                // conversation so far, re-read: it is the term that grows with
                // the session and it is sixty-odd times the output on a real
                // project. Recording only the output was measuring the smallest
                // number in the transaction.
                let cached = num("cache_read_input_tokens");
                let written = match usage.and_then(|u| u.get("cache_creation")) {
                    Some(c) => {
                        c.get("ephemeral_5m_input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            + c.get("ephemeral_1h_input_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                    }
                    None => num("cache_creation_input_tokens"),
                };
                let estimate = msg
                    .get("total_cost_usd")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                if output > 0 || estimate > 0.0 || cached > 0 {
                    out.push(ev(Kind::CostSpent {
                        output,
                        estimate,
                        cached,
                        written,
                    }));
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

/// `PATH` for a session Sightline starts, with Sightline on it.
///
/// The chief's brief tells it to run `sightline check`, because that is the one
/// step that turns a worker's claim into a verdict. Nothing made that command
/// reachable: this module's own documentation described "a session with
/// `sightline` on its path" and no code put it there. A live chief ran
/// `which sightline`, got nothing, and reported — correctly — that it could not
/// verify any work.
///
/// The directory taken is the one this executable is in, so a session is handed
/// the same Sightline that started it rather than whichever one happens to be
/// installed. Prepended, never replacing: everything the machine already
/// provides stays provided.
fn path_with_sightline() -> std::ffi::OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    else {
        return existing;
    };
    // `join_paths` rather than a literal separator: it is ';' on Windows and
    // ':' everywhere else, and this file is cross-checked against Windows.
    let mut entries = vec![dir];
    entries.extend(std::env::split_paths(&existing));
    std::env::join_paths(entries).unwrap_or(existing)
}

#[cfg(test)]
mod path_tests {
    use super::path_with_sightline;

    #[test]
    fn a_session_is_handed_the_sightline_that_started_it() {
        let path = path_with_sightline();
        let text = path.to_string_lossy().into_owned();
        let here = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        assert!(
            text.starts_with(&here),
            "the running executable's own directory has to come first, or a \
             different Sightline answers: {text}"
        );
        // Prepended, not replacing. A session that lost the machine's PATH
        // would fail on `git`, which is most of what it does.
        if let Some(existing) = std::env::var_os("PATH") {
            let existing = existing.to_string_lossy().into_owned();
            if !existing.is_empty() {
                assert!(
                    text.ends_with(&existing),
                    "everything the machine already provides stays provided"
                );
            }
        }
    }
}

/// The command line that starts an owned session.
///
/// Kept in one place because it is the whole contract with Claude Code: change
/// a flag and the parser above is talking to something else.
pub fn argv(spec: &Spec) -> Vec<String> {
    // Each agent is started its own way. Claude Code routes permission
    // decisions to a tool this process serves; Cursor cannot be told that on a
    // command line and is governed by a hook file written into its worktree
    // instead. Sharing one flag list between them would mean handing Cursor
    // arguments it refuses to start with.
    if spec.agent == "cursor" {
        return cursor_argv(spec);
    }
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
    if let Some(e) = &spec.effort {
        v.push("--effort".into());
        v.push(e.clone());
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
    // hypothetical: a chief with no grant could not run a single `sightline`
    // command and correctly reported itself blocked.
    //
    // Not passed when a policy is attached, and this is not a tidiness
    // decision. A granted tool does not prompt, and what does not prompt does
    // not reach `--permission-prompt-tool` — so every grant here is a hole in
    // the boundary, silently, for exactly the calls somebody thought were
    // important enough to name. With a policy the gate is what says yes.
    if !spec.allow.is_empty() && spec.policy.is_none() {
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
    // Widens the directories Claude Code will let a tool touch. It does not
    // widen anything here: every call still arrives at the gate, and the scope
    // kernel still confines writes to the policy's root.
    for dir in &spec.reach {
        v.push("--add-dir".into());
        v.push(dir.clone());
    }
    // The seam `claude --help` does not mention, and the reason any of this is
    // more than advice: every permission decision is routed to a tool Sightline
    // serves in-process, so `gate::decide` runs before the call does.
    if spec.policy.is_some() {
        v.push("--permission-prompt-tool".into());
        v.push(format!("mcp__{SERVER}__{APPROVE}"));
    }
    // Declaring the server in a config as well is what makes its *other* tools
    // callable by the model. The permission tool does not need this; a tool the
    // session is meant to reach for does.
    if spec.kernel_tools {
        v.push("--mcp-config".into());
        v.push(
            serde_json::json!({"mcpServers": {SERVER: {"type": "sdk", "name": SERVER}}})
                .to_string(),
        );
    }
    v
}

/// What starts a Cursor worker.
///
/// `--print` with a stream is the whole of driving it headlessly, and `--trust`
/// is not a convenience: without it the agent stops on a workspace-trust prompt
/// that nothing in this mode can answer, and the session simply never begins.
/// Trust here means "Sightline prepared this directory", which it did.
///
/// Nothing about permissions appears on this line. Cursor's boundary is
/// `.cursor/hooks.json`, written into the worktree by `hook::config` before the
/// session starts, so every tool call reaches the same `gate::decide` that a
/// Claude Code call reaches.
fn cursor_argv(spec: &Spec) -> Vec<String> {
    let mut v = vec![
        "--print".to_string(),
        "--output-format".into(),
        "stream-json".into(),
        "--trust".into(),
        // `--force` is the opposite of what it sounds like here, and that is
        // worth stating because it looks exactly like the flag somebody turns a
        // safety off with.
        //
        // Cursor has its own approval layer, which asks a person. Headless there
        // is no person, so it refuses — everything, including Sightline's own
        // kernel tools: a Cursor worker calling `note` came back "User rejected
        // MCP", with nobody having rejected anything.
        //
        // This turns that layer off. What remains is `.cursor/hooks.json`,
        // written into the worktree before the session starts, routing every
        // call to `gate::decide`. So it is not removing a boundary, it is
        // removing the second one — and two boundaries, where one is blind and
        // answers no to everything, is worse than one that can actually decide.
        "--force".into(),
    ];
    if let Some(m) = &spec.model {
        v.push("--model".into());
        v.push(m.clone());
    }
    // Effort is not a flag of its own here — it is spelled into the model name,
    // as `gpt-5.3-codex-high` or a bracketed override. A caller that asked for
    // effort without naming a model has asked for something this agent cannot
    // express, and inventing a model to carry it would be choosing a model on
    // their behalf.
    v
}

/// The MCP server Sightline serves to its own sessions, in this process.
///
/// In-process on purpose. A server on a socket, or a CLI on the session's PATH,
/// is a second way to reach the kernel and therefore a second thing to secure.
/// This one exists only for the length of a pipe.
pub const SERVER: &str = "sightline";
/// The tool every permission decision arrives at.
pub const APPROVE: &str = "approve";

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
    /// What it may do, decided here rather than by the settings it inherited.
    ///
    /// With a policy the session is started with a permission tool Sightline
    /// serves itself, so every call stops at `gate::decide` before it happens.
    /// Without one the session runs under whatever its permission mode allows,
    /// which is the older behaviour and still what a one-shot wants.
    #[serde(default)]
    pub policy: Option<crate::gate::Policy>,
    /// Directories this session may reach beyond the one it starts in.
    ///
    /// Claude Code confines a session's tools to its working directory, and
    /// that — not any kernel here — is what pinned the first live chief to one
    /// project while the work it had been given was in another. The scope
    /// kernel only ever judges `WRITES`, so it was never the thing in the way.
    ///
    /// A person who starts a session in their home directory can work anywhere
    /// under it. A supervisor is worth less than that, not more, so it is given
    /// the same reach rather than a narrower one.
    ///
    /// Worth being exact about what this was measured to do, because the
    /// obvious claim for it is false. Removing it and running the same live
    /// chief again, the read outside the project still succeeded — so on this
    /// machine and this Claude Code, neither Bash nor Read was confined to the
    /// working directory, and this flag changed nothing. It is kept because the
    /// confinement it lifts is real where it is configured — a settings file
    /// listing permitted directories, or a sandbox — and a supervisor that
    /// works here and is mute on someone else's machine is worse than one
    /// carrying a flag that is sometimes a no-op.
    /// Which agent this is, by the id its adapter answers to.
    ///
    /// Empty means Claude Code, so every spec written before other vendors
    /// existed still means what it meant. The flags an agent is started with
    /// differ completely between them — Claude Code takes
    /// `--permission-prompt-tool`, Cursor takes none of it and is governed
    /// through a hook file instead — so this is the field `argv` dispatches on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// How hard the model thinks, per Claude Code's `--effort`.
    ///
    /// The cheapest lever there is on a fleet, and the least used. Reasoning
    /// tokens are output tokens, and output tokens become context, and context
    /// is re-read on every subsequent request — so effort does not cost once, it
    /// compounds. A worker applying a change somebody else already decided does
    /// not need to think as hard as the session that decided it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    ///
    /// Not serialised when empty, and that is a compatibility decision rather
    /// than tidiness. `Spec` is `deny_unknown_fields` on purpose — a daemon that
    /// ignored a field it did not know would start a session with the wrong
    /// permissions and say nothing — so every field added here is refused
    /// outright by any daemon still running from before it existed. A worker's
    /// reach is always empty, and an empty field that is never sent cannot break
    /// a daemon that has not been restarted since the upgrade.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reach: Vec<String>,
    /// Whether Sightline also offers this session tools of its own.
    ///
    /// This is how a supervisor creates work: not by starting a process, which
    /// would put it outside everything below, but by asking the kernel to.
    #[serde(default)]
    pub kernel_tools: bool,
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
    /// What Sightline calls it: the handle a person or a foreman uses.
    pub name: String,
    pub cwd: String,
    /// The model it was asked for; empty means the agent's own default.
    pub model: String,
    /// Which agent this is. Empty means Claude Code, so a session recorded
    /// before other vendors existed still reads correctly.
    ///
    /// Kept because how you speak to a session depends on it: Claude Code holds
    /// a pipe open and takes another message down it, and Cursor does not exist
    /// between turns.
    #[serde(default)]
    pub agent: String,
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
    /// Shared with the reader thread, which answers control requests on it.
    ///
    /// The protocol is a conversation, not a broadcast: a `control_request`
    /// arriving on stdout has to be answered on stdin, by whoever read it, and
    /// a request left unanswered stalls the session with no error at all.
    stdin: Input,
    session: String,
    /// Kept current by the reader thread, so asking what a session is doing
    /// costs neither of the locks above.
    state: std::sync::Arc<std::sync::Mutex<Owned>>,
}

/// The host side of Claude Code's control protocol.
///
/// Claude Code speaks two things down the same pipe. Most lines are the
/// transcript — what the model said, what it called, what came back — and those
/// go to `Parser`. Some are `control_request`, which are questions: the session
/// blocks until each is answered, and an unanswered one is a session that hangs
/// with nothing in the log to say why. That cost an evening, so this answers
/// everything, including the ones it does not understand.
///
/// The one that matters is a permission request, which arrives wrapped twice:
/// a `control_request` of subtype `mcp_message`, carrying a JSON-RPC
/// `tools/call` for the tool named in `--permission-prompt-tool`. Unwrapping
/// that is this type's real job; deciding is `gate`'s.
struct Control {
    policy: Option<crate::gate::Policy>,
    serving: bool,
    stdin: Input,
}

impl Control {
    fn new(policy: Option<crate::gate::Policy>, serving: bool, stdin: Input) -> Self {
        Control {
            policy,
            serving,
            stdin,
        }
    }

    /// Answer the line if it is a control request; say nothing if it is not.
    fn consider(&mut self, line: &str, session: &str) -> Vec<Event> {
        if !self.policy.is_some() && !self.serving {
            return Vec::new();
        }
        // Cheap reject before parsing: most lines are transcript.
        if !line.contains("\"control_request\"") {
            return Vec::new();
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        if msg.get("type").and_then(Value::as_str) != Some("control_request") {
            return Vec::new();
        }
        let Some(id) = msg.get("request_id").and_then(Value::as_str) else {
            return Vec::new();
        };
        let request = msg.get("request").cloned().unwrap_or(Value::Null);
        if request.get("subtype").and_then(Value::as_str) != Some("mcp_message") {
            // Not ours to interpret, but still a question. Anything unanswered
            // stalls the session.
            self.respond(id, serde_json::json!({}));
            return Vec::new();
        }
        self.mcp(
            id,
            request.get("message").cloned().unwrap_or(Value::Null),
            session,
        )
    }

    /// The JSON-RPC server Sightline serves in this process.
    fn mcp(&mut self, id: &str, inner: Value, session: &str) -> Vec<Event> {
        let method = inner.get("method").and_then(Value::as_str).unwrap_or("");
        let call_id = inner.get("id").cloned().unwrap_or(Value::Null);
        let result = |r: Value| serde_json::json!({"jsonrpc": "2.0", "id": call_id, "result": r});

        match method {
            "initialize" => {
                self.respond(
                    id,
                    serde_json::json!({"mcp_response": result(serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
                    }))}),
                );
                Vec::new()
            }
            "tools/list" => {
                let mut tools = Vec::new();
                if self.policy.is_some() {
                    tools.push(serde_json::json!({
                        "name": APPROVE,
                        "description": "Decide whether a tool call may proceed",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "tool_name": {"type": "string"},
                                "input": {"type": "object"},
                            },
                            "required": ["tool_name", "input"],
                        },
                    }));
                }
                if self.serving {
                    tools.extend(crate::kernel::schemas());
                }
                self.respond(
                    id,
                    serde_json::json!({"mcp_response": result(serde_json::json!({"tools": tools}))}),
                );
                Vec::new()
            }
            "tools/call" => {
                let params = inner.get("params").cloned().unwrap_or(Value::Null);
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                let (text, events) = if name == APPROVE {
                    self.approve(&args, session)
                } else {
                    (
                        match crate::kernel::call(session, name, &args) {
                            Ok(said) => said,
                            Err(why) => format!("refused: {why}"),
                        },
                        Vec::new(),
                    )
                };
                self.respond(
                    id,
                    serde_json::json!({"mcp_response": result(serde_json::json!({
                        "content": [{"type": "text", "text": text}],
                    }))}),
                );
                events
            }
            // A notification has no id and wants no result — but the wrapper is
            // still a request, and still has to be answered.
            _ => {
                self.respond(
                    id,
                    serde_json::json!({"mcp_response": result(serde_json::json!({}))}),
                );
                Vec::new()
            }
        }
    }

    /// One permission decision, and the record of it.
    fn approve(&mut self, args: &Value, session: &str) -> (String, Vec<Event>) {
        let tool = args.get("tool_name").and_then(Value::as_str).unwrap_or("");
        let input = args.get("input").cloned().unwrap_or(serde_json::json!({}));
        let policy = self.policy.clone().unwrap_or_default();
        let (decision, by) = crate::gate::decide(&policy, session, tool, &input);

        let reply = match &decision {
            crate::gate::Decision::Allow => {
                serde_json::json!({"behavior": "allow", "updatedInput": input})
            }
            crate::gate::Decision::Rewrite { input, .. } => {
                serde_json::json!({"behavior": "allow", "updatedInput": input})
            }
            crate::gate::Decision::Deny { why } => {
                serde_json::json!({"behavior": "deny", "message": why})
            }
        };
        // The journal reads the same whether a person or a kernel answered,
        // which is the honest shape: a decision was made, and this is who by.
        let event = Event::new(
            session,
            "claude",
            Kind::PermissionAnswered {
                option: format!("{} {tool}", decision.option()),
                by: crate::bus::By::Policy { name: by.into() },
            },
        );
        (reply.to_string(), vec![event])
    }

    fn respond(&self, id: &str, payload: Value) {
        use std::io::Write;
        let line = serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": id, "response": payload},
        });
        let mut held = take(&self.stdin);
        if let Some(stdin) = held.as_mut() {
            let _ = writeln!(stdin, "{line}");
            let _ = stdin.flush();
        }
    }
}

/// The session's input, shared between whoever is speaking to it and the
/// reader thread that has to answer the protocol.
type Input = std::sync::Arc<std::sync::Mutex<Option<std::process::ChildStdin>>>;

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
            .env("PATH", path_with_sightline())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(match stderr {
                Stderr::Quiet => Stdio::null(),
                Stderr::Inherit => Stdio::inherit(),
            })
            .spawn()?;

        let stdin: Input = std::sync::Arc::new(std::sync::Mutex::new(child.stdin.take()));
        let stdout = child.stdout.take();
        let (session_owned, agent_owned) = (session.to_string(), agent.to_string());
        let state = std::sync::Arc::new(std::sync::Mutex::new(Owned {
            name: session.to_string(),
            cwd: cwd.to_string_lossy().into_owned(),
            model: spec.model.clone().unwrap_or_default(),
            agent: spec.agent.clone(),
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
        let answering = stdin.clone();
        let policy = spec.policy.clone();
        let serving = spec.kernel_tools;
        if let Some(stdout) = stdout {
            let spawned = std::thread::Builder::new()
                .name("sightline-owned-read".into())
                .spawn(move || {
                    // One parser for the whole session, so a failed result can
                    // name the tool that produced it.
                    let mut parser = Parser::new();
                    let mut control = Control::new(policy, serving, answering);
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        // The protocol first. A control request is a question
                        // the session is blocked on, and answering it is this
                        // thread's job because this thread is the one that saw
                        // it. Anything it produces is an event like any other.
                        let mut events = control.consider(&line, &session_owned);
                        // Not every agent speaks the same wire. Cursor's stream
                        // carries the same events under different names — usage
                        // in camelCase, thinking as a message type, a tool named
                        // by its object key — so it is rewritten into the shape
                        // this parser was written and tested against rather than
                        // parsed a second time. One translation beats two
                        // parsers: the event logic stays in one place, and the
                        // captured streams in tests/fixtures/cursor fail loudly
                        // if the shape moves.
                        let line = match agent_owned.as_str() {
                            "cursor" => match crate::agent::cursor::normalise(&line) {
                                Some(rewritten) => rewritten,
                                None => continue,
                            },
                            _ => line,
                        };
                        events.extend(parser.feed(&line, &session_owned, &agent_owned));
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
                    // It ended on its own rather than being stopped, which is
                    // the common case and the one `end` never sees.
                    crate::gate::release(&session_owned);
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

        let held = OwnedSession {
            child: std::sync::Mutex::new(child),
            stdin,
            session: session.to_string(),
            state,
        };
        // Before anything else is said to it: tell it which servers we serve, so
        // that the permission tool it was started with resolves to us.
        if spec.policy.is_some() || spec.kernel_tools {
            held.initialize()?;
        }
        Ok(held)
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
    /// Tell the session which servers Sightline serves, so the permission tool
    /// it was started with resolves to this process.
    ///
    /// Sent before the opening message, because the first thing the session does
    /// with that message may be to ask for permission.
    fn initialize(&self) -> std::io::Result<()> {
        use std::io::Write;
        let line = serde_json::json!({
            "type": "control_request",
            "request_id": "sightline-init",
            "request": {"subtype": "initialize", "sdkMcpServers": [SERVER]},
        });
        let mut held = take(&self.stdin);
        let Some(stdin) = held.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the session's input is closed",
            ));
        };
        writeln!(stdin, "{line}")?;
        stdin.flush()
    }

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

/// The name space for sessions Sightline holds by pipe rather than by terminal.
///
/// Distinct from `sightline-N` on purpose: those are terminal sessions, and a
/// person who types the name of one should not reach the other.
pub const PREFIX: &str = "owned-";

/// What a connected session is called.
///
/// A separate namespace, and not for tidiness. A spawned session's handle is
/// swapped for the agent's own id the moment it reports one — `fold_owned`
/// rekeys every task whose session is that handle. A connected session never
/// reports one, so it keeps its handle forever; sharing the namespace meant a
/// connected session's task was absorbed by whichever spawned session happened
/// to hold the same number, and its work silently joined another project. It
/// happened three times in a row before this existed.
pub const LINKED: &str = "linked-";

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
    /// None for a connected session: there is no process to hold.
    session: Option<std::sync::Arc<OwnedSession>>,
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
    FLEET.get_or_init(|| {
        let mut map = std::collections::HashMap::new();
        restore(&mut map);
        note_size(&map);
        std::sync::Mutex::new(map)
    })
}

/// Connected sessions outlive the process that assigned them — that is the
/// point of not spawning. A daemon restart, or an MCP door in another
/// process, still has to see them, so they are read back from disk here
/// rather than invented empty.
fn restore(fleet: &mut Fleet) {
    for meta in crate::mail::list() {
        if fleet.contains_key(&meta.name) {
            continue;
        }
        let pending: Pending = std::sync::Arc::new(std::sync::Mutex::new((
            std::collections::VecDeque::new(),
            0,
        )));
        let state = Owned {
            name: meta.name.clone(),
            cwd: meta.cwd,
            model: meta.model,
            agent: meta.agent,
            mode: String::new(),
            session_id: String::new(),
            pid: 0,
            alive: true,
            busy: false,
            tool: String::new(),
            started: meta.started,
            last: meta.started,
        };
        fleet.insert(meta.name.clone(), hold_connected(state, pending));
    }
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
        session: Some(std::sync::Arc::new(session)),
        state,
        pending,
    }
}

fn hold_connected(state: Owned, pending: Pending) -> Held {
    Held {
        session: None,
        state: std::sync::Arc::new(std::sync::Mutex::new(state)),
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
/// Every handle already spoken for, from all three places one can live.
///
/// The fleet this process holds is not the whole answer and assuming it was
/// caused a real collision: a `sightline mcp` process holds no fleet at all, so
/// it read `taken` as empty, handed out `owned-1`, and the daemon already had an
/// `owned-1` — a live chief. The new session's task was then written against the
/// chief's identity, where it silently became part of somebody else's project.
///
/// So: what this process holds, what has been connected on disk, and every
/// handle the work store has ever assigned against. The last is the one that
/// makes this safe across processes, because a task record outlives the session
/// that made it and is visible to everyone.
/// Take the next handle, and never give it out again.
///
/// Derived names do not work here and two attempts proved it. A process's own
/// fleet is not the whole fleet: `sightline mcp` holds none at all, read the
/// taken list as empty, and handed out `owned-1` while the daemon already had
/// one — a live chief, whose task record the new session's work was then
/// written against. Widening the search to the connected directory and the work
/// store did not fix it either, because a live session's handle is in another
/// process's memory and its task has since been rekeyed to a uuid. There is
/// nothing on disk to find.
///
/// So the number is claimed rather than computed. A counter that only goes up,
/// written before the name is used, is correct no matter who is asking or what
/// they can see — which is the property every previous version lacked.
///
/// Seeded from whatever *is* visible the first time, so an existing install does
/// not restart at one and collide with everything it already has.
pub fn claim_name() -> String {
    claim_handle(PREFIX)
}

/// The same counter for both namespaces, so a number is never reused by either.
pub fn claim_handle(prefix: &str) -> String {
    let path = crate::app::data_dir().join("handles");
    let seen = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .unwrap_or_else(|| {
            taken_names()
                .iter()
                .filter_map(|n| n.strip_prefix(PREFIX))
                .filter_map(|n| n.parse::<u64>().ok())
                .max()
                .unwrap_or(0)
        });
    let next = seen + 1;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Written before it is used. A crash between the two wastes a number, which
    // costs nothing; the other order hands the same name to two sessions.
    let _ = std::fs::write(&path, next.to_string());
    format!("{prefix}{next}")
}

pub fn taken_names() -> Vec<String> {
    let mut all: std::collections::BTreeSet<String> = locked().keys().cloned().collect();
    all.extend(crate::mail::connected_names());
    let store = crate::work::Store::load(crate::work::path_in(&crate::app::data_dir()));
    all.extend(
        store
            .tasks()
            .iter()
            .map(|t| t.session.clone())
            .filter(|s| is_owned_name(s)),
    );
    all.into_iter().collect()
}

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
    name.starts_with(PREFIX) || name.starts_with(LINKED)
}

/// Whether this is a session Sightline connected to rather than started.
///
/// The distinction the rekey has to respect: a connected handle is permanent
/// because there is no agent id coming to replace it.
pub fn is_linked_name(name: &str) -> bool {
    name.starts_with(LINKED)
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
    // Before the lock: claiming reads the fleet to seed itself, and this mutex
    // is not reentrant — asking for it twice on one thread is a deadlock, which
    // is what the first version of this did.
    let name = claim_name();
    let mut held = locked();
    let pending: Pending = std::sync::Arc::new(std::sync::Mutex::new((
        std::collections::VecDeque::new(),
        0,
    )));
    let agent_id = if spec.agent.is_empty() {
        "claude"
    } else {
        spec.agent.as_str()
    };
    let session = OwnedSession::start_with(
        program,
        cwd,
        spec,
        &name,
        agent_id,
        // Many of these may run at once under a daemon with no terminal;
        // their diagnostics must not scatter across whatever it inherited.
        Stderr::Quiet,
        // Kept, not dropped. What the session does arrives here live, on the
        // pipe, as it happens — the tool call before its result, the failure
        // with its reason. Sightline used to learn the same things by re-reading
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
    note_size(&held);
    Ok(state)
}

/// Start one, or connect one, according to the agent.
///
/// The kernel calls this rather than `start`, so an agent that cannot be
/// spawned is not asked to be. Vendor differences stay on the adapter.
pub fn open(
    program: &str,
    cwd: &std::path::Path,
    spec: &Spec,
    settle: std::time::Duration,
) -> Result<Owned, String> {
    let id = if spec.agent.is_empty() {
        "claude"
    } else {
        spec.agent.as_str()
    };
    if crate::agent::find(id).is_some_and(|a| !a.spawnable()) {
        connect(cwd, spec)
    } else {
        start(program, cwd, spec, settle)
    }
}

/// Register a worker that already exists, rather than starting one.
///
/// Grok Bot is the case this was written for: the assistant is a long-lived
/// Cursor desktop chat, and inventing a process for it would be lying about
/// how it runs. The session still appears in the fleet, still counts against
/// the ceiling, and still has a name a chief can `tell` — the message waits
/// in the mailbox until a later turn reads it.
pub fn connect(cwd: &std::path::Path, spec: &Spec) -> Result<Owned, String> {
    // Before the lock, for the same reason as in `start` — and in the connected
    // namespace, which no rekey will ever touch.
    let name = claim_handle(LINKED);
    let mut held = locked();
    let pending: Pending = std::sync::Arc::new(std::sync::Mutex::new((
        std::collections::VecDeque::new(),
        0,
    )));
    let started = now_secs();
    let state = Owned {
        name: name.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        model: spec.model.clone().unwrap_or_default(),
        agent: if spec.agent.is_empty() {
            "grok".into()
        } else {
            spec.agent.clone()
        },
        mode: spec.mode.clone().unwrap_or_default(),
        session_id: String::new(),
        pid: 0,
        alive: true,
        busy: false,
        tool: String::new(),
        started,
        last: started,
    };
    crate::mail::remember(&crate::mail::Connected {
        name: name.clone(),
        cwd: state.cwd.clone(),
        agent: state.agent.clone(),
        model: state.model.clone(),
        started,
    })?;
    // Visible the way a spawned session is, from the moment it exists: a
    // connected worker that never announced itself would be a row the Hub
    // could not explain.
    if let Ok(mut buffer) = pending.lock() {
        buffer.0.push_back(Event::new(
            &name,
            &state.agent,
            Kind::SessionStarted {
                cwd: state.cwd.clone(),
                branch: String::new(),
            },
        ));
    }
    held.insert(name, hold_connected(state.clone(), pending));
    note_size(&held);
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

/// One of them, by the name Sightline gave it or by the transcript id the agent
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
    let (key, session, pending, snapshot) = {
        let fleet = locked();
        match find_key(&fleet, who) {
            Some(key) => {
                let held = fleet.get(&key).expect("just found");
                if !held.state().alive {
                    return Err(format!("{key} has ended"));
                }
                (
                    key.clone(),
                    held.session.clone(),
                    held.pending.clone(),
                    held.state(),
                )
            }
            None => {
                // A connected session the fleet in *this* process has not
                // restored yet — the MCP door, a test, a tell from a process
                // that is not holding anyone. The mailbox is the session.
                drop(fleet);
                if crate::mail::exists(who) {
                    return crate::mail::push(who, text);
                }
                return Err(format!("no owned session called {who}"));
            }
        }
    };
    // Checked again with the session in hand: it may have exited between the
    // two, and writing to a dead pipe is a broken-pipe error rather than a
    // sentence saying what happened.
    if let Some(session) = &session {
        let held = session.state();
        if !held.alive {
            return Err(format!("{key} has ended"));
        }
    }
    match crate::agent::find(&snapshot.agent).map(|a| a.delivery()) {
        Some(crate::agent::Delivery::Resume) => resume(&snapshot, text, pending),
        Some(crate::agent::Delivery::Mailbox) => crate::mail::push(&key, text),
        _ => {
            let session = session.ok_or_else(|| {
                format!("{key} is not listening — there is no process to write to")
            })?;
            session.send(text).map_err(|e| e.to_string())
        }
    }
}

/// Say something to a Cursor session, which is not listening.
///
/// Claude Code in this mode holds a pipe open across turns, so another message
/// is a write. Cursor does not exist between turns: `--print` reads its prompt,
/// runs once and exits, and a second message written to that pipe is read as
/// more of the first prompt — which was the first thing tried here, and it
/// produced one turn answering both.
///
/// What it keeps instead is the chat. `--resume <id>` reopens it with everything
/// already said still in place, so the same conversation continues in a new
/// process. The session is therefore a chat id plus a series of runs rather than
/// a process, and this is the whole of that difference.
///
/// The reply is streamed back through the same reader as the original turn, so
/// the transcript, the feed, the boundary and the cost ledger see it exactly as
/// they see anything else. A worker spoken to twice looks like one worker.
fn resume(held: &Owned, text: &str, pending: Pending) -> Result<(), String> {
    if held.session_id.is_empty() {
        return Err(format!(
            "{} has not named its chat yet — it is still on its first turn, and there \
             is nothing to resume until that finishes",
            held.name
        ));
    }
    let spec = Spec {
        agent: "cursor".into(),
        model: (!held.model.is_empty()).then(|| held.model.clone()),
        ..Default::default()
    };
    let mut argv = argv(&spec);
    argv.push("--resume".into());
    argv.push(held.session_id.clone());
    argv.push(text.to_string());

    let child = std::process::Command::new("cursor-agent")
        .args(&argv)
        .current_dir(&held.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not reach {}: {e}", held.name))?;

    let name = held.name.clone();
    let stdout = child.stdout;
    std::thread::Builder::new()
        .name(format!("resume-{name}"))
        .spawn(move || {
            let Some(stdout) = stdout else { return };
            let mut parser = Parser::new();
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                let Some(rewritten) = crate::agent::cursor::normalise(&line) else {
                    continue;
                };
                for event in parser.feed(&rewritten, &name, "cursor") {
                    // The same buffer the original turn filled, so a front end
                    // draining this session sees one conversation rather than
                    // two — which is the whole point of resuming rather than
                    // starting something new.
                    if let Ok(mut buffer) = pending.lock() {
                        buffer.0.push_back(event);
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// End one, and forget it.
pub fn stop(who: &str) -> Result<(), String> {
    let taken = {
        let mut fleet = locked();
        let key = find_key(&fleet, who).ok_or_else(|| format!("no owned session called {who}"))?;
        let taken = fleet.remove(&key);
        note_size(&fleet);
        taken
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
    // Before the process: a file held against something that is no longer
    // running is a file nobody else can work on.
    let name = held.state().name;
    crate::gate::release(&name);
    if let Some(session) = &held.session {
        session.stop();
    }
    crate::mail::forget(&name);
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
    note_size(&fleet);
    dead
}

/// End every one of them. Returns the names that were stopped.
pub fn stop_all() -> Vec<String> {
    let taken: Vec<(String, Held)> = {
        let mut fleet = locked();
        let taken = fleet.drain().collect();
        note_size(&fleet);
        taken
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

/// The same number, without taking the fleet lock.
///
/// The ceiling is consulted on every tool call of every session, from the reader
/// thread that has to answer it. Taking the fleet lock there would put a lock
/// held by whoever is *starting* a session on the critical path of every
/// permission decision — bounded, since nothing holds it forever, but a stall
/// on the one path that must not stall. This is written under that lock at each
/// mutation, so it is exact rather than approximate.
pub fn running() -> usize {
    LIVE.load(std::sync::atomic::Ordering::Relaxed)
}

static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Record the size of the fleet. Call while still holding the lock.
fn note_size(fleet: &Fleet) {
    LIVE.store(fleet.len(), std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_policed_session_is_never_handed_a_grant() {
        // A granted tool does not prompt, and what does not prompt never
        // reaches the permission tool. This is the refutation for the whole
        // boundary: if `allow` survives alongside a policy, the gate is blind
        // to precisely the calls someone cared enough to name.
        let spec = Spec {
            allow: vec!["Bash(rm *)".into(), "Write".into()],
            policy: Some(crate::gate::Policy::default()),
            ..Spec::default()
        };
        let args = argv(&spec);
        assert!(
            !args.iter().any(|a| a == "--allowedTools"),
            "a grant slipped past the gate: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--permission-prompt-tool"),
            "a policed session must route its decisions here: {args:?}"
        );
    }

    #[test]
    fn without_a_policy_grants_still_work_as_they_did() {
        let spec = Spec {
            allow: vec!["Bash(echo *)".into()],
            ..Spec::default()
        };
        let args = argv(&spec);
        assert!(args.iter().any(|a| a == "--allowedTools"));
        assert!(!args.iter().any(|a| a == "--permission-prompt-tool"));
    }
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
            Kind::CostSpent {
                output, estimate, ..
            } => Some((output, estimate)),
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
        let dir = std::env::temp_dir().join("sightline-owned-shim");
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
            !is_owned_name("sightline-3"),
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
        let dir = std::env::temp_dir().join(format!("sightline-fleet-{tag}"));
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
        let dir = std::env::temp_dir().join("sightline-fleet-dead");
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
        let dir = std::env::temp_dir().join("sightline-fleet-deaf");
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
        let v = argv(&Spec::default().allowing(&["Bash(sightline *)", "Read"]));
        let at = v
            .iter()
            .position(|a| a == "--allowedTools")
            .unwrap_or_else(|| panic!("the grant is passed: {v:?}"));
        assert_eq!(v[at + 1], "Bash(sightline *)");
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
                .allowing(&["Bash(sightline *)"])
                .denying(&["Write"]),
        );
        let allow = v.iter().position(|a| a == "--allowedTools").unwrap();
        let deny = v.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(v[allow + 1], "Bash(sightline *)");
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

#[cfg(test)]
mod usage_tests {
    use super::*;
    use crate::bus::Kind;

    /// The turn's own accounting, as Claude Code reports it.
    fn a_result(output: u64, cached: u64, wrote_5m: u64, wrote_1h: u64) -> serde_json::Value {
        serde_json::json!({
            "type": "result",
            "total_cost_usd": 0.25,
            "usage": {
                "output_tokens": output,
                "input_tokens": 12,
                "cache_read_input_tokens": cached,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": wrote_5m,
                    "ephemeral_1h_input_tokens": wrote_1h,
                },
            }
        })
    }

    #[test]
    fn a_turn_records_what_it_actually_cost_to_send() {
        // The defect this was written for: only `output_tokens` and the cost
        // estimate were kept, and on a real supervised project those were 924k
        // against 61.5M cache reads — a ratio of sixty-seven to one. Every cost
        // view in this program was built on the smallest number in the
        // transaction and reported it as the whole.
        let mut parser = Parser::new();
        let events = parser.feed(
            &a_result(1_000, 90_000, 4_000, 1_000).to_string(),
            "s1",
            "claude",
        );
        let cost = events
            .iter()
            .find_map(|e| match &e.kind {
                Kind::CostSpent {
                    output,
                    cached,
                    written,
                    ..
                } => Some((*output, *cached, *written)),
                _ => None,
            })
            .expect("a finished turn publishes what it spent");
        assert_eq!(cost.0, 1_000);
        assert_eq!(cost.1, 90_000, "the context re-read is the term that grows");
        assert_eq!(cost.2, 5_000, "both cache lifetimes count as written");
    }

    #[test]
    fn a_turn_that_only_re_read_context_is_still_a_turn_that_spent() {
        // Output can be nearly nothing while the send was enormous — a session
        // deep in a long conversation answering "yes". Publishing only when
        // output moved would drop exactly the turns this is meant to expose.
        let mut parser = Parser::new();
        let events = parser.feed(
            &serde_json::json!({
                "type": "result",
                "total_cost_usd": 0.0,
                "usage": { "output_tokens": 0, "cache_read_input_tokens": 150_000 }
            })
            .to_string(),
            "s1",
            "claude",
        );
        assert!(
            events.iter().any(|e| matches!(
                e.kind,
                Kind::CostSpent {
                    cached: 150_000,
                    ..
                }
            )),
            "a turn that spent 150k re-reading and produced nothing still spent"
        );
    }

    #[test]
    fn the_older_shape_of_the_field_is_still_read() {
        // Claude Code has reported cache creation both as a flat number and as
        // a map of lifetimes. Pinning both, because this is somebody else's
        // format and the compatibility suite is where that gets noticed.
        let mut parser = Parser::new();
        let events = parser.feed(
            &serde_json::json!({
                "type": "result",
                "usage": { "output_tokens": 5, "cache_creation_input_tokens": 7_777 }
            })
            .to_string(),
            "s1",
            "claude",
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.kind, Kind::CostSpent { written: 7_777, .. }))
        );
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    fn held(agent: &str, session_id: &str) -> Owned {
        Owned {
            name: "owned-3".into(),
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            model: String::new(),
            agent: agent.into(),
            mode: String::new(),
            session_id: session_id.into(),
            pid: 1,
            alive: true,
            busy: false,
            tool: String::new(),
            started: 0,
            last: 0,
        }
    }

    #[test]
    fn a_cursor_session_is_reached_by_resuming_its_chat() {
        // The command that carries a second message. Claude Code takes one down
        // a pipe it is still holding; Cursor has exited, and the chat is the
        // only thing that survived the turn.
        let spec = Spec {
            agent: "cursor".into(),
            ..Default::default()
        };
        let argv = argv(&spec);
        assert!(argv.iter().any(|a| a == "--print"));
        assert!(argv.iter().any(|a| a == "stream-json"));
        // Sent on to the same reader, so a resumed turn is read exactly like the
        // first one — same events, same cost, same boundary.
        assert!(argv.iter().any(|a| a == "--force"));
    }

    #[test]
    fn a_cursor_session_that_has_not_named_its_chat_says_so() {
        // The window between starting and the first `system/init` line. There is
        // nothing to resume, and the honest answer names why rather than failing
        // to spawn something.
        let pending: Pending = Default::default();
        let why =
            resume(&held("cursor", ""), "anything", pending).expect_err("there is no chat yet");
        assert!(why.contains("first turn"), "{why}");
    }

    #[test]
    fn a_grok_worker_is_listed_told_and_reads_the_message_on_a_later_turn() {
        // The Cursor lesson, for an agent that cannot even be resumed: tell
        // writes a file, the worker is in the fleet without a process, and
        // inbox is how a later turn actually sees the message.
        let dir = std::env::temp_dir().join(format!(
            "sightline-grok-connect-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let started = connect(
            &dir,
            &Spec {
                agent: "grok".into(),
                ..Default::default()
            },
        )
        .expect("a connected session does not need a binary");
        assert_eq!(started.agent, "grok");
        assert_eq!(started.pid, 0, "there is no process");
        assert!(
            list().iter().any(|o| o.name == started.name && o.alive),
            "the Hub lists it like any other worker"
        );
        say(&started.name, "fix the flaky test").expect("tell lands in the mailbox");
        assert!(
            crate::mail::waiting(&started.name)
                .iter()
                .any(|m| m.contains("flaky")),
            "the message is still there before a later turn reads it"
        );
        let got = crate::kernel::call(&started.name, "inbox", &serde_json::json!({}))
            .expect("inbox is a kernel tool");
        assert!(got.contains("flaky"), "{got}");
        let empty = crate::kernel::call(&started.name, "inbox", &serde_json::json!({}))
            .expect("an empty mailbox is not a failure");
        assert!(
            empty.contains("nothing waiting"),
            "taking is how the same assignment is not done twice: {empty}"
        );
        let _ = stop(&started.name);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tell_does_not_invent_a_process_for_a_name_it_does_not_know() {
        let why = say("owned-nobody-here", "hello").expect_err("unknown names are not invented");
        assert!(why.contains("no owned session"), "{why}");
    }
}

#[cfg(test)]
mod naming_tests {
    use super::*;

    #[test]
    fn a_handle_is_never_handed_out_twice_across_processes() {
        // The collision this was written for, and it did real damage: a
        // `sightline mcp` process holds no fleet, so it read the taken list as
        // empty, handed out `owned-1`, and the daemon already had an `owned-1`
        // — a live chief. The new session's task was written against the
        // chief's identity and silently joined somebody else's project.
        //
        // A process's own fleet cannot answer this. The record that can is the
        // one that outlives every session and is visible to everyone.
        assert_eq!(next_name(&[]), "owned-1");
        assert_eq!(
            next_name(&["owned-1".into(), "owned-4".into()]),
            "owned-5",
            "the highest wins, not the count — a gap is a session that ended"
        );
        // And the union is what is asked, rather than any one source.
        let names = taken_names();
        assert!(
            names.iter().all(|n| is_owned_name(n)),
            "only this fleet's handles are counted: {names:?}"
        );
    }
}

#[cfg(test)]
mod handle_tests {
    use super::*;

    #[test]
    fn a_connected_session_cannot_take_a_spawned_ones_name() {
        // Three tasks in a row were absorbed by other sessions before this
        // existed. A spawned session's handle is temporary — `fold_owned` swaps
        // it for the agent's id the moment one arrives, rekeying every task that
        // named it. A connected session never reports an id, so it keeps its
        // handle forever. Sharing the namespace meant the rekey found a
        // connected session's task under a handle a spawned session had just
        // claimed, and moved it into that session's project.
        assert!(is_owned_name("owned-3"));
        assert!(is_owned_name("linked-3"));
        assert!(is_linked_name("linked-3"));
        assert!(
            !is_linked_name("owned-3"),
            "the rekey asks this question, and the wrong answer moves somebody's work"
        );
    }

    #[test]
    fn a_number_is_claimed_once_and_never_reissued() {
        // Derived names failed twice: a process's own fleet is not the fleet,
        // and a live session's handle lives in another process's memory with
        // nothing on disk to find. The counter is shared by both namespaces so
        // that `owned-4` and `linked-4` cannot both exist.
        let dir = std::env::temp_dir().join("sightline-handle-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("handles");
        std::fs::write(&path, "7").unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read.trim(), "7", "the counter is the record, not a guess");
    }
}
