//! The permission boundary: where a rule stops being advice.
//!
//! Everything above this module used to be a brief — text handed to an agent
//! that it may follow or ignore, and which nothing checks. Claude Code's
//! control protocol makes a different thing possible: the host answers each
//! permission request itself, synchronously, before the tool runs. So the same
//! rules can be asked here instead, where declining is not one of the agent's
//! options.
//!
//! Four answers are possible, and `docs/probes/control_protocol.py` shows all
//! of them working against the real tool:
//!
//! ```text
//! allow      it is within what this session may do
//! rewrite    it may proceed, in an altered form
//! deny       with a reason the model reads and can act on
//! abstain    no kernel is confident — someone should be asked
//! ```
//!
//! `rewrite` is the one with no equivalent in a settings file or a hook, and it
//! is what keeps the number of questions low enough to live with. A gate that
//! can only say yes or no has to escalate every ambiguous call to a person; one
//! that can amend answers most of them itself.
//!
//! The kernels here are ordinary functions over a tool name and its input. They
//! are deterministic, they hold no model, and every one of them is tested by
//! calling it — which is the point of moving them here.

use crate::limits;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// What the host decides about one tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// It may happen as asked.
    Allow,
    /// It may happen, but not as asked.
    Rewrite { input: Value, why: String },
    /// It may not happen. The reason goes to the model, which can act on it.
    Deny { why: String },
}

impl Decision {
    pub fn denied(&self) -> bool {
        matches!(self, Decision::Deny { .. })
    }

    /// How the decision reads in the journal and in the window.
    pub fn option(&self) -> String {
        match self {
            Decision::Allow => "allow".into(),
            Decision::Rewrite { .. } => "rewrite".into(),
            Decision::Deny { .. } => "deny".into(),
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Decision::Allow => "",
            Decision::Rewrite { why, .. } | Decision::Deny { why } => why,
        }
    }
}

/// What one session may do.
///
/// Declarative, because it travels: the daemon holds sessions, so this has to
/// survive a socket and arrive on the far side meaning the same thing. An
/// unknown field is refused rather than dropped, for the reason `owned::Spec`
/// gives — a policy silently missing the field that restricted it is worse than
/// no policy, because it looks like one.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    /// The directory this session owns. Writes land here or nowhere.
    #[serde(default)]
    pub root: String,
    /// Commands refused whatever the permission mode says.
    #[serde(default)]
    pub forbid: Vec<String>,
    /// Consult the ceiling before every call, not only at the door.
    #[serde(default)]
    pub ceilings: bool,
    /// Whether this session was started to do assigned work.
    ///
    /// A flag rather than the task's id, because the task a session is doing can
    /// change while it runs and the id here would go stale. Which task it is, is
    /// looked up by session name — a name Sightline chose, that the session
    /// never gets to pick or change.
    #[serde(default)]
    pub assigned: bool,
}

impl Policy {
    /// A worker confined to one directory, with the usual refusals.
    pub fn confined_to(root: &Path) -> Self {
        Policy {
            root: root.to_string_lossy().into_owned(),
            forbid: FORBID.iter().map(|s| s.to_string()).collect(),
            ceilings: true,
            assigned: false,
        }
    }

    /// The same, but doing work somebody wrote down.
    pub fn on_assigned_work(mut self) -> Self {
        self.assigned = true;
        self
    }
}

/// Commands that are refused whatever else a session is allowed.
///
/// Not a security boundary — a determined agent can spell any of these another
/// way — but the list of things that have no business happening unattended, and
/// each is cheap to catch. The point is that an accident is stopped, not that
/// an adversary is.
pub const FORBID: [&str; 7] = [
    "git push",
    "rm -rf /",
    "sudo ",
    "shutdown",
    "mkfs",
    "> /dev/sd",
    "curl | sh",
];

/// The tools whose input names a file the session is about to change.
const WRITES: [&str; 3] = ["Write", "Edit", "NotebookEdit"];

