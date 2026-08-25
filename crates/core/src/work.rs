//! What each session was asked to do, and which session asked it.
//!
//! Sessions are otherwise peers in a flat list: nothing records that one
//! started another to do part of its job, so there is no tree to supervise, no
//! way to attribute cost to a piece of work rather than to a process, and no
//! way to ask what a supervisor's workers are doing.
//!
//! The task record exists for a harder reason. Context does not transfer
//! between sessions — when one dies its understanding dies with it — so the
//! only thing that can survive is state written down explicitly. A task that
//! records what was asked, what must be true to be finished, and what has been
//! learned along the way is what lets a session be replaced rather than
//! mourned.
//!
//! `Claimed` and `Verified` are deliberately different states. An agent can
//! reach the first by saying so. Only evidence reaches the second, and
//! producing that evidence is Phase 2's job, not this one's.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum State {
    /// written down, not yet started
    Assigned,
    Working,
    /// stopped, and cannot proceed without something
    Blocked {
        why: String,
    },
    /// the agent says it is finished
    Claimed,
    /// the mechanical bar is met — it builds, the tests pass — and nothing has
    /// been shown to be wrong. This is not "done". A suite that passes says
    /// only that the failures it can express did not happen.
    Checked,
    /// something that would have shown the work to be wrong was tried, and did
    /// not show it. This state is never reached by passing checks alone.
    Verified,
    Abandoned,
}

impl State {
    pub fn label(&self) -> &'static str {
        match self {
            State::Assigned => "assigned",
            State::Working => "working",
            State::Blocked { .. } => "blocked",
            State::Claimed => "claimed",
            State::Checked => "checked",
            State::Verified => "verified",
            State::Abandoned => "abandoned",
        }
    }

    /// Whether this task still wants attention. An abandoned one does not, and
    /// neither does a verified one; everything else is live work — `Checked`
    /// included, because passing the checks is not finishing.
    pub fn open(&self) -> bool {
        !matches!(self, State::Verified | State::Abandoned)
    }
}

/// Something learned while doing the work, kept because the session that
/// learned it will not survive to be asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub at: DateTime<Utc>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// the session doing the work
    pub session: String,
    /// the session that assigned it; absent when a person did
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// what was asked, in words
    pub assignment: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    /// what must be true for this to be finished
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success: Vec<String>,
    /// when the worker must stop and ask rather than decide
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalate_if: Vec<String>,
    /// What would show this work to be wrong.
    ///
    /// Each is a command that must *fail*. A refutation that succeeds has
    /// demonstrated the defect it was written to find, and the claim is
    /// refused. A task with none of these can never be verified — only checked
    /// — because nobody has said what being wrong would look like, and a
    /// definition of done that cannot fail is not one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refutes: Vec<String>,
    /// Refutations that have been seen to fire at least once.
    ///
    /// A refutation nobody has ever watched catch anything is an unvalidated
    /// instrument. `sightline refute t1 "false"` cannot fire, stands for ever,
    /// and would otherwise verify anything — which is the same mistake as
    /// trusting a passing suite, one level further down.
    ///
    /// The honest workflow proves them for free: write the refutation while the
    /// defect is there, watch it fire and refuse the claim, fix the defect,
    /// watch it stand. Only then has anything been demonstrated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proven: Vec<String>,
    pub state: State,
    /// named suites that must pass; Phase 2 runs them
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    pub assigned_at: DateTime<Utc>,
}

impl Task {
    pub fn new(id: String, session: String, assignment: String) -> Self {
        Task {
            id,
            session,
            parent: None,
            assignment,
            constraints: Vec::new(),
            success: Vec::new(),
            escalate_if: Vec::new(),
            refutes: Vec::new(),
            proven: Vec::new(),
            state: State::Assigned,
            checks: Vec::new(),
            notes: Vec::new(),
            assigned_at: Utc::now(),
        }
    }

    /// One line: what it is and how it is going.
    pub fn summary(&self) -> String {
        format!("{} · {} · {}", self.id, self.state.label(), self.assignment)
    }
}

/// Cost attributed to one session, before any rolling up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub output: u64,
    pub estimate: f64,
}

impl std::ops::AddAssign for Cost {
    fn add_assign(&mut self, other: Self) {
        self.output += other.output;
        self.estimate += other.estimate;
    }
}

/// Tasks and lineage, held together because they are read together and are
/// written to one file.
#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    tasks: Vec<Task>,
    /// child session to the session that started it
    #[serde(default)]
    lineage: HashMap<String, String>,
    #[serde(default)]
    next: u64,
    #[serde(skip)]
    path: Option<PathBuf>,
    #[serde(skip)]
    dirty: bool,
    /// when the file was last known to change, so a store written by another
    /// process is noticed rather than ignored
    #[serde(skip)]
    stamp: Option<std::time::SystemTime>,
}

