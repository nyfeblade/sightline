//! Aider, whose habits are nothing like Claude Code's.
//!
//! It keeps no registry and no central store of conversations: the record lives
//! beside the code, as `.aider.chat.history.md` in the repository being worked
//! on. So conversations are found by looking where Ironsight has seen Aider
//! working rather than in one known place.
//!
//! The record is markdown, written as it goes:
//!
//! ```text
//! # aider chat started at 2026-08-21 10:45:04
//! > Aider v0.86.2
//! > Model: ollama_chat/qwen2.5-coder:7b with whole edit format
//! #### add a docstring to add()
//! calc.py
//! ```…```
//! > Tokens: 788 sent, 80 received.
//! ```
//!
//! `####` is what a person asked, plain text is the answer, and `>` is
//! everything the tool said about it — including how many tokens it cost, which
//! is where Ironsight's figures for an Aider session come from.

use super::{Adapter, Found, Naming, Options, Record};
use std::path::{Path, PathBuf};

pub struct Aider;

/// What the record is called, beside the code it is about.
pub const HISTORY: &str = ".aider.chat.history.md";

impl Adapter for Aider {
    fn id(&self) -> &'static str {
        "aider"
    }

    fn label(&self) -> &'static str {
        "Aider"
    }

    fn program(&self) -> &'static str {
        "aider"
    }

    fn command(&self, options: Options) -> Vec<String> {
        let mut argv = vec!["aider".to_string()];
        // Aider has a model and nothing else Ironsight offers; effort and
        // permission mode are Claude Code's ideas.
        if let Some(m) = options.model {
            argv.push("--model".into());
            argv.push(m.into());
        }
        argv
    }

    /// Aider picks up where it left off in a folder, so the id is the folder.
    fn resume(&self, id: &str) -> Option<Vec<String>> {
        Some(vec![
            "aider".into(),
            "--restore-chat-history".into(),
            "--".into(),
            id.into(),
        ])
    }

    fn naming(&self) -> Naming {
        Naming::Kept
    }

    fn record(&self) -> Record {
        Record::AiderMarkdown
    }

    fn conversations(&self, roots: &[PathBuf]) -> Vec<Found> {
        roots
            .iter()
            .filter_map(|root| found_in(root))
            .collect::<Vec<_>>()
    }
}

/// The conversation recorded in a folder, if there is one.
pub fn found_in(root: &Path) -> Option<Found> {
    let path = root.join(HISTORY);
    let md = std::fs::metadata(&path).ok()?;
    if md.len() == 0 {
        return None;
    }
    Some(Found {
        // Aider resumes by folder rather than by conversation id.
        id: root.to_string_lossy().into_owned(),
        path,
        cwd: root.to_string_lossy().into_owned(),
        modified: md.modified().ok()?,
        bytes: md.len(),
    })
}

/// What one line of the record says. Reading it a line at a time is what lets
/// Ironsight follow a session as it works rather than re-reading the file.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    /// a new run of aider in this folder
    Started(String),
    /// what a person asked
    Asked(String),
    /// what the agent said back
    Said(String),
    /// something the tool reported about itself
    Told(String),
    /// which model is answering
    Model(String),
    /// tokens sent and received for one exchange
    Tokens { sent: u64, received: u64 },
    /// what one exchange and the session so far have cost
    Cost { message: f64, session: f64 },
    /// nothing worth keeping
    Nothing,
}

pub fn read_line(line: &str) -> Line {
    let trimmed = line.trim_end();
    if let Some(when) = trimmed.strip_prefix("# aider chat started at ") {
        return Line::Started(when.trim().to_string());
    }
    if let Some(asked) = trimmed.strip_prefix("#### ") {
        return Line::Asked(asked.trim().to_string());
    }
    if let Some(told) = trimmed.strip_prefix("> ") {
        let told = told.trim();
        if let Some(model) = told.strip_prefix("Model: ") {
            let model = model.split(" with ").next().unwrap_or(model);
            return Line::Model(model.trim().to_string());
        }
        if let Some(counts) = told.strip_prefix("Tokens: ") {
            if let Some(tokens) = read_tokens(counts) {
                return tokens;
            }
        }
        if let Some(money) = told.strip_prefix("Cost: ") {
            if let Some(cost) = read_cost(money) {
                return cost;
            }
        }
        return Line::Told(told.to_string());
    }
    if trimmed.trim().is_empty() {
        return Line::Nothing;
    }
    Line::Said(trimmed.to_string())
}

