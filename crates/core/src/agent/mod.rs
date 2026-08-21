//! What Ironsight needs from a coding agent, and how each one answers.
//!
//! Ironsight grew around Claude Code and learned its habits: a JSONL transcript per
//! conversation under `~/.claude/projects`, a registry of live sessions under
//! `~/.claude/sessions`, numbered permission prompts. None of that is shared by
//! other agents, and none of it is what makes Ironsight work.
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

/// How a session gets a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naming {
    /// The agent renames itself when told: the name then lives wherever the
    /// agent keeps it, and everything downstream agrees.
    Command(&'static str),
    /// The agent has no idea of a name, so Ironsight keeps one for it.
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

    fn naming(&self) -> Naming;

    /// How its transcript is written, if it writes one.
    fn record(&self) -> Record;

    /// Conversations it has recorded. `roots` are the folders Ironsight has seen
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
}

/// Everything Ironsight knows how to run.
pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude::ClaudeCode),
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

/// Whether a pane is running something Ironsight should treat as a session.
pub fn is_agent(cmd: &str) -> bool {
    of_command(cmd).is_some()
}

/// An agent Ironsight can run and watch on screen, but whose records it cannot
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

/// Whether a folder is worth asking an agent about — somewhere Ironsight has seen
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
        // Aider takes a model and nothing else Ironsight offers.
        assert_eq!(
            find("aider").unwrap().command(opts),
            vec!["aider", "--model", "opus"]
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
}