impl Store {
    pub fn new() -> Self {
        Store::default()
    }

    /// Read the store, or start an empty one. A store that will not parse is
    /// replaced rather than fatal — losing task history is bad, refusing to
    /// start is worse — but the unreadable file is kept beside it.
    pub fn load(path: PathBuf) -> Self {
        let mut store = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Store>(&text) {
                Ok(s) => s,
                Err(_) => {
                    let _ = std::fs::rename(&path, path.with_extension("json.unreadable"));
                    Store::default()
                }
            },
            Err(_) => Store::default(),
        };
        store.stamp = modified(&path);
        store.path = Some(path);
        store
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        // Written beside and renamed over: a crash mid-write leaves the old
        // store intact rather than a half-written one.
        let tmp = path.with_extension("json.writing");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        self.dirty = false;
        self.stamp = modified(&path);
        Ok(())
    }

    /// Write only if something changed. Called from the tick, so it must be
    /// cheap when nothing has happened.
    pub fn flush(&mut self) {
        if self.dirty {
            let _ = self.save();
        }
    }

    /// Pick up a store another process has written.
    ///
    /// `sightline assign` is a separate, short-lived process, so the Sightline
    /// holding the stream would otherwise stamp events with lineage it read at
    /// startup and never hear about an assignment made a minute ago.
    ///
    /// Anything of this process's own is written first, so its own work is not
    /// what gets discarded. Beyond that the last writer wins: two processes
    /// editing the same task in the same second is a race this does not try to
    /// resolve, because the alternative is a lock file and the cost of one is
    /// not yet justified by anything real.
    pub fn reload_if_stale(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.dirty {
            let _ = self.save();
            return;
        }
        let on_disk = modified(&path);
        if on_disk.is_some() && on_disk != self.stamp {
            *self = Store::load(path);
        }
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty() && self.lineage.is_empty()
    }

    /// Name something that would show a task's work to be wrong.
    pub fn refute_with(&mut self, id: &str, command: &str) -> Result<(), String> {
        let task = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("no task {id}"))?;
        task.refutes.push(command.to_string());
        self.dirty = true;
        Ok(())
    }

    /// Note that a refutation has been seen to catch something.
    pub fn proved(&mut self, id: &str, command: &str) {
        let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) else {
            return;
        };
        if !task.proven.iter().any(|p| p == command) {
            task.proven.push(command.to_string());
            self.dirty = true;
        }
    }

    /// Give a session an assignment, replacing whatever it had. Returns the id.
    pub fn assign(&mut self, session: &str, assignment: &str) -> String {
        self.next += 1;
        let id = format!("t{}", self.next);
        let mut task = Task::new(id.clone(), session.to_string(), assignment.to_string());
        task.parent = self.lineage.get(session).cloned();
        self.tasks
            .retain(|t| t.session != session || !t.state.open());
        self.tasks.push(task);
        self.dirty = true;
        id
    }

    /// The open task for a session, which is the one anything acting on that
    /// session cares about.
    pub fn task_for(&self, session: &str) -> Option<&Task> {
        self.tasks
            .iter()
            .rev()
            .find(|t| t.session == session && t.state.open())
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn set_state(&mut self, id: &str, state: State) -> Result<(), String> {
        let task = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("no task {id}"))?;
        task.state = state;
        self.dirty = true;
        Ok(())
    }

    /// Move a session's open task along, if it has one. Used by the tick, where
    /// having no task is the ordinary case rather than an error.
    pub fn advance(&mut self, session: &str, state: State) {
        let Some(task) = self
            .tasks
            .iter_mut()
            .rev()
            .find(|t| t.session == session && t.state.open())
        else {
            return;
        };
        if task.state != state {
            task.state = state;
            self.dirty = true;
        }
    }

    pub fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        let task = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("no task {id}"))?;
        task.notes.push(Note {
            at: Utc::now(),
            text: text.to_string(),
        });
        self.dirty = true;
        Ok(())
    }

    /// Record that `parent` started `child`. A session started by a person has
    /// no parent and is never recorded here.
    pub fn record_lineage(&mut self, child: &str, parent: &str) {
        if child == parent {
            return;
        }
        self.lineage.insert(child.to_string(), parent.to_string());
        if let Some(task) = self
            .tasks
            .iter_mut()
            .rev()
            .find(|t| t.session == child && t.state.open())
        {
            task.parent = Some(parent.to_string());
        }
        self.dirty = true;
    }

    pub fn parent_of(&self, session: &str) -> Option<&str> {
        self.lineage.get(session).map(String::as_str)
    }

    pub fn children_of(&self, session: &str) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .lineage
            .iter()
            .filter(|(_, parent)| parent.as_str() == session)
            .map(|(child, _)| child.as_str())
            .collect();
        out.sort_unstable();
        out
    }

    /// How deep a session sits under whoever started it, for indenting a list.
    /// A cycle — which should be impossible, but is cheap to survive — stops
    /// rather than hanging.
    pub fn depth_of(&self, session: &str) -> usize {
        let mut seen = HashSet::new();
        let mut at = session;
        let mut depth = 0;
        while let Some(parent) = self.lineage.get(at) {
            if !seen.insert(at.to_string()) {
                break;
            }
            depth += 1;
            at = parent;
        }
        depth
    }

    /// The session at the top of this one's tree, which is what cost is
    /// attributed to.
    pub fn root_of<'a>(&'a self, session: &'a str) -> &'a str {
        let mut seen = HashSet::new();
        let mut at = session;
        while let Some(parent) = self.lineage.get(at) {
            if !seen.insert(at.to_string()) {
                break;
            }
            at = parent.as_str();
        }
        at
    }

    /// Every session in the store's trees, parents before their children, so a
    /// front end can print the shape of the work without recursing itself.
    pub fn ordered(&self, known: &[String]) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        let mut roots: Vec<&String> = known
            .iter()
            .filter(|s| self.lineage.get(*s).is_none_or(|p| !known.contains(p)))
            .collect();
        roots.sort();
        for root in roots {
            self.walk(root, 0, known, &mut out, &mut HashSet::new());
        }
        out
    }

    fn walk(
        &self,
        at: &str,
        depth: usize,
        known: &[String],
        out: &mut Vec<(String, usize)>,
        seen: &mut HashSet<String>,
    ) {
        if !seen.insert(at.to_string()) {
            return;
        }
        out.push((at.to_string(), depth));
        for child in self.children_of(at) {
            if known.iter().any(|k| k == child) {
                self.walk(child, depth + 1, known, out, seen);
            }
        }
    }

    /// Cost with every descendant's cost added to its ancestors, so a
    /// supervisor's figure is what its workers actually spent.
    pub fn rollup(&self, own: &HashMap<String, Cost>) -> HashMap<String, Cost> {
        let mut out: HashMap<String, Cost> = own.clone();
        for (session, cost) in own {
            let mut seen = HashSet::new();
            let mut at = session.as_str();
            seen.insert(at.to_string());
            while let Some(parent) = self.lineage.get(at) {
                if !seen.insert(parent.clone()) {
                    break;
                }
                *out.entry(parent.clone()).or_default() += *cost;
                at = parent.as_str();
            }
        }
        out
    }

    /// Move every record from one session id to another.
    ///
    /// A session Sightline has just started is known only by the pane it is
    /// running in — it has no transcript yet, so it has no id of its own — and
    /// an assignment given at that moment is filed under `pane:%7`. Minutes
    /// later the session writes its first record and acquires a real id. Without
    /// this, its assignment and everything it was recorded as having started
    /// stay filed under a name nothing will ask about again.
    pub fn rekey(&mut self, from: &str, to: &str) {
        if from == to || !self.knows(from) {
            return;
        }
        for task in &mut self.tasks {
            if task.session == from {
                task.session = to.to_string();
            }
            if task.parent.as_deref() == Some(from) {
                task.parent = Some(to.to_string());
            }
        }
        let mut moved: HashMap<String, String> = HashMap::new();
        for (child, parent) in self.lineage.drain() {
            let child = if child == from { to.to_string() } else { child };
            let parent = if parent == from {
                to.to_string()
            } else {
                parent
            };
            moved.insert(child, parent);
        }
        self.lineage = moved;
        self.dirty = true;
    }

    /// A pane-keyed record whose handoff window has passed, so it should be
    /// forgotten rather than adopted by whatever later runs in that pane id.
    ///
    /// A `pane:%N` record exists only to carry an assignment across the minutes
    /// between starting a session and it acquiring a real id. tmux reuses pane
    /// ids — within a server as panes close and open, and certainly across a
    /// restart — so a record still filed under a pane long after it was made is
    /// a record whose session never came, and adopting it onto an unrelated
    /// future session would put someone else's assignment on it.
    pub fn stale_pane_record(&self, pane_key: &str, older_than_secs: i64) -> bool {
        if !pane_key.starts_with("pane:") {
            return false;
        }
        let now = Utc::now();
        // The most recent thing filed under this pane. If even that is old, the
        // handoff is not going to happen.
        let newest = self
            .tasks
            .iter()
            .filter(|t| t.session == pane_key)
            .map(|t| t.assigned_at)
            .max();
        match newest {
            Some(at) => (now - at).num_seconds() > older_than_secs,
            // Lineage with no task attached carries no time of its own; it is
            // made alongside a task, so its absence means the task already
            // moved on and this is debris.
            None => self.lineage.contains_key(pane_key),
        }
    }

    /// Whether this session appears anywhere in the store.
    pub fn knows(&self, session: &str) -> bool {
        self.tasks
            .iter()
            .any(|t| t.session == session || t.parent.as_deref() == Some(session))
            || self.lineage.contains_key(session)
            || self.lineage.values().any(|p| p == session)
    }

    /// Drop every record filed under a pane key. Used when its handoff window
    /// has passed without the session arriving.
    pub fn forget_pane_record(&mut self, pane_key: &str) {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.session != pane_key);
        let had = self.lineage.remove(pane_key).is_some();
        if self.tasks.len() != before || had {
            self.dirty = true;
        }
    }

    /// Forget a session's lineage when it is gone for good. Its task stays: a
    /// record of what was asked outlives the session that was asked, which is
    /// the entire point of writing it down.
    pub fn forget_session(&mut self, session: &str) {
        if self.lineage.remove(session).is_some() {
            self.dirty = true;
        }
    }
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Where the store lives. Beside the event journal, because they are two halves
/// of the same record.
pub fn default_path() -> PathBuf {
    crate::app::data_dir().join("work.json")
}

