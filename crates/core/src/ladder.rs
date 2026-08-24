//! Claimed → Checked → Verified, and the reasons for stopping short.
//!
//! This is Sightline's opinion about what finished means, and it is the part of
//! the product that is not plumbing. A wrapper around one session cannot hold
//! it, because two of the three steps depend on things that outlive the session:
//! what the project's checks are and whether a person approved them, and whether
//! a refutation has ever — at any point, in any earlier run — been seen to catch
//! something.
//!
//! The rule that does the work:
//!
//! ```text
//! Claimed    the agent says it is finished. Worth nothing on its own.
//! Checked    the suite passed. That says the failures it can express did not
//!            happen. It does not say the work is right.
//! Verified   something written to show the work wrong was run, did not fire,
//!            and has been seen to fire at some point.
//! ```
//!
//! The last clause is the one people drop, and dropping it is how a suite of
//! refutations that could never fire verifies everything forever.
//!
//! Lives here rather than in a front end because both the commands and the
//! kernel tool a worker calls have to reach the same verdict. Two
//! implementations of "done" is two definitions of done.

use crate::bus::{Event, Kind};
use crate::{checks, git, work};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where a claim got to, and why it got no further.
#[derive(Debug, Clone, PartialEq)]
pub enum Reached {
    /// A guarantee that must never stop being true has stopped being true.
    Broke { invariants: Vec<String> },
    /// The suite refused it.
    Refused { why: String },
    /// Something written to show the work wrong fired.
    Refuted { how: String },
    /// The suite passed, and that is as far as evidence goes.
    Checked { why_not_verified: String },
    /// Nothing that was tried could show the work wrong.
    Verified { refutations: usize },
}

impl Reached {
    pub fn state(&self) -> work::State {
        match self {
            // Not blocked: there is nothing to wait for, only something to fix.
            Reached::Broke { .. } | Reached::Refused { .. } | Reached::Refuted { .. } => {
                work::State::Working
            }
            Reached::Checked { .. } => work::State::Checked,
            Reached::Verified { .. } => work::State::Verified,
        }
    }

    pub fn good(&self) -> bool {
        matches!(self, Reached::Checked { .. } | Reached::Verified { .. })
    }

    /// How it reads to whoever asked — a person or the agent that claimed.
    pub fn say(&self) -> String {
        match self {
            Reached::Broke { invariants } => format!(
                "refused: {} invariant(s) broken — {}. These are guarantees that must \
                 never stop being true, so this is refused however green the suite is.",
                invariants.len(),
                invariants.join(", ")
            ),
            Reached::Refused { why } => format!("refused: {why}"),
            Reached::Refuted { how } => format!(
                "refuted: {how} succeeded, and it was written to fail. The work is \
                 wrong in the way that command was written to catch."
            ),
            Reached::Checked { why_not_verified } => {
                format!("checked, not verified: {why_not_verified}")
            }
            Reached::Verified { refutations } => format!(
                "verified: {refutations} demonstrated attempt(s) to show this wrong \
                 were run, and none of them fired"
            ),
        }
    }
}

/// What running the checks produced, and what it means.
pub struct Report {
    pub reached: Reached,
    pub outcomes: Vec<checks::Outcome>,
    /// Each refutation and how it went, in the order they were tried.
    pub tried: Vec<String>,
    /// Events worth journalling. The caller publishes: only one process may.
    pub events: Vec<Event>,
}

