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
//!
//! [[route]]
//! name   = "desktop"
//! what   = "work the Cursor desktop assistant is already in the middle of"
//! agent  = "grok"
//! ```
//!
//! `what` is the whole of how a chief chooses. It is prose on purpose: the thing
//! being matched is a description of work, and the only reader that can match a
//! description of work to an assignment is the one writing the assignment.
//!
//! `agent` is an adapter id: `claude`, `cursor`, or `grok`. Grok Bot is the
//! Cursor desktop assistant, not a CLI — assigning to `grok` connects that
//! chat rather than spawning a process.

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

[[route]]
name   = "desktop"
what   = "work the Cursor desktop assistant is already in the middle of"
agent  = "grok"
"#;

    #[test]
    fn a_route_carries_everything_needed_to_start_the_worker() {
        let routes: Routes = toml::from_str(SAMPLE).unwrap();
        let r = routes.find("mechanical").expect("by name");
        assert_eq!(r.agent, "claude");
        assert_eq!(r.model, "sonnet");
        assert_eq!(r.effort, "low");
        assert_eq!(routes.find("desktop").expect("by name").agent, "grok");
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

/// What a route actually cost, and how much of its work survived.
///
/// The point of writing routes down was never the routing. It was to make the
/// question answerable: is the cheap model actually cheaper once you count the
/// work it had to redo, and is the expensive one earning it?
///
/// Neither number alone answers that. Cost without outcomes rewards a route that
/// produces nothing; outcomes without cost rewards a route that spends
/// everything. So both, per route, and the ratio between them — which is the
/// only figure here worth acting on.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Outcome {
    pub route: String,
    pub tasks: usize,
    /// Reached the top of the ladder: something written to show the work wrong
    /// was run, did not fire, and has been seen to fire.
    pub verified: usize,
    /// The suite passed and nothing more was shown.
    pub checked: usize,
    pub open: usize,
    /// Gave up, or was refuted.
    pub failed: usize,
    pub turns: u64,
    pub output: u64,
    /// Context re-read, which is the term that dominates.
    pub cached: u64,
    pub estimate: f64,
}

impl Outcome {
    /// Cache reads billed at a tenth and writes at one and a quarter, which is
    /// how they are actually charged. Output is included and is almost never the
    /// term that matters — on a measured project it ran one part in sixty-seven.
    pub fn billable(&self) -> u64 {
        self.output + self.cached / 10
    }

    /// What one piece of finished work cost on this route.
    ///
    /// `None` when nothing has finished, and that is the honest answer rather
    /// than zero or infinity: a route with three open tasks and nothing verified
    /// has not yet said anything about itself.
    pub fn per_verified(&self) -> Option<u64> {
        (self.verified > 0).then(|| self.billable() / self.verified as u64)
    }
}

/// Every route's record, from the tasks it produced and what those sessions
/// spent.
///
/// Work assigned without a route is kept under `(unrouted)` rather than dropped.
/// It is usually most of the history when a project starts writing routes down,
/// and it is the only baseline the routes can be compared against.
pub fn outcomes(
    store: &crate::work::Store,
    spend: &std::collections::HashMap<String, crate::limits::Spend>,
) -> Vec<Outcome> {
    use crate::work::State;
    let mut by: std::collections::BTreeMap<String, Outcome> = Default::default();
    for task in store.tasks() {
        // A supervisor's own task is supervision, not work, and counting it
        // would attribute a chief's whole conversation to whichever route its
        // first worker used.
        if task.assignment.starts_with("supervise:") {
            continue;
        }
        let name = task
            .route
            .clone()
            .unwrap_or_else(|| "(unrouted)".to_string());
        let o = by.entry(name.clone()).or_insert_with(|| Outcome {
            route: name,
            ..Default::default()
        });
        o.tasks += 1;
        match &task.state {
            State::Verified => o.verified += 1,
            State::Checked => o.checked += 1,
            State::Abandoned => o.failed += 1,
            _ => o.open += 1,
        }
        if let Some(s) = spend.get(&task.session) {
            o.turns += s.turns;
            o.output += s.output;
            o.cached += s.cached;
            o.estimate += s.estimate;
        }
    }
    let mut all: Vec<Outcome> = by.into_values().collect();
    all.sort_by(|a, b| b.billable().cmp(&a.billable()));
    all
}

