//! Turning what Sightline sees into what Sightline publishes.
//!
//! Nothing here detects anything new. Every event this produces comes from a
//! comparison Sightline was already making — a status that changed, a transcript
//! that grew, a file whose counters moved — and the only thing being added is
//! that the comparison is now written down where something else can read it.
//!
//! It is a separate module from `app` for one reason: it can be tested without
//! a machine. Feeding it a sequence of hand-written snapshots and asserting the
//! exact events that come out is a stronger check than anything that needs a
//! live session, and it is the check that will catch a change in Claude Code's
//! transcript format before a person notices a wrong number in the interface.
//!
//! Two rules it must not break.
//!
//! It never replays history. A session that has been running all day, first
//! seen when Sightline starts, produces no backlog: its cursor is seeded where it
//! stands. The stream is what happens from now on, and a consumer that wants
//! what came before reads the journal.
//!
//! It never guesses. A stall is reported when there has been no transcript
//! growth and no file activity for long enough to be unusual — not as a
//! judgement that the session is stuck, which from outside is indistinguishable
//! from thinking. Whoever consumes it decides what to do, and this phase
//! deliberately gives them nothing that restarts anything.

use crate::bus::{Ended, Event, Kind};
use crate::event::{Ev, Kind as EvKind};
use crate::session::{Session, Status};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// How long a working session may produce nothing before it is called stalled.
/// Long enough that ordinary thinking, a slow build or a large file read does
/// not trip it.
pub const STALL_AFTER: Duration = Duration::from_secs(300);

/// Everything the stream needs to know about one session at one moment.
///
/// A borrowed view rather than a copy: this runs on every tick, over every
/// session, and cloning a transcript's worth of events each time would be a
/// cost paid for nothing.
pub struct Snapshot<'a> {
    pub id: String,
    pub agent: String,
    pub cwd: String,
    pub branch: String,
    pub status: Status,
    /// events this session has ever pushed, including those aged out of the ring
    pub pushed: usize,
    pub events: &'a VecDeque<Ev>,
    pub dropped: usize,
    /// path, lines added, lines removed — cumulative, as the session counts them
    pub files: Vec<(String, usize, usize)>,
    pub output: u64,
    pub cost: f64,
    /// commit at the head of this session's directory, when it is a repository
    pub head: Option<Commit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub message: String,
    pub branch: String,
}

impl<'a> Snapshot<'a> {
    /// The view of a real session.
    ///
    /// `agent` is Claude Code unless the caller knows better, because a Session
    /// is read from Claude Code's transcript root and that is what put it
    /// there. A session Sightline started itself may be running something else
    /// entirely, and only the caller can see the pane that would say so — hence
    /// `with_agent`. It is emphatically not `agent_name`, which is the name the
    /// session was given by a person.
    ///
    /// `head` is filled in by the caller too, because asking git is not free
    /// and is not done on every tick.
    pub fn of(s: &'a Session) -> Self {
        Snapshot {
            id: s.id.clone(),
            agent: "claude".into(),
            cwd: s.cwd.clone(),
            branch: s.branch.clone(),
            status: s.status(),
            pushed: s.events.len() + s.dropped,
            events: &s.events,
            dropped: s.dropped,
            files: s
                .files
                .iter()
                .map(|(path, t)| (path.clone(), t.added, t.removed))
                .collect(),
            output: s.totals.output,
            cost: s.totals.cost,
            head: None,
        }
    }

    pub fn with_head(mut self, head: Option<Commit>) -> Self {
        self.head = head;
        self
    }

    pub fn with_agent(mut self, agent: String) -> Self {
        if !agent.is_empty() {
            self.agent = agent;
        }
        self
    }
}

fn state_of(status: &Status) -> &'static str {
    match status {
        Status::Running(_) | Status::Working => "working",
        Status::Waiting => "waiting",
        Status::Ended => "ended",
    }
}

/// Where one session was left last time it was looked at.
struct Cursor {
    consumed: usize,
    state: &'static str,
    files: HashMap<String, (usize, usize)>,
    output: u64,
    cost: f64,
    head: Option<String>,
    /// last moment this session produced anything at all
    active: Instant,
    /// whether a stall has already been reported, so it is said once
    stalled: bool,
}

/// Holds the last look at every session, and reports the difference.
pub struct Watcher {
    seen: HashMap<String, Cursor>,
    started: bool,
    stall_after: Duration,
}

impl Default for Watcher {
    fn default() -> Self {
        Watcher::new()
    }
}