pub fn path_in(dir: &Path) -> PathBuf {
    dir.join("work.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sightline-work-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("work.json")
    }

    /// A chief with two workers, one of which started a worker of its own.
    fn fleet() -> Store {
        let mut s = Store::new();
        s.record_lineage("worker-a", "chief");
        s.record_lineage("worker-b", "chief");
        s.record_lineage("helper", "worker-a");
        s
    }

    #[test]
    fn an_assignment_records_who_asked() {
        let mut s = fleet();
        let id = s.assign("worker-a", "implement the OAuth callback");
        let task = s.get(&id).expect("the task exists");
        assert_eq!(
            task.parent.as_deref(),
            Some("chief"),
            "lineage is picked up"
        );
        assert_eq!(task.state, State::Assigned);
        assert_eq!(
            s.task_for("worker-a").map(|t| t.id.as_str()),
            Some(id.as_str())
        );
    }

    #[test]
    fn a_session_a_person_started_has_no_parent() {
        let mut s = Store::new();
        let id = s.assign("alone", "do the thing");
        assert_eq!(s.get(&id).unwrap().parent, None);
        assert_eq!(s.depth_of("alone"), 0);
    }

    #[test]
    fn builds_a_tree_parents_before_children() {
        let s = fleet();
        let known: Vec<String> = ["chief", "worker-a", "worker-b", "helper"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            s.ordered(&known),
            vec![
                ("chief".into(), 0),
                ("worker-a".into(), 1),
                ("helper".into(), 2),
                ("worker-b".into(), 1),
            ],
            "the list is the shape of the work, not a list of processes"
        );
        assert_eq!(s.depth_of("helper"), 2);
        assert_eq!(s.root_of("helper"), "chief");
    }

    #[test]
    fn a_session_whose_parent_is_gone_reads_as_a_root() {
        let s = fleet();
        // The chief has been closed; its workers are still running.
        let known: Vec<String> = ["worker-a", "helper"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            s.ordered(&known),
            vec![("worker-a".into(), 0), ("helper".into(), 1)],
            "indentation is relative to what is on screen: a child indented \
             under a parent nobody can see is a dangling indent"
        );
        assert_eq!(
            s.depth_of("worker-a"),
            1,
            "its real depth is unchanged — the chief existed, and the record says so"
        );
    }

    #[test]
    fn cost_rolls_up_the_tree() {
        let s = fleet();
        let own = HashMap::from([
            (
                "chief".to_string(),
                Cost {
                    output: 100,
                    estimate: 0.10,
                },
            ),
            (
                "worker-a".to_string(),
                Cost {
                    output: 200,
                    estimate: 0.20,
                },
            ),
            (
                "helper".to_string(),
                Cost {
                    output: 50,
                    estimate: 0.05,
                },
            ),
        ]);
        let total = s.rollup(&own);
        assert_eq!(total["helper"].output, 50, "a leaf is only itself");
        assert_eq!(
            total["worker-a"].output, 250,
            "a session carries what it started"
        );
        assert_eq!(
            total["chief"].output, 350,
            "and the top carries everything below it"
        );
        assert!((total["chief"].estimate - 0.35).abs() < 1e-9);
    }

    #[test]
    fn a_cycle_cannot_hang_the_rollup() {
        // Impossible through the API, but a hand-edited store is a file like
        // any other, and a monitor that hangs is worse than one that is wrong.
        let mut s = Store::new();
        s.lineage.insert("a".into(), "b".into());
        s.lineage.insert("b".into(), "a".into());
        let own = HashMap::from([(
            "a".to_string(),
            Cost {
                output: 10,
                estimate: 0.01,
            },
        )]);
        let total = s.rollup(&own);
        assert_eq!(total["b"].output, 10);
        // Reaching these assertions at all is the test: every walk terminates.
        // The depth of a session inside a cycle is not a meaningful number, so
        // this only asserts it is bounded rather than pinning a value.
        assert!(s.depth_of("a") <= 2, "the walk stops rather than spinning");
        assert!(
            matches!(s.root_of("a"), "a" | "b"),
            "a cycle has no root; the walk answers with one of its members and returns"
        );
    }

    #[test]
    fn survives_being_written_and_read_back() {
        let path = scratch("persist");
        let id = {
            let mut s = Store::load(path.clone());
            s.record_lineage("worker", "chief");
            let id = s.assign("worker", "write the parser");
            s.advance("worker", State::Working);
            s.note(&id, "the grammar is ambiguous around unary minus")
                .unwrap();
            s.save().expect("the store writes");
            id
        };
        let mut back = Store::load(path);
        let task = back.get(&id).expect("the task came back");
        assert_eq!(task.state, State::Working);
        assert_eq!(task.notes.len(), 1, "what was learned survives the session");
        assert_eq!(back.parent_of("worker"), Some("chief"));
        assert_eq!(
            back.assign("other", "something else"),
            "t2",
            "ids continue rather than colliding with one already handed out"
        );
    }

    #[test]
    fn an_abandoned_task_stays_readable() {
        let mut s = Store::new();
        let first = s.assign("worker", "the original plan");
        s.set_state(&first, State::Abandoned).unwrap();
        let second = s.assign("worker", "what we did instead");
        assert_eq!(
            s.task_for("worker").map(|t| t.id.as_str()),
            Some(second.as_str()),
            "the open task is the current one"
        );
        assert!(
            s.get(&first).is_some(),
            "and the abandoned one is still there to read"
        );
    }

    #[test]
    fn claiming_is_not_checking_and_checking_is_not_verifying() {
        let mut s = Store::new();
        let id = s.assign("worker", "fix the bug");

        s.advance("worker", State::Claimed);
        assert!(
            s.get(&id).unwrap().state.open(),
            "an agent saying it is finished does not finish it"
        );

        // The checks passed. That is a floor, not a finish: it says the
        // failures the suite can express did not happen, and nothing else.
        s.advance("worker", State::Checked);
        assert!(
            s.get(&id).unwrap().state.open(),
            "passing the checks leaves the task open — a suite cannot say the work is right"
        );

        s.set_state(&id, State::Verified).unwrap();
        assert!(!s.get(&id).unwrap().state.open());
    }

    #[test]
    fn a_task_that_cannot_be_refuted_cannot_be_verified() {
        let mut s = Store::new();
        let id = s.assign("worker", "make the guard fire");
        assert!(
            s.get(&id).unwrap().refutes.is_empty(),
            "nothing has been said about what being wrong would look like"
        );
        s.refute_with(&id, "test ! -x ./guard-fires.sh || ./guard-fires.sh")
            .unwrap();
        assert_eq!(s.get(&id).unwrap().refutes.len(), 1);
    }

    #[test]
    fn picks_up_an_assignment_made_by_another_process() {
        let path = scratch("shared");
        let mut watching = Store::load(path.clone());
        watching.save().unwrap();
        assert!(watching.task_for("worker").is_none());

        // What `sightline assign` does, in a process of its own.
        {
            let mut elsewhere = Store::load(path.clone());
            elsewhere.assign("worker", "something asked for while Sightline was running");
            elsewhere.save().unwrap();
        }

        watching.reload_if_stale();
        assert_eq!(
            watching.task_for("worker").map(|t| t.assignment.as_str()),
            Some("something asked for while Sightline was running"),
            "an assignment made beside a running Sightline reaches the stream it stamps"
        );
    }

    #[test]
    fn its_own_work_is_written_rather_than_discarded() {
        let path = scratch("own");
        let mut mine = Store::load(path.clone());
        let id = mine.assign("worker", "mine");
        // Not yet saved, and the file has changed underneath.
        {
            let mut other = Store::load(path.clone());
            other.assign("someone-else", "theirs");
            other.save().unwrap();
        }
        mine.reload_if_stale();
        assert!(
            mine.get(&id).is_some(),
            "a reload never throws away work this process has not written yet"
        );
    }

    #[test]
    fn a_stale_pane_record_is_not_adopted_by_a_reused_pane_id() {
        let mut s = Store::new();
        let id = s.assign("pane:%7", "the session that never arrived");

        // Fresh: still within its handoff window, so it is adopted normally.
        assert!(!s.stale_pane_record("pane:%7", 600));

        // Backdate the assignment past the window, as if the session died
        // before writing a transcript and tmux later reused %7.
        s.tasks.iter_mut().find(|t| t.id == id).unwrap().assigned_at =
            Utc::now() - chrono::Duration::seconds(3600);
        assert!(
            s.stale_pane_record("pane:%7", 600),
            "an hour-old pane record is debris, not a handoff"
        );

        s.forget_pane_record("pane:%7");
        assert!(
            !s.knows("pane:%7"),
            "and dropping it leaves nothing to misadopt"
        );
    }

    #[test]
    fn a_real_session_id_is_never_stale_by_this_rule() {
        let mut s = Store::new();
        s.assign("9f2c-real-id", "ordinary work");
        assert!(
            !s.stale_pane_record("9f2c-real-id", 0),
            "the rule only applies to pane-keyed records"
        );
    }

    #[test]
    fn a_session_keeps_its_work_when_it_acquires_a_real_name() {
        let mut s = Store::new();
        // What Sightline knows at the moment it starts a session: a pane, and
        // nothing else.
        s.record_lineage("pane:%7", "chief");
        let id = s.assign("pane:%7", "write the parser");
        s.record_lineage("pane:%9", "pane:%7");

        // The session writes its first record and becomes itself.
        s.rekey("pane:%7", "9f2c-real");

        assert_eq!(
            s.get(&id).map(|t| t.session.as_str()),
            Some("9f2c-real"),
            "the assignment follows the session"
        );
        assert_eq!(
            s.parent_of("9f2c-real"),
            Some("chief"),
            "and so does who started it"
        );
        assert_eq!(
            s.parent_of("pane:%9"),
            Some("9f2c-real"),
            "as does its own place as a parent"
        );
        assert!(!s.knows("pane:%7"), "nothing is left under the old name");
    }

    #[test]
    fn rekeying_a_session_it_has_never_heard_of_does_nothing() {
        let mut s = Store::new();
        s.assign("worker", "the job");
        s.rekey("pane:%3", "worker");
        assert_eq!(s.tasks().len(), 1);
        assert_eq!(
            s.task_for("worker").map(|t| t.assignment.as_str()),
            Some("the job")
        );
    }

    #[test]
    fn a_refutation_counts_as_evidence_only_once_it_has_caught_something() {
        let mut s = Store::new();
        let id = s.assign("worker", "make the guard fire");
        s.refute_with(&id, "./guard-lets-it-through.sh").unwrap();

        let task = s.get(&id).unwrap();
        assert!(
            task.proven.is_empty(),
            "a refutation nobody has watched catch anything has demonstrated nothing"
        );

        // It fires — the defect is there. That is what proves the instrument.
        s.proved(&id, "./guard-lets-it-through.sh");
        assert_eq!(s.get(&id).unwrap().proven.len(), 1);

        // Twice is still once: it is a fact about the refutation, not a count.
        s.proved(&id, "./guard-lets-it-through.sh");
        assert_eq!(s.get(&id).unwrap().proven.len(), 1);
    }

    #[test]
    fn an_unreadable_store_is_set_aside_rather_than_fatal() {
        let path = scratch("corrupt");
        std::fs::write(&path, "{ this is not json").unwrap();
        let s = Store::load(path.clone());
        assert!(s.is_empty(), "Sightline still starts");
        assert!(
            path.with_extension("json.unreadable").exists(),
            "and what could not be read is kept, not deleted"
        );
    }
}

