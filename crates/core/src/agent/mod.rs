//! What Sightline needs from a coding agent, and how each one answers.
//!
//! Sightline grew around Claude Code and learned its habits: a JSONL transcript per
//! conversation under `~/.claude/projects`, a registry of live sessions under
//! `~/.claude/sessions`, numbered permission prompts. None of that is shared by
//! other agents, and none of it is what makes Sightline work.
//!
//! What makes it work is lower down: a session is a program in a terminal, and
//! a terminal can be read and typed into. Everything above that — which
//! conversations exist, what was said in them, what a session is asking, how to
//! resume one, how it gets a name — is agent-specific, and this is where each
//! agent answers for itself.
//!
//! An agent with no adapter is still a session: started, watched on screen,
//! typed into, interrupted, closed. It simply has no insides to show, and says
//! so rather than showing empty panes.

use crate::control::Approval;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub mod aider;
pub mod claude;
pub mod cursor;
pub mod grok;

/// How a session gets a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naming {
    /// The agent renames itself when told: the name then lives wherever the
    /// agent keeps it, and everything downstream agrees.
    Command(&'static str),
    /// The agent has no idea of a name, so Sightline keeps one for it.
    Kept,
}

/// What to start an agent with. An agent is only given the options it has —
/// aiming Claude Code's `--effort` at Aider makes it refuse to start.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub mode: Option<&'a str>,
}

/// One conversation an agent has recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// what to hand back to the agent to resume it
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub modified: SystemTime,
    pub bytes: u64,
}

/// How a transcript is written, which decides how a line of it is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// One JSON object per line, Claude Code's shape
    ClaudeJsonl,
    /// Markdown, Aider's shape: `####` for what was asked, plain text for the
    /// answer, `>` for everything the tool said about it
    AiderMarkdown,
    /// Nothing written down; the screen is all there is
    None,
}

pub trait Adapter: Send + Sync {
    /// What you type to choose it: `--agent aider`.
    fn id(&self) -> &'static str;
    /// How it is written in the interface.
    fn label(&self) -> &'static str;
    /// The program to run.
    fn program(&self) -> &'static str;

    fn command(&self, options: Options) -> Vec<String>;

    /// How to pick a conversation up again, if it can be.
    fn resume(&self, id: &str) -> Option<Vec<String>>;

    /// Whether Sightline starts this by spawning `program()`.
    ///
    /// Most agents are a binary. Grok Bot is not: it is the Cursor desktop
    /// assistant already running, and a spawn would invent a CLI that does not
    /// exist. The kernel still assigns work to it; it connects rather than
    /// starts.
    fn spawnable(&self) -> bool {
        true
    }

    /// How a second message reaches a session of this agent.
    fn delivery(&self) -> Delivery {
        Delivery::Pipe
    }

    /// Whether it is here to be used, which is not always "is `program` on the
    /// PATH". An agent with no binary still has an honest installed/not answer.
    fn present(&self) -> bool {
        which(self.program())
    }

    fn naming(&self) -> Naming;

    /// How its transcript is written, if it writes one.
    fn record(&self) -> Record;

    /// Conversations it has recorded. `roots` are the folders Sightline has seen
    /// this agent working in — an agent that keeps its history beside the code
    /// has nowhere else to be found.
    fn conversations(&self, roots: &[PathBuf]) -> Vec<Found>;

    /// What it is asking, read off its screen, and what to type back. None
    /// means it is not waiting on a person.
    fn approval(&self, screen: &str) -> Option<Approval> {
        crate::control::pending_approval(screen)
    }

    fn keeps_transcripts(&self) -> bool {
        !matches!(self.record(), Record::None)
    }

