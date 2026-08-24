//! Intent: what a project has decided, and what one assignment needs to know.
//!
//! Two artifacts, both plain files a person reads and edits.
//!
//! The constitution is the project's standing decisions — its mission, the
//! architecture and why, the constraints that always apply, the approaches
//! tried and rejected and the reason, what "done" means here, and the questions
//! still open. It exists so that a decision outlives the session that made it:
//! without it, every new session re-litigates the same choices because the
//! reasoning left with the last one.
//!
//! The brief is the opposite of a transcript. Handing a worker the whole history
//! is expensive and mostly irrelevant to the one thing it was asked to do. A
//! brief renders only that: the task, the constraints that bear on it, what
//! success looks like, and the conditions under which it must stop and ask
//! rather than decide.
//!
//! Neither is generated. A person writes the constitution and amends it as
//! decisions are made; the brief is assembled from it and from a task's own
//! record. Nothing here asks a model for anything — intent that has been
//! paraphrased by a model on the way in is intent you can no longer trust to
//! be what the person meant.

use crate::work::Task;
use std::path::{Path, PathBuf};

/// Where a project keeps its standing decisions.
pub const FILE: &str = ".sightline/constitution.md";

/// Where it used to live, and still may.
///
/// A project's own files are committed with its code, so a repository written
/// before the rename has the old directory and there is no upgrade step anyone
/// would think to run. Both are read; only the new one is written.
pub const FORMER: &str = ".ironsight/constitution.md";

/// A project's constitution, parsed into the sections a brief draws from.
///
/// The sections are fixed so they can be read programmatically, but the parse
/// is forgiving: an unknown heading is ignored rather than fatal, and a missing
/// section is simply empty. A half-written constitution still yields a usable
/// brief.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Constitution {
    pub mission: String,
    pub architecture: String,
    /// standing constraints, one per line; a `[tag]` prefix scopes it
    pub constraints: Vec<String>,
    pub preferences: Vec<String>,
    /// rejected approaches, with their reasons
    pub rejected: Vec<String>,
    /// what "done" means here, when a task does not say for itself
    pub done: Vec<String>,
    pub open: Vec<String>,
}

impl Constitution {
    /// Read a project's constitution, walking up from a directory the way the
    /// checks file is found — a session may work in a subdirectory of it.
    pub fn find(from: &Path) -> Option<(PathBuf, Constitution)> {
        let mut at = Some(from);
        while let Some(dir) = at {
            for name in [FILE, FORMER] {
                if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
                    return Some((dir.to_path_buf(), Constitution::parse(&text)));
                }
            }
            at = dir.parent();
        }
        None
    }

    /// Parse the markdown into its sections.
    ///
    /// A section is a `##` heading and the lines under it until the next one.
    /// Headings are matched loosely — by the keyword they contain — so
    /// "## Constraints" and "## Standing constraints" both land in the same
    /// place, and the writer is not made to match an exact string.
    pub fn parse(md: &str) -> Constitution {
        let mut c = Constitution::default();
        let mut current = Section::None;
        let mut prose: Vec<String> = Vec::new();

        let flush = |section: &Section, prose: &mut Vec<String>, c: &mut Constitution| {
            let text = prose.join("\n").trim().to_string();
            match section {
                Section::Mission => c.mission = text,
                Section::Architecture => c.architecture = text,
                Section::None => {}
                _ => {}
            }
            prose.clear();
        };

        for line in md.lines() {
            if let Some(title) = line.trim().strip_prefix("##") {
                flush(&current, &mut prose, &mut c);
                current = Section::of(title);
                continue;
            }
            match current {
                Section::Mission | Section::Architecture => prose.push(line.to_string()),
                Section::Constraints => push_item(line, &mut c.constraints),
                Section::Preferences => push_item(line, &mut c.preferences),
                Section::Rejected => push_item(line, &mut c.rejected),
                Section::Done => push_item(line, &mut c.done),
                Section::Open => push_item(line, &mut c.open),
                Section::None => {}
            }
        }
        flush(&current, &mut prose, &mut c);
        c
    }

    /// The standing constraints that bear on a task, and only those.
    ///
    /// A constraint with no `[tag]` is standing — it applies to all work, so it
    /// is always included. A tagged one applies only when its tag appears in the
    /// assignment, so a worker briefed on the database is not handed the
    /// front-end rules. This is what lets a brief be "the constraints that
    /// match" rather than the whole rulebook.
    pub fn constraints_for(&self, assignment: &str) -> Vec<String> {
        let haystack = assignment.to_ascii_lowercase();
        self.constraints
            .iter()
            .filter_map(|c| match tag_of(c) {
                Some((tag, rest)) => haystack.contains(&tag.to_ascii_lowercase()).then(|| rest),
                None => Some(c.clone()),
            })
            .collect()
    }
}