/// The shape of a supervised project: who was asked to do what, by whom.
///
/// A chief and its workers are separate sessions and appear in the list as
/// separate rows, which is accurate and is not how anyone thinks about the work.
/// A person hands over a project; what comes back should be the project, with
/// the sessions inside it, rather than six unrelated rows they have to hold in
/// their head as a tree.
///
/// Assembled here rather than in a front end because it is a reading of the
/// task store — which parent, which state, what is still open — and both front
/// ends want the same reading. The layout is not decided here: where a node sits
/// on screen is a question about the screen.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Node {
    pub task: String,
    pub session: String,
    /// How deep below the chief. The chief itself is 0.
    pub depth: usize,
    pub assignment: String,
    pub state: String,
    /// Whether this is still work in progress rather than a finished branch.
    pub open: bool,
    pub notes: usize,
    /// How many things have been written to show this work wrong, and how many
    /// of those have ever actually fired. A refutation nobody has seen catch
    /// anything has proved nothing, so the second number is the one that counts.
    pub refutes: usize,
    pub proven: usize,
    /// The task that asked for this one, if any.
    pub from: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Chart {
    /// The task at the root — the `supervise:` one a person handed over.
    pub root: Option<String>,
    /// What the person actually asked for, with the `supervise:` prefix removed.
    pub intent: String,
    pub nodes: Vec<Node>,
    /// Counts by state, for saying how it is going in one line.
    pub open: usize,
    pub done: usize,
}

