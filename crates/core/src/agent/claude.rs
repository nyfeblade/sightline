//! Claude Code, which Sightline grew around.
//!
//! It writes a JSONL transcript per conversation under `~/.claude/projects`,
//! keeps a registry of live sessions under `~/.claude/sessions`, draws numbered
//! permission prompts, and renames a conversation when told `/rename`. Every
//! one of those is a habit of this agent rather than a fact about agents, which
//! is why it lives here rather than in the middle of scope.

use super::{Adapter, Found, Naming, Options, Record};
use std::path::PathBuf;

pub struct ClaudeCode;

impl Adapter for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn program(&self) -> &'static str {
        "claude"
    }

    fn command(&self, options: Options) -> Vec<String> {
        let mut argv = vec!["claude".to_string()];
        for (flag, value) in [
            ("--model", options.model),
            ("--effort", options.effort),
            ("--permission-mode", options.mode),
        ] {
            if let Some(v) = value {
                argv.push(flag.into());
                argv.push(v.into());
            }
        }
        argv
    }

    fn resume(&self, id: &str) -> Option<Vec<String>> {
        Some(vec!["claude".into(), "--resume".into(), id.into()])
    }

    fn naming(&self) -> Naming {
        Naming::Command("/rename")
    }

    fn record(&self) -> Record {
        Record::ClaudeJsonl
    }

    /// Every conversation on the machine, wherever it was held: Claude Code
    /// keeps them all in one place, so the folders Sightline has seen do not come
    /// into it.
    fn conversations(&self, _roots: &[PathBuf]) -> Vec<Found> {
        crate::history::scan(&crate::app::default_root())
            .into_iter()
            .map(|p| Found {
                id: p.id,
                path: p.path,
                cwd: p.cwd,
                modified: p.modified,
                bytes: p.bytes,
            })
            .collect()
    }

    fn install_hint(&self) -> Option<&'static str> {
        Some("npm install -g @anthropic-ai/claude-code")
    }

    fn signin_hint(&self) -> Option<&'static str> {
        Some("claude  (then /login)")
    }

    /// The only one, and the reason everything else in this program exists.
    ///
    /// Claude Code can hand a permission decision to a tool the host serves, so
    /// `gate::decide` runs before a call happens. No other agent here exposes
    /// that seam, which is a difference in what is being promised rather than a
    /// difference in polish.
    fn governance(&self) -> super::Governance {
        super::Governance::Full
    }
}