    /// How somebody gets this on their machine, in one line they can run.
    ///
    /// Here rather than in a README because a README is not where a person is
    /// standing when they find out they need it. `None` means it comes with the
    /// system or has no single answer.
    fn install_hint(&self) -> Option<&'static str> {
        None
    }

    /// How they sign in, when signing in is a thing it needs.
    ///
    /// Always a command for the person to run rather than something Sightline
    /// runs for them: every one of these opens a browser and authenticates an
    /// account, which is not a decision a background process should be making.
    fn signin_hint(&self) -> Option<&'static str> {
        None
    }

    /// What to run to find out whether it is signed in, and what a signed-in
    /// answer contains. `None` means there is no cheap way to ask, and the
    /// honest report is then "cannot tell" rather than a guess in either
    /// direction.
    fn signin_probe(&self) -> Option<(&'static [&'static str], &'static str)> {
        None
    }

    /// How much of Sightline's boundary reaches work this agent does.
    ///
    /// Three states rather than two, and the middle one was found by being wrong
    /// about it. Cursor was recorded here as ungoverned on the strength of its
    /// `--help`, which mentions no permission hook. Its binary does: a
    /// `hooks.json` with `beforeShellExecution`, `beforeMCPExecution` and
    /// `beforeReadFile`, each of which takes `{"permission": "deny"}` and throws
    /// rather than running the call. What it has no *before* hook for is its own
    /// file edits — `afterFileEdit` fires once the write has happened.
    ///
    /// So "can it be governed" has no yes-or-no answer, and forcing one would
    /// have to round in a direction. Rounding up claims a boundary that is not
    /// there; rounding down throws away most of one that is.
    fn governance(&self) -> Governance {
        Governance::None
    }

    /// The same thing in a sentence, so a view does not have to know the rules.
    /// Override when the enum's sentence would describe a different agent's gap.
    fn governance_note(&self) -> &'static str {
        self.governance().describe()
    }

    /// Files this agent needs in the worktree before it is assigned, if any.
    ///
    /// Cursor's boundary is a hook file rather than a flag, and writing it
    /// after the session starts leaves the first calls ungoverned. Default is
    /// nothing: an agent whose boundary is on the command line has no file to
    /// place.
    fn prepare(&self, _root: &Path, _sightline: &Path) -> Result<(), String> {
        Ok(())
    }

    /// Bind this session's kernel door, once it has a name.
    ///
    /// The session's name is baked into the MCP config, because a server
    /// spawned by an agent has no other way to know which session it is
    /// serving — and a `claim` attributed to the wrong one marks somebody
    /// else's work finished.
    fn offer_kernel(&self, _root: &Path, _session: &str, _sightline: &Path) -> Result<(), String> {
        Ok(())
    }
}

/// How a second message reaches a session.
///
/// Claude Code in this mode holds a pipe open across turns, so another
/// message is a write. Cursor does not exist between turns: `--print` runs
/// once and exits, and the chat is what `--resume` reopens. Grok Bot is not
/// even that: there is no process and no resume flag, so the message waits
/// in a file a later turn reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The process is still listening.
    Pipe,
    /// The process has exited. `--resume <id>` reopens the chat.
    Resume,
    /// No process. Messages wait on disk for a later turn to pull them.
    Mailbox,
}

/// How much of the boundary reaches an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Governance {
    /// Every call stops at the gate before it happens.
    Full,
    /// Some calls do. The rest are seen afterwards, which catches a mistake and
    /// does not prevent one.
    Partial,
    /// Watched, driven and measured. Not policed.
    None,
}

impl Governance {
    /// Said in a line, because this is the difference between what a person
    /// thinks they have and what they have.
    pub fn describe(self) -> &'static str {
        match self {
            Governance::Full => "governed — every call stops at the boundary",
            Governance::Partial => {
                "partly governed — shell, MCP and reads stop at the boundary; its own \
                 file edits are seen only after they happen"
            }
            Governance::None => "not governed — watched and driven only",
        }
    }
}