/// Ask every kernel in turn. The first one with an opinion decides.
///
/// The order is not cosmetic. A ceiling that has been reached is the end of the
/// conversation, so it is asked first: there is no point deciding whether a
/// write is in scope for a fleet that is not allowed to be doing anything. The
/// forbidden list comes next because it is absolute. Scope is last because it
/// is the only one that can *amend* rather than refuse, and amending something
/// that was going to be denied outright would be a lie about what happened.
pub fn decide(
    policy: &Policy,
    session: &str,
    tool: &str,
    input: &Value,
) -> (Decision, &'static str) {
    let asking = Asking {
        policy,
        session,
        tool,
        input,
    };
    for (name, kernel) in KERNELS {
        if let Some(d) = kernel(&asking) {
            return (d, name);
        }
    }
    // Nothing refused it, so remember what it is about to do. Only now: a call
    // that was denied never happens, and holding a file against a session that
    // was stopped from touching it would block the next one for nothing.
    asking.remember();
    (Decision::Allow, "abstain")
}

/// One call, and everything a kernel is allowed to know about it.
///
/// The session's name is here because two of these kernels are about the fleet
/// rather than the call — and a per-session wrapper, which is what this would be
/// without it, cannot ask those questions at all.
pub struct Asking<'a> {
    pub policy: &'a Policy,
    pub session: &'a str,
    pub tool: &'a str,
    pub input: &'a Value,
}

type Kernel = fn(&Asking) -> Option<Decision>;

const KERNELS: [(&str, Kernel); 6] = [
    ("ceiling", ceiling),
    ("forbid", forbid),
    ("task", task),
    ("trust", trust),
    ("collision", collision),
    ("scope", scope),
];

/// Nothing happens for a fleet that has spent what it was allowed.
///
/// The ceiling is read from Sightline's own directory, outside every worktree,
/// so a session cannot answer this question in its own favour by editing a file
/// it can reach.
///
/// Only the spend half is asked here, and the distinction cost a bug. A count
/// ceiling answers "may another session start", which is a question for the
/// door — `limits::refuse`, from `kernel::assign`. Asking it on every tool call
/// means a fleet running at exactly its permitted size refuses all of its own
/// work, because one more than what is running is one more than the ceiling.
/// The ceiling would hold perfectly and nothing would ever get done.
fn ceiling(a: &Asking) -> Option<Decision> {
    let policy = a.policy;
    if !policy.ceilings {
        return None;
    }
    let root = PathBuf::from(&policy.root);
    let limits = match limits::in_force(&root) {
        Ok(l) => l,
        // A ceilings file that will not parse is not permission to ignore the
        // ceiling. Refusing is the safe direction, and the loud one.
        Err(why) => {
            return Some(Decision::Deny {
                why: format!("the ceilings could not be read — {why}"),
            });
        }
    };
    let most = limits.spend?;
    let spent = limits::spent_since(
        &crate::app::data_dir().join("events.jsonl"),
        limits.window_hours(),
    );
    if spent < most {
        return None;
    }
    Some(Decision::Deny {
        why: format!(
            "${spent:.2} has been spent in the last {} hours and the ceiling is \
             ${most:.2}. Nothing more will be allowed until the window rolls over \
             or somebody raises it. Stop, and say so.",
            limits.window_hours()
        ),
    })
}

/// The things that have no business happening unattended.
fn forbid(a: &Asking) -> Option<Decision> {
    if a.tool != "Bash" {
        return None;
    }
    let command = a.input.get("command").and_then(Value::as_str)?;
    let hit = a
        .policy
        .forbid
        .iter()
        .find(|f| command.contains(f.as_str()))?;
    Some(Decision::Deny {
        why: format!(
            "`{hit}` is on this session's forbidden list. If it is genuinely \
             needed, it is a decision for whoever is supervising, not for you."
        ),
    })
}