/// A `[tag]` prefix and the constraint after it, when there is one.
fn tag_of(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let tag = rest[..close].trim().to_string();
    let body = rest[close + 1..].trim().to_string();
    (!tag.is_empty() && !body.is_empty()).then_some((tag, body))
}

/// A line of a list section, added to it.
///
/// A line that starts with a bullet marker begins a new item; one that does not
/// is a wrapped continuation of the item above it, and is joined onto it rather
/// than becoming an item of its own — otherwise the second line of a two-line
/// constraint reads as a separate, untagged constraint, which is how a scoped
/// rule's tail leaked into every brief.
fn push_item(line: &str, into: &mut Vec<String>) {
    let raw = line.trim();
    if raw.is_empty() {
        return;
    }
    let is_bullet = raw.starts_with(['-', '*', '+'])
        || raw
            .split_once(['.', ')'])
            .is_some_and(|(head, _)| !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()));
    let content = raw
        .trim_start_matches(['-', '*', '+'])
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
        .trim();
    if is_bullet || into.is_empty() {
        if !content.is_empty() {
            into.push(content.to_string());
        }
    } else if let Some(last) = into.last_mut() {
        // A continuation: join with a space.
        last.push(' ');
        last.push_str(raw);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Mission,
    Architecture,
    Constraints,
    Preferences,
    Rejected,
    Done,
    Open,
}

impl Section {
    fn of(title: &str) -> Section {
        let t = title.to_ascii_lowercase();
        if t.contains("mission") {
            Section::Mission
        } else if t.contains("architect") {
            Section::Architecture
        } else if t.contains("constraint") {
            Section::Constraints
        } else if t.contains("prefer") {
            Section::Preferences
        } else if t.contains("reject") {
            Section::Rejected
        } else if t.contains("done") || t.contains("definition") {
            Section::Done
        } else if t.contains("open") || t.contains("question") {
            Section::Open
        } else {
            Section::None
        }
    }
}