impl Store {
    /// Everything descending from one session's task, including its own.
    ///
    /// Walks down rather than up: the chief is the root, and a worker's task
    /// names the session that assigned it. A task whose parent is not in the
    /// set is not part of this project, which is what keeps two chiefs running
    /// at once from appearing as one tangle.
    pub fn chart(&self, chief: &str) -> Chart {
        // Deliberately not `task_for`, which returns only *open* tasks. A
        // finished worker has to stay in the picture: this is a diagram of how
        // the work was distributed, and one where branches disappear as they
        // succeed shows the opposite of what happened. It reads worst exactly
        // when a project is going well.
        let latest = |session: &str| {
            self.tasks()
                .iter()
                .rev()
                .find(|t| t.session == session && t.state.open())
                .or_else(|| self.tasks().iter().rev().find(|t| t.session == session))
        };
        let root = latest(chief);
        let intent = root
            .map(|t| {
                t.assignment
                    .strip_prefix("supervise:")
                    .unwrap_or(&t.assignment)
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();

        let mut nodes: Vec<Node> = Vec::new();
        // Breadth first, so depth is the number of hops from the chief and a
        // cycle — which should not exist and would hang a naive walk — cannot
        // be entered twice.
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        let mut frontier: Vec<(String, usize, Option<String>)> = vec![(chief.to_string(), 0, None)];
        while let Some((session, depth, from)) = frontier.pop() {
            if !seen.insert(session.clone()) {
                continue;
            }
            if let Some(task) = latest(&session) {
                nodes.push(Node {
                    task: task.id.clone(),
                    session: task.session.clone(),
                    depth,
                    assignment: task.assignment.clone(),
                    state: task.state.label().to_string(),
                    open: task.state.open(),
                    notes: task.notes.len(),
                    refutes: task.refutes.len(),
                    proven: task.proven.len(),
                    from: from.clone(),
                });
            } else if depth == 0 {
                // A chief with no task of its own still has a project.
                //
                // Found by running the real thing: `start_chief` in the window
                // writes a `supervise:` task, and a chief started any other way
                // does not — so the walk began at a node that did not exist and
                // the entire project vanished, workers and all. The root is the
                // session, not the record of it.
                nodes.push(Node {
                    task: String::new(),
                    session: session.clone(),
                    depth: 0,
                    assignment: String::new(),
                    state: "supervising".to_string(),
                    open: true,
                    notes: 0,
                    refutes: 0,
                    proven: 0,
                    from: None,
                });
            }
            for task in self.tasks() {
                if task.parent.as_deref() == Some(session.as_str()) {
                    frontier.push((task.session.clone(), depth + 1, Some(session.clone())));
                }
            }
        }
        nodes.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.task.cmp(&b.task)));
        let open = nodes.iter().filter(|n| n.open && n.depth > 0).count();
        let done = nodes.iter().filter(|n| !n.open && n.depth > 0).count();
        Chart {
            root: root.map(|t| t.id.clone()),
            intent,
            nodes,
            open,
            done,
        }
    }
}