impl Watcher {
    pub fn new() -> Self {
        Watcher {
            seen: HashMap::new(),
            started: false,
            stall_after: STALL_AFTER,
        }
    }

    /// For tests, and for anyone who finds five minutes wrong for their work.
    pub fn stall_after(mut self, after: Duration) -> Self {
        self.stall_after = after;
        self
    }

    pub fn watching(&self) -> usize {
        self.seen.len()
    }

    /// Everything that has changed since the last call.
    ///
    /// The first call is special: it announces the sessions that are alive,
    /// because a consumer connecting to a machine already at work needs to know
    /// what is on it, and seeds everything else silently so a day of history is
    /// not replayed at whoever just arrived.
    pub fn poll(&mut self, now: Instant, sessions: &[Snapshot<'_>]) -> Vec<Event> {
        let mut out = Vec::new();
        let first = !self.started;
        self.started = true;

        for snap in sessions {
            let known = self.seen.contains_key(&snap.id);
            if !known {
                let live = !matches!(snap.status, Status::Ended);
                if !first || live {
                    out.push(self.event(
                        snap,
                        Kind::SessionStarted {
                            cwd: snap.cwd.clone(),
                            branch: snap.branch.clone(),
                        },
                    ));
                }
                self.seen.insert(
                    snap.id.clone(),
                    Cursor {
                        consumed: snap.pushed,
                        state: state_of(&snap.status),
                        files: snap
                            .files
                            .iter()
                            .map(|(p, a, r)| (p.clone(), (*a, *r)))
                            .collect(),
                        output: snap.output,
                        cost: snap.cost,
                        head: snap.head.as_ref().map(|c| c.sha.clone()),
                        active: now,
                        stalled: false,
                    },
                );
                // A session first seen is not a session that has just done
                // something: everything it has already done is deliberately
                // not replayed.
                continue;
            }

            let mut produced = Vec::new();
            self.diff(snap, now, &mut produced);
            for kind in produced {
                out.push(self.event(snap, kind));
            }
        }

        // A session that has gone from the list has ended, whether it exited or
        // Sightline simply lost sight of it. Saying which would be a guess, so it
        // says it cannot tell.
        let present: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        let vanished: Vec<String> = self
            .seen
            .keys()
            .filter(|id| !present.contains(&id.as_str()))
            .cloned()
            .collect();
        for id in vanished {
            self.seen.remove(&id);
            let mut ev = Event::new(
                &id,
                "",
                Kind::SessionEnded {
                    reason: Ended::Lost,
                },
            );
            ev.agent = String::new();
            out.push(ev);
        }
        out
    }

    fn event(&self, snap: &Snapshot<'_>, kind: Kind) -> Event {
        Event::new(&snap.id, &snap.agent, kind)
    }

    /// Everything one session has done since it was last looked at, in the
    /// order it happened: what it ran, what it changed, what it spent, and only
    /// then what it became.
    fn diff(&mut self, snap: &Snapshot<'_>, now: Instant, out: &mut Vec<Kind>) {
        let Some(cursor) = self.seen.get_mut(&snap.id) else {
            return;
        };
        let mut moved = false;

        // Transcript. The ring may have dropped events between looks; taking
        // the later of the two bounds skips what is genuinely gone rather than
        // reporting the wrong records.
        if snap.pushed > cursor.consumed {
            let from = cursor.consumed.max(snap.dropped);
            for i in from..snap.pushed {
                let Some(ev) = snap.events.get(i - snap.dropped) else {
                    continue;
                };
                if let Some(kind) = tool_event(ev) {
                    out.push(kind);
                }
            }
            cursor.consumed = snap.pushed;
            moved = true;
        }

        // Files, by the amount each one moved rather than its running total.
        for (path, added, removed) in &snap.files {
            let (was_added, was_removed) = cursor.files.get(path).copied().unwrap_or((0, 0));
            if *added > was_added || *removed > was_removed {
                out.push(Kind::FileChanged {
                    path: path.clone(),
                    added: added.saturating_sub(was_added),
                    removed: removed.saturating_sub(was_removed),
                });
                cursor.files.insert(path.clone(), (*added, *removed));
                moved = true;
            }
        }

        // Commits.
        if let Some(head) = &snap.head {
            if cursor.head.as_deref() != Some(head.sha.as_str()) {
                if cursor.head.is_some() {
                    out.push(Kind::CommitCreated {
                        sha: head.sha.clone(),
                        message: crate::redact::text(&head.message),
                        branch: head.branch.clone(),
                    });
                    moved = true;
                }
                cursor.head = Some(head.sha.clone());
            }
        }

        // Spend, as the difference. A consumer adding these up gets the total,
        // and one that missed some is not told a wrong running figure.
        if snap.output > cursor.output {
            out.push(Kind::CostSpent {
                output: snap.output - cursor.output,
                estimate: (snap.cost - cursor.cost).max(0.0),
            });
            cursor.output = snap.output;
            cursor.cost = snap.cost;
            moved = true;
        } else if snap.output < cursor.output {
            // The totals went backwards, which a running session does not do —
            // its transcript was rewritten, or re-read from an earlier point.
            // Re-baseline silently rather than ignoring every future increment
            // until it climbs back above the old high-water mark, which would
            // lose all the spend in between. No event: nothing was actually
            // spent, the counter simply moved under us.
            cursor.output = snap.output;
            cursor.cost = snap.cost;
        }

        // Status.
        let state = state_of(&snap.status);
        if state != cursor.state {
            out.push(match &snap.status {
                Status::Running(tool) => Kind::SessionWorking {
                    tool: Some(tool.clone()),
                },
                Status::Working => Kind::SessionWorking { tool: None },
                Status::Waiting => Kind::SessionWaiting,
                Status::Ended => Kind::SessionEnded {
                    reason: Ended::Exited,
                },
            });
            cursor.state = state;
            moved = true;
        }

        // Stalls, last, so that anything the session did this tick has already
        // been reported and cleared the stall before it could be raised.
        if moved {
            cursor.active = now;
            cursor.stalled = false;
        } else if state == "working"
            && !cursor.stalled
            && now.duration_since(cursor.active) >= self.stall_after
        {
            cursor.stalled = true;
            let quiet = now.duration_since(cursor.active).as_secs();
            out.push(Kind::SessionStalled {
                quiet_for: quiet,
                no_files_for: quiet,
                repeated: None,
            });
        }
    }
}

/// A transcript event, if it is one the fleet cares about.
///
/// Prompts, replies and thinking are conversation: interesting to a person
/// reading one session, noise to something supervising twenty. Tool calls and
/// their failures are what a fleet is made of.
fn tool_event(ev: &Ev) -> Option<Kind> {
    let tool = ev.tool.clone().unwrap_or_else(|| "tool".into());
    // Redacted on the way out, and only on the way out. The interface still
    // shows the command as it was — it is your machine and you can already see
    // it. What is written to a file that outlives the session, and served on a
    // socket to whatever else is running, is not the place for a token.
    let said = || crate::redact::text(&crate::event::clip(&ev.head, 200));
    match ev.kind {
        EvKind::Tool => Some(Kind::ToolCalled {
            tool,
            summary: said(),
        }),
        EvKind::Result if !ev.ok => Some(Kind::ToolFailed {
            tool,
            summary: said(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: EvKind, tool: &str, head: &str, ok: bool) -> Ev {
        let mut e = Ev::new(None, kind, head.to_string(), head.to_string());
        e.tool = Some(tool.to_string());
        e.ok = ok;
        e
    }

    struct Fake {
        events: VecDeque<Ev>,
        files: Vec<(String, usize, usize)>,
        status: Status,
        output: u64,
        cost: f64,
        dropped: usize,
        head: Option<Commit>,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                events: VecDeque::new(),
                files: Vec::new(),
                status: Status::Waiting,
                output: 0,
                cost: 0.0,
                dropped: 0,
                head: None,
            }
        }
        fn snap(&self) -> Snapshot<'_> {
            Snapshot {
                id: "s1".into(),
                agent: "claude".into(),
                cwd: "/repo".into(),
                branch: "main".into(),
                status: self.status.clone(),
                pushed: self.events.len() + self.dropped,
                events: &self.events,
                dropped: self.dropped,
                files: self.files.clone(),
                output: self.output,
                cost: self.cost,
                head: self.head.clone(),
            }
        }
    }

    fn names(events: &[Event]) -> Vec<&'static str> {
        events.iter().map(|e| e.kind.name()).collect()
    }