/// Assemble the brief for one task.
///
/// The task carries its own constraints, success criteria and escalation
/// conditions; the constitution supplies the standing constraints that bear on
/// the work and, when the task names no success of its own, the project's
/// definition of done. Everything else in the constitution is deliberately left
/// out — a brief is not a transcript.
pub fn render(constitution: Option<&Constitution>, task: &Task) -> String {
    let mut out = String::new();

    if let Some(c) = constitution {
        if !c.mission.is_empty() {
            out.push_str(&format!("MISSION       {}\n\n", one_line(&c.mission)));
        }
    }

    out.push_str(&format!("TASK          {}\n", task.assignment));

    // Standing constraints that bear on this task, then the task's own.
    let mut constraints: Vec<String> = constitution
        .map(|c| c.constraints_for(&task.assignment))
        .unwrap_or_default();
    constraints.extend(task.constraints.iter().cloned());
    if !constraints.is_empty() {
        out.push('\n');
        for (i, c) in constraints.iter().enumerate() {
            let label = if i == 0 { "CONSTRAINTS" } else { "" };
            out.push_str(&format!("{label:<13} {c}\n"));
        }
    }

    // Success: the task's own, or the project's definition of done.
    let success: Vec<String> = if !task.success.is_empty() {
        task.success.clone()
    } else {
        constitution.map(|c| c.done.clone()).unwrap_or_default()
    };
    if !success.is_empty() {
        out.push('\n');
        for (i, s) in success.iter().enumerate() {
            let label = if i == 0 { "SUCCESS" } else { "" };
            out.push_str(&format!("{label:<13} {s}\n"));
        }
    }

    if !task.escalate_if.is_empty() {
        out.push('\n');
        for (i, e) in task.escalate_if.iter().enumerate() {
            let label = if i == 0 { "ESCALATE IF" } else { "" };
            out.push_str(&format!("{label:<13} {e}\n"));
        }
    }

    out
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::Store;

    const SAMPLE: &str = "\
# The project

## Mission
Watch and steer coding agents.
It should feel like one place.

## Architecture
One engine, two front ends.

## Constraints
- Dependencies point downward only.
- [database] Never change the schema without a migration.
- [frontend] No external scripts; the CSP forbids them.

## Preferences
- Plainer prose over clever prose.

## Rejected approaches
- A monolith — capability grows through layers, not features.

## Definition of done
- Tests pass.
- The compatibility suite covers it.

## Open questions
- Does supervision actually help?
";

    #[test]
    fn parses_a_constitution_into_its_sections() {
        let c = Constitution::parse(SAMPLE);
        assert!(c.mission.contains("Watch and steer"));
        assert!(c.architecture.contains("One engine"));
        assert_eq!(c.constraints.len(), 3);
        assert_eq!(c.preferences.len(), 1);
        assert_eq!(c.rejected.len(), 1);
        assert_eq!(c.done.len(), 2);
        assert_eq!(c.open.len(), 1);
    }

    #[test]
    fn a_wrapped_bullet_is_one_item_not_two() {
        let c = Constitution::parse(
            "## Constraints\n             - [db] Never change the schema without a\n  migration written first.\n             - A plain rule.\n",
        );
        assert_eq!(
            c.constraints.len(),
            2,
            "the wrapped line is not a third item"
        );
        assert!(c.constraints[0].contains("migration written first"));
        // And the continuation of a tagged rule does not leak into an untagged
        // brief: a task that does not mention the db sees neither line of it.
        let brief_out = {
            let mut s = Store::new();
            let id = s.assign("w", "restyle the header");
            render(Some(&c), s.get(&id).unwrap())
        };
        assert!(
            !brief_out.contains("migration"),
            "the tagged rule is omitted whole"
        );
        assert!(
            brief_out.contains("A plain rule"),
            "the untagged rule stays"
        );
    }

    #[test]
    fn a_loose_heading_still_lands_in_the_right_section() {
        let c = Constitution::parse("## Standing constraints\n- one\n## What done means\n- two\n");
        assert_eq!(c.constraints, vec!["one"]);
        assert_eq!(c.done, vec!["two"]);
    }

    #[test]
    fn a_brief_carries_the_constraints_that_bear_on_the_task_and_omits_the_rest() {
        let c = Constitution::parse(SAMPLE);
        let mut s = Store::new();
        let id = s.assign("worker", "add a database migration for the users table");
        let task = s.get(&id).unwrap();

        let brief = render(Some(&c), task);
        assert!(
            brief.contains("Dependencies point downward"),
            "an untagged standing constraint always applies"
        );
        assert!(
            brief.contains("Never change the schema"),
            "a [database] constraint applies to a database task"
        );
        assert!(
            !brief.contains("No external scripts"),
            "but a [frontend] constraint does not: it is not this task's concern"
        );
    }

    #[test]
    fn success_falls_back_to_the_projects_definition_of_done() {
        let c = Constitution::parse(SAMPLE);
        let mut s = Store::new();
        let id = s.assign("worker", "anything");
        let task = s.get(&id).unwrap();
        let brief = render(Some(&c), task);
        assert!(
            brief.contains("compatibility suite covers it"),
            "a task with no success of its own inherits the project's: {brief}"
        );
    }

    #[test]
    fn a_task_success_wins_over_the_project_default() {
        use crate::work::Task;
        let c = Constitution::parse(SAMPLE);
        let mut task = Task::new("t1".into(), "worker".into(), "the job".into());
        task.success = vec!["the callback returns 200".into()];
        let brief = render(Some(&c), &task);
        assert!(brief.contains("the callback returns 200"));
        assert!(
            !brief.contains("compatibility suite"),
            "the task's own success replaces the default, not adds to it"
        );
    }

    #[test]
    fn a_brief_without_a_constitution_is_still_the_task() {
        let mut s = Store::new();
        let id = s.assign("worker", "do the thing");
        let brief = render(None, s.get(&id).unwrap());
        assert!(brief.contains("TASK          do the thing"));
    }

    #[test]
    fn an_amended_decision_appears_in_the_next_brief() {
        // The constitution is a file; amending it is editing the file, and the
        // next brief reads the file again. This is the mechanism, in miniature.
        let before = Constitution::parse("## Constraints\n- one rule\n");
        let mut s = Store::new();
        let id = s.assign("w", "work");
        assert!(!render(Some(&before), s.get(&id).unwrap()).contains("a second rule"));
        let after = Constitution::parse("## Constraints\n- one rule\n- a second rule\n");
        assert!(render(Some(&after), s.get(&id).unwrap()).contains("a second rule"));
    }
}