#[cfg(test)]
mod chart_tests {
    use super::*;

    fn store() -> Store {
        let mut s = Store::new();
        let chief = s.assign("chief-1", "supervise: build me a GUI btop");
        let _ = chief;
        s
    }

    #[test]
    fn a_project_is_its_chief_and_everything_below_it() {
        let mut s = store();
        s.assign("worker-a", "the backend");
        s.record_lineage("worker-a", "chief-1");
        s.assign("worker-b", "the panels");
        s.record_lineage("worker-b", "chief-1");

        let chart = s.chart("chief-1");
        assert_eq!(
            chart.intent, "build me a GUI btop",
            "the prefix is Sightline's, not the person's"
        );
        assert_eq!(chart.nodes.len(), 3);
        assert_eq!(chart.nodes[0].depth, 0, "the chief is the root");
        assert!(chart.nodes[1..].iter().all(|n| n.depth == 1));
        assert!(
            chart.nodes[1..]
                .iter()
                .all(|n| n.from.as_deref() == Some("chief-1"))
        );
    }

    #[test]
    fn two_chiefs_at_once_are_two_projects_and_not_one_tangle() {
        // The reason this walks down from a root rather than collecting every
        // task: a machine running two supervised projects would otherwise show
        // both as a single diagram with no root, which is worse than no diagram.
        let mut s = store();
        s.assign("worker-a", "the backend");
        s.record_lineage("worker-a", "chief-1");
        s.assign("chief-2", "supervise: something else entirely");
        s.assign("worker-z", "unrelated work");
        s.record_lineage("worker-z", "chief-2");

        let first = s.chart("chief-1");
        assert_eq!(first.nodes.len(), 2);
        assert!(first.nodes.iter().all(|n| n.session != "worker-z"));
        let second = s.chart("chief-2");
        assert_eq!(second.nodes.len(), 2);
        assert!(second.nodes.iter().all(|n| n.session != "worker-a"));
    }