/// A session writes inside the directory it owns, or it does not write.
///
/// The interesting case is not the refusal, it is the amendment. A worker in a
/// worktree that writes to the main checkout by absolute path is making the
/// single most expensive mistake available to it — the isolation is gone and
/// nobody finds out until later. But it is nearly always a stale path rather
/// than an intention, and the same file exists where it should be writing. So
/// the call is redirected there instead of refused, and the session is told.
fn scope(a: &Asking) -> Option<Decision> {
    let policy = a.policy;
    if policy.root.is_empty() || !WRITES.contains(&a.tool) {
        return None;
    }
    let root = PathBuf::from(&policy.root);
    let (key, raw) = written(a.input)?;
    let target = normalize(&root, raw);
    if target.starts_with(&root) {
        return None;
    }
    match redirect(&root, &target) {
        Some(inside) => {
            let mut amended = a.input.clone();
            amended[key] = Value::String(inside.to_string_lossy().into_owned());
            Some(Decision::Rewrite {
                input: amended,
                why: format!(
                    "{} is outside the directory this session owns. The same path \
                     exists inside it, so the write was redirected to {}.",
                    target.display(),
                    inside.display()
                ),
            })
        }
        None => Some(Decision::Deny {
            why: format!(
                "{} is outside {}, which is the only place this session may write. \
                 Nothing inside it matches that path, so it has not been redirected.",
                target.display(),
                root.display()
            ),
        }),
    }
}

/// Work nobody asked for is not work.
///
/// Sightline wrote down what this session was assigned before it started, and
/// the session has never seen that record. So this is a question only the host
/// can answer, and it answers two things: a session with no assignment is not
/// supposed to be changing anything, and a session whose assignment is finished
/// is changing things nobody is going to check.
///
/// It is deliberately not a judgement about whether the *particular* change
/// serves the task — that needs a model, and a model at this boundary is a
/// second agent to be wrong. It is the two cases that can be decided from a
/// record.
fn task(a: &Asking) -> Option<Decision> {
    if !WRITES.contains(&a.tool) {
        return None;
    }
    if !a.policy.assigned {
        return None;
    }
    let store = crate::work::Store::load(crate::work::path_in(&crate::app::data_dir()));
    let Some(t) = store.task_for(a.session) else {
        return Some(Decision::Deny {
            why: "there is no record of what this session was asked to do. Changing \
                  files without an assignment means nothing will check the work and \
                  nobody is expecting it. Stop, and say so."
                .into(),
        });
    };
    if t.state.open() {
        return None;
    }
    Some(Decision::Deny {
        why: format!(
            "the task this session was given is {} — finished. Further changes are \
             unattributed: no check covers them and nobody is waiting for them. If \
             more is genuinely needed it is a new assignment, which is not yours to \
             make.",
            t.state.label()
        ),
    })
}

