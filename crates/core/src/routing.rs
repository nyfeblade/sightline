//! Which agent and model do which kind of work.
//!
//! The obvious version of this feature is the one this project has already
//! rejected: Sightline deciding that one model is better at architecture and
//! another at boilerplate. It will not, and the reason is not modesty. Any
//! ranking it shipped would be a guess about someone else's models, frozen at
//! the moment it was written, applied to work it cannot see, and wrong in a way
//! nobody would notice — because a fleet that quietly used the wrong model still
//! produces plausible output.
//!
//! So the judgement is the person's and the arithmetic is Sightline's. You write
//! down the routes; Sightline executes them, and measures what each one actually
//! cost and how much of its work reached Verified. Over a few weeks that turns an
//! opinion into a number, and the number is yours rather than a vendor's.
//!
//! A route is named, and a supervisor asks for it by name:
//!
//! ```toml
//! [[route]]
//! name   = "mechanical"
//! what   = "applying a change somebody has already decided"
//! agent  = "claude"
//! model  = "sonnet"
//! effort = "low"
//!
//! [[route]]
//! name   = "second-opinion"
//! what   = "a claim worth checking with a model that did not make it"
//! agent  = "cursor"
//! model  = "gpt-5.3-codex-high"
//! ```
//!
//! `what` is the whole of how a chief chooses. It is prose on purpose: the thing
//! being matched is a description of work, and the only reader that can match a
//! description of work to an assignment is the one writing the assignment.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Route {
    pub name: String,
    /// What this route is for, in the words of whoever wrote it.
    #[serde(default)]
    pub what: String,
    /// The agent's id. Empty means Claude Code.
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Routes {
    #[serde(default, rename = "route")]
    pub routes: Vec<Route>,
}

pub const FILE: &str = ".sightline/routing.toml";

/// The routes a project has written down, if it has.
///
/// Beside the constitution and the checks, and committed with them: which model
/// does which work is a decision about a project, and a decision about a project
/// belongs with its code rather than in one machine's settings.
pub fn load(from: &Path) -> Routes {
    let mut at = Some(from);
    while let Some(dir) = at {
        if let Ok(text) = std::fs::read_to_string(dir.join(FILE)) {
            return toml::from_str(&text).unwrap_or_default();
        }
        at = dir.parent();
    }
    Routes::default()
}

impl Routes {
    pub fn find(&self, name: &str) -> Option<&Route> {
        self.routes
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
    }

    /// The routes, as a supervisor should read them: what each is for, and what
    /// it will actually start. Empty when a project has written none, and the
    /// caller says so rather than inventing a default set — a route nobody chose
    /// is Sightline having an opinion.
    pub fn describe(&self) -> String {
        if self.routes.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for r in &self.routes {
            let mut how = Vec::new();
            if !r.agent.is_empty() {
                how.push(r.agent.clone());
            }
            if !r.model.is_empty() {
                how.push(r.model.clone());
            }
            if !r.effort.is_empty() {
                how.push(format!("effort {}", r.effort));
            }
            out.push_str(&format!(
                "   {:<16} {}\n",
                r.name,
                if r.what.is_empty() {
                    how.join(" · ")
                } else {
                    format!("{} — {}", r.what, how.join(" · "))
                }
            ));
        }
        out
    }
}

/// Where the file would go for a project.
pub fn path_in(root: &Path) -> PathBuf {
    root.join(FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[route]]
name   = "mechanical"
what   = "applying a change somebody has already decided"
agent  = "claude"
model  = "sonnet"
effort = "low"

[[route]]
name   = "second-opinion"
what   = "a claim worth checking with a model that did not make it"
agent  = "cursor"
model  = "gpt-5.3-codex-high"
"#;

    #[test]
    fn a_route_carries_everything_needed_to_start_the_worker() {
        let routes: Routes = toml::from_str(SAMPLE).unwrap();
        let r = routes.find("mechanical").expect("by name");
        assert_eq!(r.agent, "claude");
        assert_eq!(r.model, "sonnet");
        assert_eq!(r.effort, "low");
    }

    #[test]
    fn names_are_matched_the_way_people_type_them() {
        let routes: Routes = toml::from_str(SAMPLE).unwrap();
        assert!(routes.find("Second-Opinion").is_some());
        assert!(
            routes.find("nonesuch").is_none(),
            "unknown routes are not invented"
        );
    }

    #[test]
    fn a_project_with_no_routing_file_has_no_routes_rather_than_default_ones() {
        // The line this feature must not cross. A default set would be
        // Sightline ranking models — a guess about somebody else's product,
        // frozen when it was written, applied to work it cannot see.
        let empty = load(&std::env::temp_dir().join("sightline-no-routing-here"));
        assert!(empty.routes.is_empty());
        assert_eq!(empty.describe(), "");
    }

    #[test]
    fn what_a_route_is_for_reaches_the_supervisor_in_its_own_words() {
        let routes: Routes = toml::from_str(SAMPLE).unwrap();
        let text = routes.describe();
        assert!(text.contains("applying a change somebody has already decided"));
        assert!(text.contains("gpt-5.3-codex-high"));
    }
}