    #[test]
    fn a_chief_with_no_task_of_its_own_still_has_a_project() {
        // The window's `start_chief` writes a `supervise:` task; a chief started
        // any other way — the live example, a future front end — does not. The
        // walk then began at a node that did not exist and the whole project
        // vanished, workers included. Caught by running the real thing, which is
        // the only place this shape occurs.
        let mut s = Store::new();
        s.assign("worker-a", "add the numbers up");
        s.record_lineage("worker-a", "chief-1");

        let chart = s.chart("chief-1");
        assert_eq!(
            chart.nodes.len(),
            2,
            "the root is the session, not the record of it"
        );
        assert_eq!(chart.nodes[0].session, "chief-1");
        assert_eq!(chart.nodes[0].state, "supervising");
        assert_eq!(chart.nodes[1].from.as_deref(), Some("chief-1"));
    }

    #[test]
    fn a_cycle_does_not_hang_it() {
        // Lineage is written by the kernel and should never loop. "Should never"
        // is not a reason to walk a graph without a visited set: this is the
        // difference between a bug and a window that stops responding.
        let mut s = store();
        s.assign("worker-a", "one");
        s.record_lineage("worker-a", "chief-1");
        s.record_lineage("chief-1", "worker-a");

        let chart = s.chart("chief-1");
        assert_eq!(chart.nodes.len(), 2);
    }