/// `788 sent, 80 received.` — and the same with cache figures in between,
/// which are counted as sent because that is what they are.
fn read_tokens(text: &str) -> Option<Line> {
    let mut sent = 0;
    let mut received = 0;
    for part in text.trim_end_matches('.').split(',') {
        let part = part.trim();
        let (count, what) = part.split_once(' ')?;
        let n = scaled(count)?;
        match what.trim() {
            "received" => received = n,
            _ => sent += n,
        }
    }
    (sent > 0 || received > 0).then_some(Line::Tokens { sent, received })
}

/// Aider writes counts as `788`, `3.1k`, `1.2M`.
fn scaled(text: &str) -> Option<u64> {
    let text = text.trim();
    let (number, scale) = match text.chars().last()? {
        'k' | 'K' => (&text[..text.len() - 1], 1_000.0),
        'm' | 'M' => (&text[..text.len() - 1], 1_000_000.0),
        _ => (text, 1.0),
    };
    number.parse::<f64>().ok().map(|n| (n * scale) as u64)
}

/// `$0.0123 message, $0.0456 session.`
fn read_cost(text: &str) -> Option<Line> {
    let money = |part: &str| -> Option<f64> {
        part.trim()
            .trim_start_matches('$')
            .split_whitespace()
            .next()?
            .trim_start_matches('$')
            .parse()
            .ok()
    };
    let mut parts = text.trim_end_matches('.').split(',');
    let message = money(parts.next()?)?;
    let session = parts.next().and_then(money).unwrap_or(message);
    Some(Line::Cost { message, session })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lines taken from a real run: aider 0.86.2 against a local model.
    #[test]
    fn reads_what_was_asked_and_what_came_back() {
        assert_eq!(
            read_line("#### add a docstring to add()"),
            Line::Asked("add a docstring to add()".into())
        );
        assert_eq!(
            read_line("Understood. I will follow the provided format."),
            Line::Said("Understood. I will follow the provided format.".into())
        );
        assert_eq!(
            read_line("# aider chat started at 2026-08-21 10:45:04"),
            Line::Started("2026-08-21 10:45:04".into())
        );
        assert_eq!(read_line(""), Line::Nothing);
    }

    #[test]
    fn reads_the_model_and_what_it_cost() {
        assert_eq!(
            read_line("> Model: ollama_chat/qwen2.5-coder:7b with whole edit format  "),
            Line::Model("ollama_chat/qwen2.5-coder:7b".into())
        );
        assert_eq!(
            read_line("> Tokens: 788 sent, 80 received."),
            Line::Tokens {
                sent: 788,
                received: 80
            }
        );
        // Bigger numbers are written short, and cache figures count as sent.
        assert_eq!(
            read_line("> Tokens: 3.1k sent, 2.0k cache write, 245 received."),
            Line::Tokens {
                sent: 5100,
                received: 245
            }
        );
        assert_eq!(
            read_line("> Cost: $0.0123 message, $0.0456 session."),
            Line::Cost {
                message: 0.0123,
                session: 0.0456
            }
        );
    }

    #[test]
    fn everything_else_it_says_is_kept_as_what_it_is() {
        assert_eq!(
            read_line("> Add file to the chat? (Y)es/(N)o [Yes]: y"),
            Line::Told("Add file to the chat? (Y)es/(N)o [Yes]: y".into())
        );
        assert_eq!(
            read_line("> Aider v0.86.2"),
            Line::Told("Aider v0.86.2".into())
        );
    }

    #[test]
    fn finds_the_record_beside_the_code() {
        let dir = std::env::temp_dir().join(format!("scope-aider-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(found_in(&dir).is_none(), "no record, no conversation");
        std::fs::write(dir.join(HISTORY), "# aider chat started at now\n").unwrap();
        let found = found_in(&dir).expect("a record beside the code is a conversation");
        assert_eq!(found.cwd, dir.to_string_lossy());
        assert_eq!(found.id, found.cwd, "aider resumes by folder, not by id");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
