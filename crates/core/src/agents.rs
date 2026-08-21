//! The coding agents scope can run, and what each of them is called.
//!
//! scope grew around Claude Code, which writes a transcript and a live-session
//! registry, and that is where everything it knows about a session's insides
//! comes from — the feed, files, cost, subagents, plans. Other agents write
//! none of that.
//!
//! What does generalise is the part that made steering work in the first place:
//! a session is a program running in a terminal, and a terminal can be read and
//! typed into. So any agent here can be started, watched on screen, typed into,
//! interrupted, given its own worktree and closed, and only the panes that read
//! a transcript are missing. Nothing pretends otherwise: a session that keeps no
//! transcript says so rather than showing empty panes.

/// One agent scope knows how to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agent {
    /// what you type to choose it: `--agent codex`
    pub id: &'static str,
    /// how it is written in the interface
    pub label: &'static str,
    /// the program to run
    pub program: &'static str,
    /// whether it writes a transcript scope can read, which decides how much
    /// of a session can be shown beyond its screen
    pub transcripts: bool,
    /// the flag it takes to choose a model, when it has one
    pub model_flag: Option<&'static str>,
}

pub const CLAUDE: Agent = Agent {
    id: "claude",
    label: "Claude Code",
    program: "claude",
    transcripts: true,
    model_flag: Some("--model"),
};

pub const KNOWN: [Agent; 4] = [
    CLAUDE,
    Agent {
        id: "codex",
        label: "Codex",
        program: "codex",
        transcripts: false,
        model_flag: Some("--model"),
    },
    Agent {
        id: "gemini",
        label: "Gemini",
        program: "gemini",
        transcripts: false,
        model_flag: Some("--model"),
    },
    Agent {
        id: "aider",
        label: "Aider",
        program: "aider",
        transcripts: false,
        model_flag: Some("--model"),
    },
];

/// The agent with this id, or the one whose program this is. An id scope does
/// not know is not an error: it is a program name, run as given, which is how
/// anything else local gets to be a session too.
pub fn find(id: &str) -> Option<Agent> {
    KNOWN.into_iter().find(|a| a.id == id || a.program == id)
}

/// What a session was started with, read back. A pane knows its command line
/// and nothing else, so this is how a running session is identified as one of
/// ours at all.
pub fn of_command(cmd: &str) -> Option<Agent> {
    let program = cmd.split_whitespace().next()?;
    let program = program.rsplit(['/', '\\']).next()?;
    let program = program.strip_suffix(".exe").unwrap_or(program);
    KNOWN.into_iter().find(|a| a.program == program)
}

/// Whether a pane is running something scope should treat as a session.
pub fn is_agent(cmd: &str) -> bool {
    of_command(cmd).is_some()
}

/// What to run for an agent, with the options that apply to it. Options an
/// agent has no flag for are dropped rather than guessed at — passing Claude
/// Code's `--effort` to Aider would just make it refuse to start.
pub fn command(
    agent: Agent,
    model: Option<&str>,
    effort: Option<&str>,
    mode: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![agent.program.to_string()];
    if let (Some(flag), Some(value)) = (agent.model_flag, model) {
        argv.push(flag.into());
        argv.push(value.into());
    }
    // Effort and permission mode are Claude Code's, and only it is asked for
    // them.
    if agent.id == CLAUDE.id {
        if let Some(e) = effort {
            argv.push("--effort".into());
            argv.push(e.into());
        }
        if let Some(m) = mode {
            argv.push("--permission-mode".into());
            argv.push(m.into());
        }
    }
    argv
}

/// The command for something scope has no entry for, run exactly as typed.
pub fn custom_command(program: &str) -> Vec<String> {
    program.split_whitespace().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_its_agents_by_id_or_program() {
        assert_eq!(find("claude").unwrap().label, "Claude Code");
        assert_eq!(find("codex").unwrap().program, "codex");
        assert!(find("nonesuch").is_none(), "unknown ids are not invented");
    }

    #[test]
    fn recognises_a_running_agent_from_its_command_line() {
        assert_eq!(of_command("claude --resume abc").unwrap().id, "claude");
        assert_eq!(of_command("/usr/local/bin/codex").unwrap().id, "codex");
        assert_eq!(
            of_command("C:\\tools\\gemini.exe --model x").unwrap().id,
            "gemini"
        );
        assert!(of_command("bash").is_none());
        assert!(of_command("").is_none());
        // The old test was `starts_with("claude")`, which called this a session.
        assert!(!is_agent("claudette --serve"));
    }

    #[test]
    fn passes_each_agent_only_the_flags_it_has() {
        let claude = command(CLAUDE, Some("opus"), Some("high"), Some("plan"));
        assert_eq!(
            claude,
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
        // Aider takes a model and nothing else scope offers.
        let aider = command(
            find("aider").unwrap(),
            Some("gpt-5"),
            Some("high"),
            Some("plan"),
        );
        assert_eq!(aider, vec!["aider", "--model", "gpt-5"]);
        let bare = command(find("gemini").unwrap(), None, None, None);
        assert_eq!(bare, vec!["gemini"]);
    }

    #[test]
    fn only_claude_code_has_insides_to_show() {
        assert!(CLAUDE.transcripts);
        assert!(!find("codex").unwrap().transcripts);
    }
}