/// Run everything a project has to say about whether this work is finished, and
/// move the task to wherever the evidence actually reaches.
///
/// `session` is who claimed; `cwd` is where its work is. The store is written
/// through, so the verdict outlives the process that reached it.
pub fn adjudicate(store: &mut work::Store, session: &str, cwd: &Path) -> Result<Report, String> {
    let (root, suite) = checks::Suite::find(cwd)?.ok_or_else(|| {
        format!(
            "{} has no {} — a project has to say what finished means before anything \
             can be said to be finished",
            cwd.display(),
            checks::FILE
        )
    })?;
    // Nothing runs until these exact commands have been approved: a checks file
    // arrives with a repository, and cloning something should not run whatever
    // its author felt like running.
    if !checks::trusted(&root, &suite) {
        return Err(checks::untrusted_hint(&root, &suite));
    }

    let mut env = HashMap::new();
    if let Some(tree) = git::status(cwd) {
        env.insert("BRANCH".to_string(), tree.branch);
    }

    let task = store.task_for(session).map(|t| t.id.clone());
    let mut events = Vec::new();
    let mut tried = Vec::new();

    // The invariants first, and separately, because they answer a different
    // question. The checks ask whether this work is finished; these ask whether
    // it broke something that was never its business. That is the case a
    // passing suite is worst at catching, and the reason they exist.
    let broke: Vec<String> = suite
        .hold(&root, &env)
        .into_iter()
        .filter(|h| h.broken())
        .map(|h| h.name)
        .collect();
    if !broke.is_empty() {
        for name in &broke {
            events.push(Event::new(
                session,
                "foreman",
                Kind::ChecksFailed {
                    suite: format!("invariant · {name}"),
                    first: "a guarantee that must never stop being true has".into(),
                },
            ));
        }
        let reached = Reached::Broke { invariants: broke };
        return Ok(finish(store, task, reached, Vec::new(), tried, events));
    }

    let outcomes = suite.run(&root, &env);
    if !checks::Suite::verified(&outcomes) {
        let why = checks::Suite::refusal(&outcomes).unwrap_or_else(|| "not verified".into());
        for o in &outcomes {
            if let checks::State::Failed { first } = &o.state {
                events.push(Event::new(
                    session,
                    "foreman",
                    Kind::ChecksFailed {
                        suite: o.name.clone(),
                        first: first.clone(),
                    },
                ));
            }
        }
        let reached = Reached::Refused { why };
        return Ok(finish(store, task, reached, outcomes, tried, events));
    }

    for o in &outcomes {
        events.push(Event::new(
            session,
            "foreman",
            Kind::ChecksPassed {
                suite: o.name.clone(),
                ms: o.ms,
            },
        ));
    }

    // The suite passing is a floor, never a finish.
    let Some(id) = task.clone() else {
        let reached = Reached::Checked {
            why_not_verified: "this session has no task on record, so there is nothing \
                               to carry past checked"
                .into(),
        };
        return Ok(finish(store, None, reached, outcomes, tried, events));
    };
    let (refutations, proven) = store
        .get(&id)
        .map(|t| (t.refutes.clone(), t.proven.clone()))
        .unwrap_or_default();

    if refutations.is_empty() {
        let reached = Reached::Checked {
            why_not_verified: format!(
                "nothing says what wrong would look like for this work. Write one: \
                 `sightline refute {id} <a command that should fail>`"
            ),
        };
        return Ok(finish(store, task, reached, outcomes, tried, events));
    }

    let mut stood = 0;
    let mut unproven: Vec<String> = Vec::new();
    let mut unrunnable = 0;
    for command in &refutations {
        let (verdict, ms) = checks::refute(command, &root, &env);
        match verdict {
            checks::Verdict::Stands => {
                // Standing is only evidence if this refutation has ever been
                // seen to catch anything. One that cannot fire stands for ever
                // and proves nothing at all.
                if proven.iter().any(|p| p == command) {
                    stood += 1;
                    tried.push(format!("ok   {ms:>6}ms · did not fire · {command}"));
                } else {
                    unproven.push(command.clone());
                    tried.push(format!(
                        "??   {ms:>6}ms · did not fire, and never has · {command}"
                    ));
                }
            }
            checks::Verdict::Refuted { how } => {
                // It caught something. Bad news for the claim, good news for the
                // refutation: it is a demonstrated instrument now, so its
                // standing will mean something next time.
                store.proved(&id, command);
                tried.push(format!("FIRED {ms:>6}ms · {command}"));
                events.push(Event::new(
                    session,
                    "foreman",
                    Kind::ChecksFailed {
                        suite: "refutation".into(),
                        first: how.clone(),
                    },
                ));
                let reached = Reached::Refuted { how };
                return Ok(finish(store, task, reached, outcomes, tried, events));
            }
            checks::Verdict::Unrunnable { why } => {
                unrunnable += 1;
                tried.push(format!(
                    "??   {ms:>6}ms · could not run · {command} · {why}"
                ));
            }
        }
    }

    let reached = if stood == refutations.len() {
        Reached::Verified { refutations: stood }
    } else if !unproven.is_empty() {
        Reached::Checked {
            why_not_verified: format!(
                "{} refutation(s) have never caught anything, so their standing is not \
                 evidence — {}",
                unproven.len(),
                unproven.join(", ")
            ),
        }
    } else {
        Reached::Checked {
            why_not_verified: format!(
                "{unrunnable} of {} refutations could not be run",
                refutations.len()
            ),
        }
    };
    Ok(finish(store, task, reached, outcomes, tried, events))
}

/// Write the verdict down and hand it back.
fn finish(
    store: &mut work::Store,
    task: Option<String>,
    reached: Reached,
    outcomes: Vec<checks::Outcome>,
    tried: Vec<String>,
    events: Vec<Event>,
) -> Report {
    if let Some(id) = &task {
        let _ = store.set_state(id, reached.state());
        let _ = store.note(id, &reached.say());
        store.flush();
    }
    Report {
        reached,
        outcomes,
        tried,
        events,
    }
}

/// Where a session's work actually is, for the common case of asking about the
/// session rather than a path.
pub fn cwd_of(store: &work::Store, session: &str) -> Option<PathBuf> {
    let _ = store;
    crate::owned::get(session).map(|o| PathBuf::from(o.cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passing_checks_never_reach_verified_on_their_own() {
        // The whole point of the ladder, as a value rather than as prose: the
        // only state a passing suite can produce is Checked.
        let checked = Reached::Checked {
            why_not_verified: "nothing says what wrong would look like".into(),
        };
        assert_eq!(checked.state(), work::State::Checked);
        assert!(checked.good(), "checked is progress, not failure");
        assert!(
            checked.say().contains("not verified"),
            "it has to say so out loud: {}",
            checked.say()
        );
    }

    #[test]
    fn a_refuted_claim_goes_back_to_working_rather_than_blocked() {
        // Blocked means waiting on someone. Refuted means there is something to
        // fix, and the session is the one to fix it.
        let r = Reached::Refuted {
            how: "the guard was removed and the tests still passed".into(),
        };
        assert_eq!(r.state(), work::State::Working);
        assert!(!r.good());
    }

    #[test]
    fn a_broken_invariant_refuses_however_green_the_suite_is() {
        let r = Reached::Broke {
            invariants: vec!["one writer journals".into()],
        };
        assert_eq!(r.state(), work::State::Working);
        assert!(r.say().contains("however green"), "{}", r.say());
    }

    #[test]
    fn only_a_standing_demonstrated_refutation_verifies() {
        let v = Reached::Verified { refutations: 3 };
        assert_eq!(v.state(), work::State::Verified);
        assert!(
            v.say().contains("demonstrated"),
            "the word carries the rule: {}",
            v.say()
        );
    }
}