    #[test]
    fn open_and_done_count_the_work_and_not_the_supervision() {
        let mut s = store();
        let a = s.assign("worker-a", "one");
        s.assign("worker-b", "two");
        s.record_lineage("worker-a", "chief-1");
        s.record_lineage("worker-b", "chief-1");
        s.set_state(&a, State::Verified).unwrap();

        let chart = s.chart("chief-1");
        assert_eq!(chart.done, 1);
        assert_eq!(
            chart.open, 1,
            "the chief's own task is supervision, not work"
        );
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn a_task_assigned_before_a_session_names_itself_must_not_be_orphaned() {
        // An owned session has two identities. Until Claude Code reports a
        // session id it is known by Sightline's own handle — `owned-2` — and
        // afterwards the rest of the application keys it by the uuid. The
        // kernel writes a worker's task at the moment it starts it, which is
        // before the uuid exists, so the task is written under the handle.
        //
        // Nothing rekeys an owned session: `App::rekey_panes` walks `steer`,
        // which holds tmux panes, and an owned session is not in it. So this
        // test says what happens when the two identities meet.
        let mut s = Store::new();
        s.assign("owned-1", "supervise: build the thing");
        s.assign("owned-2", "the backend");
        s.record_lineage("owned-2", "owned-1");

        // The session ids arrive.
        s.rekey("owned-1", "11111111-chief");
        s.rekey("owned-2", "22222222-worker");

        assert!(
            s.task_for("22222222-worker").is_some(),
            "a task written under the handle has to survive the session naming itself"
        );
        let chart = s.chart("11111111-chief");
        assert_eq!(
            chart.nodes.len(),
            2,
            "and the lineage has to be carried across with it, or the project loses its shape"
        );
        assert_eq!(chart.nodes[1].from.as_deref(), Some("11111111-chief"));
    }
}