/// One agent, and whether it is ready to be used.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Connection {
    pub id: String,
    pub label: String,
    pub program: String,
    pub installed: bool,
    /// The version it reports, when it is installed and says so.
    pub version: String,
    /// `None` where there is no cheap way to ask.
    pub signed_in: Option<bool>,
    pub governance: Governance,
    /// The same thing in a sentence, so a view does not have to know the rules.
    pub governance_note: String,
    pub install_hint: String,
    pub signin_hint: String,
}

/// Every agent Sightline knows, and where each one stands.
///
/// Probing runs a subprocess per agent, so this is a question to ask when
/// somebody opens the panel rather than on a timer.
pub fn connections(check_signin: bool) -> Vec<Connection> {
    all()
        .iter()
        .map(|a| {
            let found = a.present();
            let version = if found && which(a.program()) {
                run(&[a.program(), "--version"])
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(40)
                    .collect()
            } else {
                String::new()
            };
            let signed_in = match (found && check_signin, a.signin_probe()) {
                (true, Some((argv, needle))) => {
                    Some(run(argv).map(|out| out.contains(needle)).unwrap_or(false))
                }
                _ => None,
            };
            Connection {
                id: a.id().into(),
                label: a.label().into(),
                program: a.program().into(),
                installed: found,
                version,
                signed_in,
                governance: a.governance(),
                governance_note: a.governance_note().into(),
                install_hint: a.install_hint().unwrap_or_default().into(),
                signin_hint: a.signin_hint().unwrap_or_default().into(),
            }
        })
        .collect()
}

/// Whether a program is on the path. `which` itself is not depended on: a
/// machine that lacks it would report every agent missing.
pub(crate) fn which(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let full = dir.join(program);
        full.is_file() || full.is_symlink()
    })
}

/// Run something short and read what it said. Errors are absence, not panic:
/// a probe is a question, and "it would not answer" is an answer.
fn run(argv: &[&str]) -> Option<String> {
    let out = std::process::Command::new(argv.first()?)
        .args(&argv[1..])
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

/// Everything Sightline knows how to run.
pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude::ClaudeCode),
        Box::new(cursor::Cursor),
        Box::new(grok::GrokBot),
        Box::new(aider::Aider),
        Box::new(Plain::new("codex", "Codex", "codex")),
        Box::new(Plain::new("gemini", "Gemini", "gemini")),
    ]
}

/// The adapter for an id or a program name.
pub fn find(name: &str) -> Option<Box<dyn Adapter>> {
    all()
        .into_iter()
        .find(|a| a.id() == name || a.program() == name)
}

/// Which agent a running command line belongs to.
pub fn of_command(cmd: &str) -> Option<Box<dyn Adapter>> {
    let program = cmd.split_whitespace().next()?;
    let program = program.rsplit(['/', '\\']).next()?;
    let program = program.strip_suffix(".exe").unwrap_or(program);
    find(program)
}

/// Whether a pane is running something Sightline should treat as a session.
pub fn is_agent(cmd: &str) -> bool {
    of_command(cmd).is_some()
}

/// An agent Sightline can run and watch on screen, but whose records it cannot
/// read — either because it keeps none, or because nobody has written the
/// adapter yet. Everything terminal-shaped still works.
pub struct Plain {
    id: &'static str,
    label: &'static str,
    program: &'static str,
}

impl Plain {
    pub const fn new(id: &'static str, label: &'static str, program: &'static str) -> Self {
        Plain { id, label, program }
    }
}

impl Adapter for Plain {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        self.label
    }
    fn program(&self) -> &'static str {
        self.program
    }
    fn command(&self, options: Options) -> Vec<String> {
        let mut argv = vec![self.program.to_string()];
        if let Some(m) = options.model {
            argv.push("--model".into());
            argv.push(m.into());
        }
        argv
    }
    fn resume(&self, _id: &str) -> Option<Vec<String>> {
        None
    }
    fn naming(&self) -> Naming {
        Naming::Kept
    }
    fn record(&self) -> Record {
        Record::None
    }
    fn conversations(&self, _roots: &[PathBuf]) -> Vec<Found> {
        Vec::new()
    }
}