    #[test]
    fn a_session_already_running_produces_no_backlog() {
        let mut f = Fake::new();
        f.status = Status::Working;
        f.events
            .push_back(ev(EvKind::Tool, "Bash", "cargo test", true));
        f.output = 5_000;
        f.files.push(("src/main.rs".into(), 40, 3));

        let mut w = Watcher::new();
        let out = w.poll(Instant::now(), &[f.snap()]);
        assert_eq!(
            names(&out),
            vec!["sessionStarted"],
            "the day it has already had is not replayed at whoever just connected"
        );
    }

    #[test]
    fn reports_what_a_session_does_in_the_order_it_happens() {
        let mut f = Fake::new();
        f.status = Status::Waiting;
        let mut w = Watcher::new();
        let t0 = Instant::now();
        w.poll(t0, &[f.snap()]);

        // A turn: a tool runs, a file changes, tokens are spent, and the
        // session goes from waiting to working.
        f.status = Status::Running("Edit".into());
        f.events
            .push_back(ev(EvKind::Tool, "Edit", "src/main.rs", true));
        f.files.push(("src/main.rs".into(), 12, 4));
        f.output = 900;
        f.cost = 0.02;

        let out = w.poll(t0 + Duration::from_secs(1), &[f.snap()]);
        assert_eq!(
            names(&out),
            vec!["toolCalled", "fileChanged", "costSpent", "sessionWorking"],
            "what it did comes before what it became"
        );
        assert!(matches!(
            &out[0].kind,
            Kind::ToolCalled { tool, .. } if tool == "Edit"
        ));
        assert!(matches!(
            &out[1].kind,
            Kind::FileChanged {
                added: 12,
                removed: 4,
                ..
            }
        ));
        assert!(matches!(&out[2].kind, Kind::CostSpent { output: 900, .. }));
        assert!(matches!(
            &out[3].kind,
            Kind::SessionWorking { tool: Some(t) } if t == "Edit"
        ));
    }