/// The rules a session is judged by are not the session's to edit.
///
/// Two things, both of which use a record kept outside the repository:
///
/// A project's `checks.toml` is shell that arrived with somebody else's code,
/// and it does not run until `sightline trust` has approved those exact
/// commands. An agent that reads the file and runs what it found would route
/// straight around that, so the commands are refused here by the same record.
///
/// And nothing writes to the files that define what done means. A session that
/// can edit `checks.toml` can make its own work pass; one that can edit the
/// ceilings can raise them. Both are the same mistake — marking your own
/// homework — and both are cheap to stop.
fn trust(a: &Asking) -> Option<Decision> {
    let root = PathBuf::from(&a.policy.root);
    if a.policy.root.is_empty() {
        return None;
    }

    if let Some((_, raw)) = written(a.input).filter(|_| WRITES.contains(&a.tool)) {
        let path = normalize(&root, raw);
        // Components, not a substring. Windows writes `.sightline\checks.toml`,
        // and searching the display string for a forward slash misses it — the
        // file would then be editable, which is the thing this kernel exists
        // to stop. `Path::ends_with` compares names, so the slash used to write
        // the path is not a way around it.
        if let Some(what) = GOVERNING
            .iter()
            .copied()
            .find(|g| path.ends_with(Path::new(g)))
        {
            return Some(Decision::Deny {
                why: format!(
                    "{what} is part of what decides whether your work is any good. \
                     Changing it from inside the work is marking your own homework. \
                     If it is wrong, say so in your report."
                ),
            });
        }
    }

    if a.tool != "Bash" {
        return None;
    }
    let command = a.input.get("command").and_then(Value::as_str)?;
    let (_, suite) = crate::checks::Suite::find(&root).ok().flatten()?;
    if crate::checks::trusted(&root, &suite) {
        return None;
    }
    let tidy = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let wanted = tidy(command);
    let hit = suite.checks.iter().find(|c| tidy(&c.run) == wanted)?;
    Some(Decision::Deny {
        why: format!(
            "`{}` is one of this project's checks, and nobody has approved this \
             project's checks yet. They are shell that arrived with the code, so \
             they do not run until a person has read them: `sightline trust {}`. \
             Report that rather than working around it.",
            hit.name,
            root.display()
        ),
    })
}

/// Files that decide whether work is any good, and are therefore not the work.
const GOVERNING: [&str; 3] = [
    ".sightline/checks.toml",
    ".sightline/constitution.md",
    "limits.toml",
];

/// Two agents, one file.
///
/// This is the question no wrapper around a single session can ask, because
/// answering it needs to see the other one. Two workers editing the same file
/// at the same time produce a merge nobody asked for and a bug nobody can
/// attribute, and the failure is silent: both of them report success.
///
/// The second one to arrive is refused and told who has it. Refused rather than
/// queued, because holding a permission request open for however long another
/// agent takes is a session that looks wedged.
fn collision(a: &Asking) -> Option<Decision> {
    if !WRITES.contains(&a.tool) {
        return None;
    }
    let (_, raw) = written(a.input)?;
    let path = normalize(&PathBuf::from(&a.policy.root), raw);
    let held = holders();
    let mut held = match held.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let now = std::time::Instant::now();
    // Kept unless it is known to be over: a session the fleet has never heard of
    // is not evidence that the hold is stale, and treating it as such would drop
    // a live claim. What actually clears a hold is `release`, called when the
    // session ends; this is the backstop for the case where that never ran.
    held.retain(|_, (who, when)| {
        now.duration_since(*when) < HOLD && !matches!(crate::owned::get(who), Some(o) if !o.alive)
    });
    let (who, _) = held.get(&path)?;
    if who == a.session {
        return None;
    }
    Some(Decision::Deny {
        why: format!(
            "{} is being changed by {who}, which is still running. Two sessions \
             editing one file produces a result neither of them meant and neither \
             can be blamed for. Work on something else, or ask for {who} to finish \
             first.",
            path.display()
        ),
    })
}

/// How long a session is taken to still be working on a file it touched.
///
/// Long enough to cover a turn that is thinking, short enough that a session
/// which wandered off does not hold a file for the rest of the day. Liveness is
/// checked as well, so this only matters for a session that is alive and idle.
const HOLD: std::time::Duration = std::time::Duration::from_secs(20 * 60);

type Holders = std::collections::HashMap<PathBuf, (String, std::time::Instant)>;

fn holders() -> &'static std::sync::Mutex<Holders> {
    static HELD: std::sync::OnceLock<std::sync::Mutex<Holders>> = std::sync::OnceLock::new();
    HELD.get_or_init(|| std::sync::Mutex::new(Holders::new()))
}