/// A command run exactly as typed, for anything local with no entry at all.
pub fn custom_command(program: &str) -> Vec<String> {
    program.split_whitespace().map(str::to_string).collect()
}

/// Whether a folder is worth asking an agent about — somewhere Sightline has seen
/// one working.
pub fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_its_agents_by_id_or_program() {
        assert_eq!(find("claude").unwrap().label(), "Claude Code");
        assert_eq!(find("aider").unwrap().program(), "aider");
        assert_eq!(find("codex").unwrap().label(), "Codex");
        assert_eq!(find("grok").unwrap().label(), "Grok Bot");
        assert!(find("nonesuch").is_none(), "unknown names are not invented");
    }

    #[test]
    fn recognises_a_running_agent_from_its_command_line() {
        assert_eq!(of_command("claude --resume abc").unwrap().id(), "claude");
        assert_eq!(of_command("/usr/local/bin/aider").unwrap().id(), "aider");
        assert_eq!(
            of_command("C:\\tools\\gemini.exe --model x").unwrap().id(),
            "gemini"
        );
        assert!(of_command("bash").is_none());
        assert!(!is_agent("claudette --serve"));
    }

    #[test]
    fn passes_each_agent_only_the_flags_it_has() {
        let opts = Options {
            model: Some("opus"),
            effort: Some("high"),
            mode: Some("plan"),
        };
        assert_eq!(
            find("claude").unwrap().command(opts),
            vec![
                "claude",
                "--model",
                "opus",
                "--effort",
                "high",
                "--permission-mode",
                "plan"
            ]
        );
        // Aider takes a model and nothing else Sightline offers.
        assert_eq!(
            find("aider").unwrap().command(opts),
            vec!["aider", "--model", "opus"]
        );
        assert!(
            find("grok").unwrap().command(opts).is_empty(),
            "Grok Bot is not a command line"
        );
    }

    #[test]
    fn says_which_agents_have_insides_to_show() {
        assert!(find("claude").unwrap().keeps_transcripts());
        assert!(find("aider").unwrap().keeps_transcripts());
        // No adapter yet — watchable, steerable, but nothing structured.
        assert!(!find("codex").unwrap().keeps_transcripts());
    }

    #[test]
    fn each_says_how_a_session_gets_its_name() {
        assert_eq!(find("claude").unwrap().naming(), Naming::Command("/rename"));
        assert_eq!(find("aider").unwrap().naming(), Naming::Kept);
    }

    #[test]
    fn grok_is_connected_rather_than_spawned() {
        let grok = find("grok").unwrap();
        assert!(!grok.spawnable());
        assert_eq!(grok.delivery(), Delivery::Mailbox);
        // Ungoverned, and assignable anyway: nothing is spawned here, so there
        // is no local process for a boundary to stand in front of.
        assert_eq!(grok.governance(), Governance::None);
        assert_eq!(find("cursor").unwrap().delivery(), Delivery::Resume);
        assert_eq!(find("claude").unwrap().delivery(), Delivery::Pipe);
    }

    #[test]
    fn connections_include_grok_bot() {
        let all = connections(false);
        assert!(
            all.iter().any(|c| c.id == "grok" && c.label == "Grok Bot"),
            "Grok Bot sits in the same panel as Claude Code and Cursor: {:?}",
            all.iter().map(|c| c.id.as_str()).collect::<Vec<_>>()
        );
        let grok = all.iter().find(|c| c.id == "grok").unwrap();
        assert_eq!(grok.governance, Governance::None);
        assert!(
            grok.governance_note.contains("cloud computer"),
            "the note has to be this vendor's gap, not Cursor CLI's: {}",
            grok.governance_note
        );
    }
}