    #[test]
    fn spend_is_reported_as_the_difference_not_the_total() {
        let mut f = Fake::new();
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[f.snap()]);
        f.output = 100;
        f.cost = 0.01;
        w.poll(t, &[f.snap()]);
        f.output = 250;
        f.cost = 0.03;
        let out = w.poll(t, &[f.snap()]);
        match &out[0].kind {
            Kind::CostSpent { output, estimate } => {
                assert_eq!(*output, 150, "the second report is what was spent since");
                assert!((estimate - 0.02).abs() < 1e-9);
            }
            other => panic!("expected spend, got {other:?}"),
        }
    }

    #[test]
    fn a_credential_never_reaches_the_stream() {
        let mut f = Fake::new();
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[f.snap()]);
        f.events.push_back(ev(
            EvKind::Tool,
            "Bash",
            "GITHUB_TOKEN=ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8 gh pr create",
            true,
        ));
        let out = w.poll(t, &[f.snap()]);
        let Kind::ToolCalled { summary, .. } = &out[0].kind else {
            panic!("expected a call, got {:?}", out[0].kind);
        };
        assert!(
            !summary.contains("ghp_"),
            "a token went into the journal and onto the socket: {summary}"
        );
        assert!(
            summary.contains("gh pr create"),
            "and the command is still recognisable: {summary}"
        );
    }

    #[test]
    fn spend_is_not_lost_when_a_transcript_is_re_read_lower() {
        let mut f = Fake::new();
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[f.snap()]);

        f.output = 1000;
        f.cost = 0.10;
        w.poll(t, &[f.snap()]);

        // The transcript is rewritten and now reports less — a reset, not a
        // spend. Nothing should be reported, but the baseline must move.
        f.output = 200;
        f.cost = 0.02;
        assert!(
            w.poll(t, &[f.snap()]).is_empty(),
            "a counter going backwards is not spending"
        );

        // Fresh spend from the new baseline is reported in full, not swallowed
        // because it has not yet passed the old high-water mark of 1000.
        f.output = 500;
        f.cost = 0.05;
        let out = w.poll(t, &[f.snap()]);
        match out.iter().find_map(|e| match &e.kind {
            Kind::CostSpent { output, .. } => Some(*output),
            _ => None,
        }) {
            Some(spent) => assert_eq!(
                spent, 300,
                "spend after a reset is measured from the reset, not lost until it exceeds the old total"
            ),
            None => panic!("spend after a reset was swallowed entirely"),
        }
    }

    #[test]
    fn a_failed_tool_is_distinguished_from_one_that_ran() {
        let mut f = Fake::new();
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[f.snap()]);
        f.events
            .push_back(ev(EvKind::Tool, "Bash", "cargo build", true));
        f.events
            .push_back(ev(EvKind::Result, "Bash", "error: no such file", false));
        f.events.push_back(ev(EvKind::Result, "Bash", "ok", true));
        let out = w.poll(t, &[f.snap()]);
        assert_eq!(
            names(&out),
            vec!["toolCalled", "toolFailed"],
            "a result that succeeded is not news; one that failed is"
        );
    }

    #[test]
    fn conversation_is_not_a_fleet_event() {
        let mut f = Fake::new();
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[f.snap()]);
        for kind in [
            EvKind::Prompt,
            EvKind::Text,
            EvKind::Thinking,
            EvKind::System,
        ] {
            f.events.push_back(ev(kind, "", "some words", true));
        }
        let out = w.poll(t, &[f.snap()]);
        assert!(
            out.is_empty(),
            "what a person and an agent said to each other is not what a fleet is made of: {:?}",
            names(&out)
        );
    }

    #[test]
    fn survives_the_ring_dropping_events_between_looks() {
        let mut f = Fake::new();
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[f.snap()]);
        // Busy enough that the session's own buffer aged out what happened
        // before Sightline looked again.
        f.dropped = 500;
        f.events
            .push_back(ev(EvKind::Tool, "Read", "big.txt", true));
        let out = w.poll(t, &[f.snap()]);
        assert_eq!(
            names(&out),
            vec!["toolCalled"],
            "what is gone is skipped; what survives is reported once"
        );
    }

    #[test]
    fn a_working_session_that_goes_quiet_is_reported_once() {
        let mut f = Fake::new();
        f.status = Status::Working;
        let mut w = Watcher::new().stall_after(Duration::from_secs(60));
        let t0 = Instant::now();
        w.poll(t0, &[f.snap()]);

        assert!(
            w.poll(t0 + Duration::from_secs(30), &[f.snap()]).is_empty(),
            "thinking is not stalling"
        );
        let out = w.poll(t0 + Duration::from_secs(61), &[f.snap()]);
        assert_eq!(names(&out), vec!["sessionStalled"]);
        assert!(
            w.poll(t0 + Duration::from_secs(200), &[f.snap()])
                .is_empty(),
            "said once, not once a second, or it becomes the noise it exists to prevent"
        );

        // It comes back to life.
        f.events.push_back(ev(EvKind::Tool, "Bash", "make", true));
        let out = w.poll(t0 + Duration::from_secs(201), &[f.snap()]);
        assert_eq!(names(&out), vec!["toolCalled"]);
        // And can stall again, having genuinely worked in between.
        let out = w.poll(t0 + Duration::from_secs(300), &[f.snap()]);
        assert_eq!(names(&out), vec!["sessionStalled"]);
    }

    #[test]
    fn a_waiting_session_is_not_stalled() {
        let mut f = Fake::new();
        f.status = Status::Waiting;
        let mut w = Watcher::new().stall_after(Duration::from_secs(60));
        let t0 = Instant::now();
        w.poll(t0, &[f.snap()]);
        assert!(
            w.poll(t0 + Duration::from_secs(3_600), &[f.snap()])
                .is_empty(),
            "a session waiting on a person is doing exactly what it should"
        );
    }

    #[test]
    fn a_commit_is_reported_but_the_first_sighting_is_not() {
        let mut f = Fake::new();
        f.head = Some(Commit {
            sha: "aaaa111".into(),
            message: "first".into(),
            branch: "main".into(),
        });
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[f.snap()]);
        assert!(
            w.poll(t, &[f.snap()]).is_empty(),
            "the commit that was already there is not news"
        );
        f.head = Some(Commit {
            sha: "bbbb222".into(),
            message: "the session's work".into(),
            branch: "main".into(),
        });
        let out = w.poll(t, &[f.snap()]);
        assert_eq!(names(&out), vec!["commitCreated"]);
    }

    #[test]
    fn a_session_that_disappears_is_reported_as_ended() {
        let mut f = Fake::new();
        f.status = Status::Working;
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[f.snap()]);
        let out = w.poll(t, &[]);
        assert_eq!(names(&out), vec!["sessionEnded"]);
        assert!(
            matches!(
                out[0].kind,
                Kind::SessionEnded {
                    reason: Ended::Lost
                }
            ),
            "gone from the list is not the same as seen to exit, and it says so"
        );
        assert_eq!(w.watching(), 0);
    }

    #[test]
    fn a_session_that_starts_later_is_announced() {
        let f = Fake::new();
        let mut w = Watcher::new();
        let t = Instant::now();
        w.poll(t, &[]);
        let out = w.poll(t, &[f.snap()]);
        assert_eq!(names(&out), vec!["sessionStarted"]);
        match &out[0].kind {
            Kind::SessionStarted { cwd, branch } => {
                assert_eq!(cwd, "/repo");
                assert_eq!(branch, "main");
            }
            other => panic!("expected a start, got {other:?}"),
        }
    }
}