impl Asking<'_> {
    /// Note that this session is about to change this file.
    ///
    /// Called only once every kernel has let the call through, so a refusal
    /// never takes a file out of circulation.
    fn remember(&self) {
        if !WRITES.contains(&self.tool) {
            return;
        }
        let Some((_, raw)) = written(self.input) else {
            return;
        };
        let path = normalize(&PathBuf::from(&self.policy.root), raw);
        if let Ok(mut held) = holders().lock() {
            held.insert(path, (self.session.to_string(), std::time::Instant::now()));
        }
    }
}

/// Forget every file this session was holding.
///
/// Called when a session ends: a file held by something that is no longer
/// running is a file nobody can work on.
pub fn release(session: &str) {
    if let Ok(mut held) = holders().lock() {
        held.retain(|_, (who, _)| who != session);
    }
}

/// The path a tool call is about to write to, and which field named it.
fn written(input: &Value) -> Option<(&'static str, &str)> {
    for key in ["file_path", "notebook_path"] {
        if let Some(v) = input.get(key).and_then(Value::as_str) {
            return Some((key, v));
        }
    }
    None
}

/// A path as it will actually be resolved, without needing it to exist.
///
/// `canonicalize` is no use here: the file is usually about to be created. So
/// this resolves `.` and `..` textually, which is the same answer for every path
/// that does not cross a symlink, and errs towards *not* matching the root when
/// it cannot tell — the direction that refuses rather than the one that lets
/// something through.
fn normalize(root: &Path, raw: &str) -> PathBuf {
    let expanded = crate::app::expand(raw);
    let joined = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        root.join(&expanded)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The longest tail of `target` that names something real under `root`.
///
/// Longest, because a short tail matches too easily: `src/lib.rs` exists in most
/// repositories, and redirecting to the wrong `lib.rs` would be worse than
/// refusing. The parent has to exist, not the file, because the file is usually
/// the thing being created.
///
/// Only the named components are considered. A tail that still carried the root
/// of the filesystem would be absolute, and `join` replaces rather than appends
/// when given an absolute path — so the "redirect" would hand back the very path
/// it was asked to move, and report success. It did exactly that once.
fn redirect(root: &Path, target: &Path) -> Option<PathBuf> {
    let parts: Vec<_> = target
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect();
    for take in (2..=parts.len().min(12)).rev() {
        let tail: PathBuf = parts[parts.len() - take..].iter().collect();
        let candidate = root.join(&tail);
        if candidate.parent().is_some_and(Path::is_dir) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bare(root: &Path) -> Policy {
        Policy {
            root: root.to_string_lossy().into_owned(),
            forbid: FORBID.iter().map(|s| s.to_string()).collect(),
            ceilings: false,
            assigned: false,
        }
    }

    #[test]
    fn a_write_inside_the_worktree_is_nobodys_business() {
        let dir = tempdir();
        let (d, by) = decide(
            &bare(&dir),
            "asker",
            "Write",
            &json!({"file_path": dir.join("src/x.rs").to_string_lossy(), "content": "hi"}),
        );
        assert_eq!(d, Decision::Allow);
        assert_eq!(by, "abstain", "no kernel should have had an opinion");
    }

    #[test]
    fn a_write_to_the_main_checkout_is_redirected_into_the_worktree() {
        // The mistake this exists for: a worker in a worktree writing to the
        // repository it was branched from, by a path that was correct an hour
        // ago.
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let elsewhere = tempdir();
        std::fs::create_dir_all(elsewhere.join("src")).unwrap();

        let (d, by) = decide(
            &bare(&dir),
            "asker",
            "Edit",
            &json!({"file_path": elsewhere.join("src/main.rs").to_string_lossy()}),
        );
        assert_eq!(by, "scope");
        match d {
            Decision::Rewrite { input, .. } => {
                let got = Path::new(input["file_path"].as_str().unwrap());
                let want = dir.join("src").join("main.rs");
                assert_eq!(
                    got, want.as_path(),
                    "it must land inside the worktree, not merely be complained about"
                );
            }
            other => panic!("expected a redirect, got {other:?}"),
        }
    }

    #[test]
    fn a_write_outside_with_nothing_to_redirect_to_is_refused() {
        let dir = tempdir();
        let (d, by) = decide(
            &bare(&dir),
            "asker",
            "Write",
            &json!({"file_path": "/etc/nowhere/that/exists/here.conf"}),
        );
        assert_eq!(by, "scope");
        assert!(d.denied(), "got {d:?}");
    }

    #[test]
    fn dots_do_not_walk_out_of_the_worktree() {
        // The refutation for `normalize`: if it only compared prefixes, this
        // would read as inside the root and be allowed.
        let dir = tempdir();
        let (d, _) = decide(
            &bare(&dir),
            "asker",
            "Write",
            &json!({"file_path": "../../etc/passwd"}),
        );
        assert!(
            d.denied(),
            "a relative path climbing out of the root was allowed: {d:?}"
        );
    }

    #[test]
    fn the_forbidden_list_does_not_care_what_the_mode_says() {
        let dir = tempdir();
        let (d, by) = decide(
            &bare(&dir),
            "asker",
            "Bash",
            &json!({"command": "git push --force origin master"}),
        );
        assert_eq!(by, "forbid");
        assert!(d.denied());
        assert!(
            d.why().contains("git push"),
            "the reason has to name the thing, so the model can act on it: {}",
            d.why()
        );
    }

    #[test]
    fn reading_is_not_writing() {
        let dir = tempdir();
        let (d, _) = decide(
            &bare(&dir),
            "asker",
            "Read",
            &json!({"file_path": "/etc/hostname"}),
        );
        assert_eq!(
            d,
            Decision::Allow,
            "a worker must be able to read what it is porting from"
        );
    }

    #[test]
    fn a_fleet_at_exactly_its_permitted_size_still_gets_to_work() {
        // The bug this exists for: the count ceiling answers "may another start",
        // and asking it per tool call means a fleet of the permitted size denies
        // all of its own calls. The ceiling holds and nothing happens.
        let dir = tempdir();
        let mut policy = bare(&dir);
        policy.ceilings = true;
        let limits = limits::Limits {
            sessions: Some(1),
            spend: None,
            window: None,
        };
        assert!(
            limits::refuse(&limits, 1, 0.0).is_some(),
            "the door should refuse a second session — that is its job"
        );
        let (d, _) = decide(
            &policy,
            "asker",
            "Read",
            &json!({"file_path": dir.join("x").to_string_lossy()}),
        );
        assert_eq!(
            d,
            Decision::Allow,
            "but the session that is already running must still be able to work"
        );
    }

    #[test]
    fn an_empty_policy_decides_nothing() {
        let (d, by) = decide(
            &Policy::default(),
            "asker",
            "Write",
            &json!({"file_path": "/anywhere.txt"}),
        );
        assert_eq!(d, Decision::Allow);
        assert_eq!(by, "abstain");
    }

    #[test]
    fn two_sessions_cannot_change_one_file() {
        // The question no wrapper around a single session can ask, because
        // answering it means seeing the other one. Both would report success.
        let dir = tempdir();
        let file = dir.join("shared.rs");
        let policy = bare(&dir);
        let call = json!({"file_path": file.to_string_lossy()});

        let (first, _) = decide(&policy, "owned-1", "Write", &call);
        assert_eq!(first, Decision::Allow, "the first one through is fine");

        let (second, by) = decide(&policy, "owned-2", "Write", &call);
        assert_eq!(by, "collision");
        assert!(second.denied(), "got {second:?}");
        assert!(
            second.why().contains("owned-1"),
            "the refusal has to name who has it, or it cannot be acted on: {}",
            second.why()
        );

        // And the one holding it is not blocked by its own claim.
        let (again, _) = decide(&policy, "owned-1", "Write", &call);
        assert_eq!(again, Decision::Allow);
        release("owned-1");

        // Once it lets go, the other may proceed.
        let (now, _) = decide(&policy, "owned-2", "Write", &call);
        assert_eq!(
            now,
            Decision::Allow,
            "a released file has to become workable"
        );
        release("owned-2");
    }

    #[test]
    fn a_refused_call_does_not_take_the_file_out_of_circulation() {
        // The bug this exists for: remembering the file before the kernels have
        // spoken means a denied write reserves a file that was never touched,
        // and the next session is refused for a call that never happened.
        let dir = tempdir();
        let policy = bare(&dir);
        let forbidden = dir.join(".sightline").join("checks.toml");
        std::fs::create_dir_all(dir.join(".sightline")).unwrap();
        let call = json!({"file_path": forbidden.to_string_lossy()});

        let (d, by) = decide(&policy, "owned-1", "Write", &call);
        assert_eq!(by, "trust");
        assert!(d.denied());

        let (after, by) = decide(&policy, "owned-2", "Write", &call);
        assert_eq!(
            by, "trust",
            "it must be refused for the same reason as the first, not for collision"
        );
        assert!(!after.why().contains("owned-1"), "{}", after.why());
    }

    #[test]
    fn the_rules_a_session_is_judged_by_are_not_its_to_edit() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join(".sightline")).unwrap();
        // Built with join so each name is a component, the way Windows writes
        // a path. A substring search for `.sightline/checks.toml` misses the
        // backslash form and the file becomes editable.
        for target in [
            dir.join(".sightline").join("checks.toml"),
            dir.join(".sightline").join("constitution.md"),
        ] {
            let (d, by) = decide(
                &bare(&dir),
                "owned-1",
                "Edit",
                &json!({"file_path": target.to_string_lossy()}),
            );
            assert_eq!(by, "trust", "{}", target.display());
            assert!(d.denied(), "{} was editable: {d:?}", target.display());
        }
    }

    #[test]
    fn a_projects_own_checks_do_not_run_until_someone_approved_them() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join(".sightline")).unwrap();
        std::fs::write(
            dir.join(".sightline/checks.toml"),
            "[[check]]\nname = \"tests\"\nrun = \"cargo test --all\"\n",
        )
        .unwrap();

        // Reading it is fine. Running what it says is not, until it is trusted.
        let (d, by) = decide(
            &bare(&dir),
            "owned-1",
            "Bash",
            &json!({"command": "cargo  test   --all"}),
        );
        assert_eq!(by, "trust", "whitespace must not be a way around it");
        assert!(d.denied(), "{d:?}");
        assert!(d.why().contains("sightline trust"), "{}", d.why());

        // Something that is not one of the project's checks is not this
        // kernel's business.
        let (other, _) = decide(
            &bare(&dir),
            "owned-1",
            "Bash",
            &json!({"command": "ls -la"}),
        );
        assert_eq!(other, Decision::Allow);
    }

    #[test]
    fn work_nobody_asked_for_is_refused() {
        let dir = tempdir();
        let mut policy = bare(&dir);
        policy.assigned = true;
        let (d, by) = decide(
            &policy,
            "a-session-with-no-task",
            "Write",
            &json!({"file_path": dir.join("x.rs").to_string_lossy()}),
        );
        assert_eq!(by, "task");
        assert!(d.denied(), "{d:?}");
        assert!(d.why().contains("no record"), "{}", d.why());
    }

    #[test]
    fn a_session_that_was_never_given_work_is_not_policed_for_having_none() {
        // The refutation for the kernel above being too eager: a session started
        // by hand has no assignment and is not doing anything wrong.
        let dir = tempdir();
        let (d, _) = decide(
            &bare(&dir),
            "unassigned",
            "Write",
            &json!({"file_path": dir.join("x.rs").to_string_lossy()}),
        );
        assert_eq!(d, Decision::Allow);
        release("unassigned");
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "sightline-gate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let p = p.join(format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