#[cfg(test)]
mod outcome_tests {
    use super::*;
    use crate::limits::Spend;
    use crate::work::{State, Store};
    use std::collections::HashMap;

    fn spent(turns: u64, output: u64, cached: u64) -> Spend {
        Spend {
            turns,
            output,
            cached,
            written: 0,
            estimate: 0.0,
        }
    }

    #[test]
    fn a_route_is_judged_on_what_it_finished_and_what_that_cost() {
        let mut s = Store::new();
        let cheap = s.assign("w1", "apply the rename");
        s.attribute(
            &cheap,
            Some("mechanical"),
            "claude",
            Some("sonnet"),
            Some("low"),
        );
        s.set_state(&cheap, State::Verified).unwrap();

        let dear = s.assign("w2", "work out the design");
        s.attribute(&dear, Some("design"), "claude", None, Some("high"));
        s.set_state(&dear, State::Verified).unwrap();

        let mut spend = HashMap::new();
        spend.insert("w1".to_string(), spent(20, 10_000, 1_000_000));
        spend.insert("w2".to_string(), spent(90, 90_000, 9_000_000));

        let all = outcomes(&s, &spend);
        let by = |n: &str| all.iter().find(|o| o.route == n).cloned().unwrap();
        assert_eq!(by("mechanical").verified, 1);
        assert_eq!(by("design").verified, 1);
        // The whole point: one finished piece of work on each, and one of them
        // cost eight times the other.
        assert!(
            by("design").per_verified().unwrap() > by("mechanical").per_verified().unwrap() * 5,
            "the expensive route has to look expensive: {:?} vs {:?}",
            by("design").per_verified(),
            by("mechanical").per_verified()
        );
    }

    #[test]
    fn a_route_that_has_finished_nothing_says_so_rather_than_looking_free() {
        // Cost with no outcome is the number that flatters the wrong route.
        // Zero would read as free and infinity as broken; neither is true of
        // work that is simply still going.
        let mut s = Store::new();
        let id = s.assign("w1", "still going");
        s.attribute(&id, Some("sweep"), "cursor", Some("composer-2.5"), None);
        let mut spend = HashMap::new();
        spend.insert("w1".to_string(), spent(40, 5_000, 4_000_000));

        let all = outcomes(&s, &spend);
        let sweep = all.iter().find(|o| o.route == "sweep").unwrap();
        assert_eq!(sweep.open, 1);
        assert_eq!(sweep.per_verified(), None);
        assert!(sweep.billable() > 0, "it has still spent what it has spent");
    }

    #[test]
    fn work_with_no_route_is_the_baseline_rather_than_a_gap() {
        // Usually most of the history when a project starts writing routes
        // down, and the only thing the routes can be compared against.
        let mut s = Store::new();
        let id = s.assign("w1", "done by hand");
        s.set_state(&id, State::Verified).unwrap();
        let all = outcomes(&s, &HashMap::new());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].route, "(unrouted)");
    }

    #[test]
    fn a_supervisors_own_task_is_not_charged_to_a_route() {
        // A chief's conversation is the most expensive session in a fleet.
        // Counting it would attribute all of it to whichever route its first
        // worker happened to use.
        let mut s = Store::new();
        s.assign("chief", "supervise: improve the thing");
        let w = s.assign("w1", "the actual work");
        s.attribute(&w, Some("mechanical"), "claude", None, None);
        let all = outcomes(&s, &HashMap::new());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].route, "mechanical");
    }
}
