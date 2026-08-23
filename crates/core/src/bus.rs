//! The event stream: every transition Ironsight detects, as a record anything can
//! consume.
//!
//! Ironsight already computes each of these — a session going quiet, a prompt
//! appearing, a tool failing — and until now kept them to itself, so anything
//! wanting to supervise a fleet had to read a screen and guess. Publishing them
//! turns supervision into consumption.
//!
//! Two properties are load-bearing and everything here is arranged around them.
//!
//! The engine never waits on a consumer. A subscriber gets a bounded queue; one
//! that stops reading loses events and is told how many, rather than applying
//! back-pressure to the thing it is supposed to be watching. A monitor that
//! wedges the sessions it monitors is worse than no monitor.
//!
//! The vocabulary is a promise. `version` starts at 1. Fields are added, never
//! removed or repurposed; kinds are added, never renamed. Anything else bumps
//! the version, with both emitted for one release. This is the whole reason it
//! is safe to build a foreman on top of it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

/// Bumped only for a breaking change; consumers check it.
pub const VERSION: u32 = 1;

/// How many events a subscriber may fall behind before it starts losing them.
/// Large enough to absorb a busy tick, small enough that a dead consumer cannot
/// hold a meaningful amount of memory hostage.
pub const BACKLOG: usize = 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub version: u32,
    /// monotonic within a run; what `--since` replays from
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub session: String,
    pub agent: String,
    /// the session that started this one, when Ironsight started it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// the assignment this session was given, when it has one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub kind: Kind,
}

/// Who answered a permission prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "camelCase")]
pub enum By {
    Human,
    /// a named policy answered on the human's behalf; nothing does this yet
    Policy {
        name: String,
    },
}

/// Why a session is no longer running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ended {
    /// the process is gone
    Exited,
    /// Ironsight closed it, or was asked to
    Closed,
    /// it stopped being visible and Ironsight cannot say why
    Lost,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Kind {
    SessionStarted {
        cwd: String,
        branch: String,
    },
    SessionWorking {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
    },
    SessionWaiting,
    PermissionAsked {
        question: String,
        options: Vec<String>,
    },
    PermissionAnswered {
        option: String,
        #[serde(flatten)]
        by: By,
    },
    ToolCalled {
        tool: String,
        summary: String,
    },
    ToolFailed {
        tool: String,
        summary: String,
    },
    FileChanged {
        path: String,
        added: usize,
        removed: usize,
    },
    CommitCreated {
        sha: String,
        message: String,
        branch: String,
    },
    /// Emitted by verification, which is Phase 2. Named now so that consumers
    /// written against this version do not have to change when it arrives.
    ChecksPassed {
        suite: String,
        ms: u64,
    },
    ChecksFailed {
        suite: String,
        first: String,
    },
    SessionStalled {
        quiet_for: u64,
        no_files_for: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repeated: Option<String>,
    },
    SessionEnded {
        reason: Ended,
    },
    CostSpent {
        output: u64,
        estimate: f64,
    },
}

impl Kind {
    /// A short, stable name for filtering and display. Deliberately the same
    /// string the JSON carries, so a filter written against one works on both.
    pub fn name(&self) -> &'static str {
        match self {
            Kind::SessionStarted { .. } => "sessionStarted",
            Kind::SessionWorking { .. } => "sessionWorking",
            Kind::SessionWaiting => "sessionWaiting",
            Kind::PermissionAsked { .. } => "permissionAsked",
            Kind::PermissionAnswered { .. } => "permissionAnswered",
            Kind::ToolCalled { .. } => "toolCalled",
            Kind::ToolFailed { .. } => "toolFailed",
            Kind::FileChanged { .. } => "fileChanged",
            Kind::CommitCreated { .. } => "commitCreated",
            Kind::ChecksPassed { .. } => "checksPassed",
            Kind::ChecksFailed { .. } => "checksFailed",
            Kind::SessionStalled { .. } => "sessionStalled",
            Kind::SessionEnded { .. } => "sessionEnded",
            Kind::CostSpent { .. } => "costSpent",
        }
    }

    /// Worth interrupting a person for. Used by the foreman later; here so the
    /// judgement lives with the vocabulary rather than being restated.
    pub fn notable(&self) -> bool {
        matches!(
            self,
            Kind::PermissionAsked { .. }
                | Kind::ToolFailed { .. }
                | Kind::ChecksFailed { .. }
                | Kind::SessionStalled { .. }
        )
    }
}

impl Event {
    /// An event with the fields every one carries; `seq` is assigned by the bus
    /// at publication, because only the bus can order them.
    pub fn new(session: &str, agent: &str, kind: Kind) -> Self {
        Event {
            version: VERSION,
            seq: 0,
            at: Utc::now(),
            session: session.to_string(),
            agent: agent.to_string(),
            parent: None,
            task: None,
            kind,
        }
    }

    pub fn with_lineage(mut self, parent: Option<String>, task: Option<String>) -> Self {
        self.parent = parent;
        self.task = task;
        self
    }

    /// One line of JSON, which is the wire format for every transport.
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from("{}"))
    }

    /// One line for a person reading a terminal.
    pub fn human(&self) -> String {
        let when = self.at.format("%H:%M:%S");
        let who = if self.session.len() > 8 {
            &self.session[..8]
        } else {
            &self.session
        };
        let what = match &self.kind {
            Kind::SessionStarted { cwd, branch } => format!("started in {cwd} on {branch}"),
            Kind::SessionWorking { tool: Some(t) } => format!("working · {t}"),
            Kind::SessionWorking { tool: None } => "working".into(),
            Kind::SessionWaiting => "waiting on you".into(),
            Kind::PermissionAsked { question, .. } => format!("asks: {question}"),
            Kind::PermissionAnswered { option, by } => match by {
                By::Human => format!("answered {option}"),
                By::Policy { name } => format!("answered {option} by policy {name}"),
            },
            Kind::ToolCalled { tool, summary } => format!("{tool} {summary}"),
            Kind::ToolFailed { tool, summary } => format!("{tool} failed · {summary}"),
            Kind::FileChanged {
                path,
                added,
                removed,
            } => format!("{path} +{added}/-{removed}"),
            Kind::CommitCreated {
                sha,
                message,
                branch,
            } => {
                let short = if sha.len() > 7 { &sha[..7] } else { sha };
                format!("commit {short} on {branch} · {message}")
            }
            Kind::ChecksPassed { suite, ms } => format!("{suite} passed in {ms}ms"),
            Kind::ChecksFailed { suite, first } => format!("{suite} failed · {first}"),
            Kind::SessionStalled { quiet_for, .. } => format!("stalled · quiet for {quiet_for}s"),
            Kind::SessionEnded { reason } => format!("ended ({reason:?})"),
            Kind::CostSpent { output, estimate } => {
                format!("{output} output tokens · ${estimate:.4}")
            }
        };
        format!("{when} {:>4} {who} {what}", self.seq)
    }
}

/// One consumer's view of the stream.
///
/// Holds a bounded queue. If it is not drained the oldest events are lost and
/// counted, which is the trade this design exists to make: a consumer's slowness
/// is a consumer's problem.
pub struct Subscriber {
    rx: Receiver<Event>,
    lost: Arc<AtomicU64>,
}

impl Subscriber {
    /// Block until the next event, or `None` once the bus is gone.
    pub fn recv(&self) -> Option<Event> {
        self.rx.recv().ok()
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// The next event, or nothing within `wait`. What a consumer loop needs in
    /// order to notice it has been asked to stop.
    pub fn recv_timeout(&self, wait: std::time::Duration) -> Option<Event> {
        self.rx.recv_timeout(wait).ok()
    }

    /// Whether the bus still exists. A subscriber outlives it harmlessly, but a
    /// loop that does not check will spin on an empty channel.
    pub fn connected(&self) -> bool {
        !matches!(
            self.rx.recv_timeout(std::time::Duration::ZERO),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        )
    }

    /// Everything waiting, without blocking.
    pub fn drain(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }

    /// How many events this subscriber has missed by not keeping up.
    pub fn lost(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }
}

struct Sink {
    tx: SyncSender<Event>,
    lost: Arc<AtomicU64>,
}

/// Assigns order, records to the journal, and fans out to whoever is listening.
pub struct Bus {
    seq: u64,
    sinks: Vec<Sink>,
    journal: Option<Journal>,
}

impl Default for Bus {
    fn default() -> Self {
        Bus::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        Bus {
            seq: 0,
            sinks: Vec::new(),
            journal: None,
        }
    }

    /// Record every published event to a file as well, so a consumer that was
    /// not running can catch up.
    pub fn with_journal(mut self, journal: Journal) -> Self {
        self.seq = journal.last_seq;
        self.journal = Some(journal);
        self
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn subscribers(&self) -> usize {
        self.sinks.len()
    }

    pub fn subscribe(&mut self) -> Subscriber {
        self.subscribe_with(BACKLOG)
    }

    pub fn subscribe_with(&mut self, backlog: usize) -> Subscriber {
        let (tx, rx) = sync_channel(backlog.max(1));
        let lost = Arc::new(AtomicU64::new(0));
        self.sinks.push(Sink {
            tx,
            lost: Arc::clone(&lost),
        });
        Subscriber { rx, lost }
    }

    /// Order it, write it down, hand it out. Returns the sequence given, so a
    /// caller can say what it produced.
    pub fn publish(&mut self, mut ev: Event) -> u64 {
        self.seq += 1;
        ev.seq = self.seq;
        ev.version = VERSION;
        if let Some(j) = self.journal.as_mut() {
            j.append(&ev);
        }
        // A subscriber that has gone away is forgotten; one that is merely slow
        // loses this event and is told. Neither delays the caller.
        self.sinks.retain(|s| match s.tx.try_send(ev.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                s.lost.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
        self.seq
    }

    pub fn journal_path(&self) -> Option<&Path> {
        self.journal.as_ref().map(|j| j.path.as_path())
    }

    /// Events the journal could not write to disk. Zero unless something is
    /// wrong with the disk; non-zero is worth telling a person about.
    pub fn journal_dropped(&self) -> u64 {
        self.journal.as_ref().map_or(0, |j| j.dropped())
    }
}

/// The right to be the one process writing the journal.
///
/// The event socket already settled this on Unix: whoever bound it owned the
/// journal, and a second Ironsight found the address in use. Windows has no such
/// socket, so two processes both opened the journal, both numbered events from
/// their own counter, and `--since` quietly stopped meaning anything.
///
/// So the lock is the authority now, on every platform, and the socket is just
/// a transport the holder also opens. It is a pid file created atomically: the
/// first writer wins, and a file left by a process that has died is stolen
/// rather than honoured for ever — the pid inside is what decides, not the
/// file's mere existence.
pub struct PublisherLock {
    path: PathBuf,
}

impl PublisherLock {
    /// Take it, or say who has it.
    pub fn acquire(path: PathBuf) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // create_new is atomic: two processes racing here, exactly one wins.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                let _ = writeln!(f, "{}", std::process::id());
                Ok(PublisherLock { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| t.trim().parse::<u32>().ok());
                match holder {
                    // A living holder: it is theirs, and this process watches.
                    Some(pid) if pid_alive(pid) => {
                        Err(format!("another Ironsight (pid {pid}) is publishing"))
                    }
                    // Left by something that has gone. Steal it, and note that
                    // the stealing is itself racy — two processes both finding
                    // it stale would both try, so the create_new below keeps
                    // only one.
                    _ => {
                        let _ = std::fs::remove_file(&path);
                        match std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)
                        {
                            Ok(mut f) => {
                                let _ = writeln!(f, "{}", std::process::id());
                                Ok(PublisherLock { path })
                            }
                            Err(_) => Err("another Ironsight took the lock first".into()),
                        }
                    }
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

impl Drop for PublisherLock {
    fn drop(&mut self) {
        // Only if it is still ours. A stale-steal by someone else may have
        // replaced it, and removing theirs would be worse than leaving ours.
        if let Ok(text) = std::fs::read_to_string(&self.path) {
            if text.trim().parse::<u32>().ok() == Some(std::process::id()) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Whether a process is still around. Best-effort and platform-specific; a
/// false "alive" only means a stale lock is honoured a little longer, never
/// that two processes publish.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // Signal 0 checks for existence without delivering anything.
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    // No libc here; ask tasklist, and default to "alive" if it cannot be asked,
    // because honouring a maybe-live lock is the safe direction.
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(true)
}

#[cfg(not(any(unix, windows)))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// The append-only record of everything published.
///
/// One JSON object per line, which is the same shape the socket and the
/// `events` subcommand emit — one format, three transports. Capped and rotated
/// so a machine left running for a month does not fill a disk.
pub struct Journal {
    path: PathBuf,
    cap: u64,
    written: u64,
    last_seq: u64,
    file: Option<std::fs::File>,
    /// Events a write refused to accept — a full disk, a vanished directory.
    /// Kept so the loss is a number that can be surfaced rather than a silence.
    dropped: u64,
}

impl Journal {
    /// Default cap: 8MB, which is a few hundred thousand events. One previous
    /// file is kept, so replay can cross a rotation.
    pub const CAP: u64 = 8 * 1024 * 1024;

    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        Self::with_cap(path, Self::CAP)
    }

    pub fn with_cap(path: PathBuf, cap: u64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        // Continue the numbering rather than restarting it, so a consumer's
        // `--since` still means what it meant before Ironsight was restarted.
        let last_seq = last_seq_in(&path);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Journal {
            path,
            cap,
            written,
            last_seq,
            file: Some(file),
            dropped: 0,
        })
    }

    fn append(&mut self, ev: &Event) {
        let line = ev.line();
        if self.written + line.len() as u64 > self.cap {
            self.rotate();
        }
        match self.file.as_mut() {
            // A write that fails — the disk is full, the directory was removed —
            // must not vanish quietly. The event still reached every live
            // subscriber; what is lost is the durable copy, and that loss is now
            // a count something can read rather than a thing that never
            // happened.
            Some(f) => {
                if writeln!(f, "{line}").is_ok() {
                    self.written += line.len() as u64 + 1;
                    self.last_seq = ev.seq;
                } else {
                    self.dropped += 1;
                }
            }
            None => self.dropped += 1,
        }
    }

    /// Events the journal could not write down. Zero on a healthy disk.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    fn rotate(&mut self) {
        self.file = None;
        let old = self.path.with_extension("jsonl.1");
        let _ = std::fs::rename(&self.path, &old);
        self.written = 0;
        self.file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
    }
}

/// The highest sequence number a journal holds, so a restart continues rather
/// than repeating. Reads the tail only.
fn last_seq_in(path: &Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    text.lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Event>(l).ok())
        .map(|e| e.seq)
        .unwrap_or(0)
}

/// Everything recorded after `since`, oldest first, across one rotation.
///
/// A line that will not parse is skipped rather than fatal: a journal is
/// appended to by a running process and its last line may be half-written.
pub fn replay(path: &Path, since: u64) -> Vec<Event> {
    replay_from(path, since).events
}

/// What a replay could recover, and what it could not.
///
/// A consumer that stopped at sequence N and comes back asking for everything
/// after N may find that N has rotated off the end of the journal. Handing it
/// the events that survive and saying nothing lets it believe it caught up when
/// there is a hole in what it saw — which for a monitor is the difference
/// between "quiet" and "I missed the alarm".
#[derive(Debug, Clone)]
pub struct Replayed {
    pub events: Vec<Event>,
    /// How many events between what was asked for and what survived are gone.
    /// Zero when nothing was lost.
    pub missed: u64,
}

/// Everything recorded after `since`, oldest first, with the size of any gap.
pub fn replay_from(path: &Path, since: u64) -> Replayed {
    let mut events = Vec::new();
    let mut earliest: Option<u64> = None;
    for p in [path.with_extension("jsonl.1"), path.to_path_buf()] {
        let Ok(file) = std::fs::File::open(&p) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(ev) = serde_json::from_str::<Event>(&line) {
                earliest = Some(earliest.map_or(ev.seq, |e| e.min(ev.seq)));
                if ev.seq > since {
                    events.push(ev);
                }
            }
        }
    }
    events.sort_by_key(|e| e.seq);
    // If the oldest event still on disk is newer than the next one the consumer
    // expected, the ones in between are gone.
    let missed = match earliest {
        Some(first) if first > since + 1 => first - 1 - since,
        _ => 0,
    };
    Replayed { events, missed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: Kind) -> Event {
        Event::new("abc12345", "claude", kind)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ironsight-bus-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("events.jsonl")
    }

    #[test]
    fn only_one_process_may_hold_the_publisher_lock() {
        let dir = std::env::temp_dir().join("ironsight-lock-one");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("publisher.lock");

        let held = PublisherLock::acquire(path.clone()).expect("the first one gets it");
        // A second acquisition by *this* process still sees a live holder —
        // this process — so it is refused rather than granted twice.
        assert!(
            PublisherLock::acquire(path.clone()).is_err(),
            "a live lock is not handed out twice"
        );
        drop(held);
        // Once released, it is available again.
        assert!(
            PublisherLock::acquire(path.clone()).is_ok(),
            "a released lock can be taken"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_stolen() {
        let dir = std::env::temp_dir().join("ironsight-lock-stale");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("publisher.lock");

        // A pid that is not running. 0x7fffffff is above the pid_max on any
        // real system, so nothing holds it.
        std::fs::write(&path, "2147483647\n").unwrap();
        assert!(
            PublisherLock::acquire(path.clone()).is_ok(),
            "a lock whose holder has died is stolen, not honoured for ever"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lock_this_process_did_not_take_is_left_alone_on_drop() {
        let dir = std::env::temp_dir().join("ironsight-lock-foreign");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("publisher.lock");

        let held = PublisherLock::acquire(path.clone()).unwrap();
        // Someone else's pid appears in the file — a stale-steal by another
        // process. Dropping ours must not remove theirs.
        std::fs::write(&path, "2147483647\n").unwrap();
        drop(held);
        assert!(
            path.exists(),
            "another holder's lock is not removed by our drop"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn numbers_events_in_the_order_they_were_published() {
        let mut bus = Bus::new();
        let sub = bus.subscribe();
        bus.publish(ev(Kind::SessionWaiting));
        bus.publish(ev(Kind::ToolCalled {
            tool: "Bash".into(),
            summary: "ls".into(),
        }));
        let got = sub.drain();
        assert_eq!(
            got.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2],
            "sequence is assigned at publication, in order"
        );
        assert_eq!(got[1].kind.name(), "toolCalled");
        assert_eq!(sub.lost(), 0);
    }

    #[test]
    fn a_subscriber_that_stops_reading_loses_events_but_never_blocks() {
        let mut bus = Bus::new();
        let slow = bus.subscribe_with(2);
        let keeping_up = bus.subscribe_with(64);
        for _ in 0..10 {
            bus.publish(ev(Kind::SessionWaiting));
        }
        assert_eq!(
            slow.drain().len(),
            2,
            "the slow subscriber keeps only what fitted"
        );
        assert_eq!(slow.lost(), 8, "and is told exactly what it missed");
        assert_eq!(
            keeping_up.drain().len(),
            10,
            "one slow consumer costs the others nothing"
        );
    }

    #[test]
    fn forgets_a_subscriber_that_has_gone_away() {
        let mut bus = Bus::new();
        let gone = bus.subscribe();
        bus.publish(ev(Kind::SessionWaiting));
        drop(gone);
        bus.publish(ev(Kind::SessionWaiting));
        assert_eq!(bus.subscribers(), 0, "a dropped consumer is not retained");
    }

    #[test]
    fn survives_a_round_trip_through_json() {
        let original = ev(Kind::PermissionAnswered {
            option: "yes".into(),
            by: By::Policy {
                name: "trusted-reads".into(),
            },
        })
        .with_lineage(Some("parent".into()), Some("task-1".into()));
        let back: Event = serde_json::from_str(&original.line()).expect("an event parses back");
        assert_eq!(back, original);
        assert!(
            original.line().contains("\"type\":\"permissionAnswered\""),
            "the kind is a tagged field, so a consumer can filter without a schema"
        );
    }

    #[test]
    fn omits_lineage_it_does_not_have() {
        let line = ev(Kind::SessionWaiting).line();
        assert!(
            !line.contains("parent") && !line.contains("task"),
            "a session nobody started carries no empty lineage: {line}"
        );
    }

    #[test]
    fn replays_from_a_point_and_across_a_rotation() {
        let path = scratch("replay");
        let one = ev(Kind::ToolCalled {
            tool: "Bash".into(),
            summary: "echo hello".into(),
        });
        // Sized from a real line rather than a guessed byte count, so this
        // stays a test about crossing a rotation rather than about how long a
        // serialised event happens to be. Six events fill it; ten are
        // published, so it rotates exactly once and one previous file is
        // enough to hold everything.
        let cap = (one.line().len() as u64 + 1) * 6;
        let journal = Journal::with_cap(path.clone(), cap).unwrap();
        let mut bus = Bus::new().with_journal(journal);
        for _ in 0..10 {
            bus.publish(one.clone());
        }
        assert!(
            path.with_extension("jsonl.1").exists(),
            "the journal rotated rather than growing past its cap"
        );
        let all = replay(&path, 0);
        assert_eq!(
            all.iter().map(|e| e.seq).collect::<Vec<_>>(),
            (1..=10).collect::<Vec<_>>(),
            "replay crosses the rotation, in order"
        );
        let tail = replay(&path, 7);
        assert_eq!(
            tail.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![8, 9, 10],
            "a consumer restarting asks only for what it has not seen"
        );
    }

    #[test]
    fn keeps_one_previous_file_and_no_more() {
        let path = scratch("cap");
        let one = ev(Kind::SessionWaiting);
        let cap = (one.line().len() as u64 + 1) * 3;
        let mut bus = Bus::new().with_journal(Journal::with_cap(path.clone(), cap).unwrap());
        for _ in 0..30 {
            bus.publish(one.clone());
        }
        // The promise is a bounded disk footprint, not unbounded history: what
        // falls off the back is gone, and a consumer that asks for it is told
        // by the gap in sequence numbers rather than by silence.
        let kept = replay(&path, 0);
        assert!(
            kept.len() < 30 && !kept.is_empty(),
            "old events are discarded, recent ones are not: kept {}",
            kept.len()
        );
        assert_eq!(
            kept.last().map(|e| e.seq),
            Some(30),
            "the most recent event is always present"
        );
        let total: u64 = [path.clone(), path.with_extension("jsonl.1")]
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        assert!(
            total <= cap * 2,
            "the whole journal stays within two files of the cap: {total} bytes"
        );
    }

    #[test]
    fn a_consumer_is_told_when_history_it_asked_for_has_rotated_away() {
        let path = scratch("gap");
        let one = ev(Kind::SessionWaiting);
        // A cap small enough that early events roll off the back.
        let cap = (one.line().len() as u64 + 1) * 3;
        let mut bus = Bus::new().with_journal(Journal::with_cap(path.clone(), cap).unwrap());
        for _ in 0..30 {
            bus.publish(one.clone());
        }
        // A consumer that last saw event 2 asks for everything after it. Most
        // of 3..=30 has rotated away.
        let replayed = replay_from(&path, 2);
        assert!(replayed.missed > 0, "the gap is reported, not hidden");
        let first_seen = replayed.events.first().map(|e| e.seq).unwrap_or(0);
        assert_eq!(
            replayed.missed,
            first_seen - 1 - 2,
            "and its size is exactly the events between what was asked for and what survived"
        );

        // A consumer that is fully caught up is told of no gap.
        let current = replay_from(&path, 30);
        assert_eq!(current.missed, 0);
        assert!(current.events.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn continues_numbering_after_a_restart() {
        let path = scratch("restart");
        {
            let mut bus = Bus::new().with_journal(Journal::open(path.clone()).unwrap());
            bus.publish(ev(Kind::SessionWaiting));
            bus.publish(ev(Kind::SessionWaiting));
        }
        let mut again = Bus::new().with_journal(Journal::open(path.clone()).unwrap());
        let seq = again.publish(ev(Kind::SessionWaiting));
        assert_eq!(
            seq, 3,
            "a restart continues the numbering, so --since still means what it meant"
        );
    }

    #[test]
    fn events_the_journal_cannot_write_are_counted_not_swallowed() {
        let path = scratch("full-disk");
        let mut journal = Journal::open(path.clone()).unwrap();
        // Stand in for a disk that will not accept writes: no file handle, the
        // state a failed rotate leaves behind.
        journal.file = None;
        let before = journal.dropped();
        journal.append(&ev(Kind::SessionWaiting));
        journal.append(&ev(Kind::SessionWaiting));
        assert_eq!(
            journal.dropped(),
            before + 2,
            "two events could not be written down, and that is a number, not a silence"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_healthy_journal_drops_nothing() {
        let path = scratch("healthy");
        let mut bus = Bus::new().with_journal(Journal::open(path.clone()).unwrap());
        for _ in 0..20 {
            bus.publish(ev(Kind::SessionWaiting));
        }
        assert_eq!(
            bus.journal_dropped(),
            0,
            "nothing is lost when the disk is fine"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn skips_a_half_written_line() {
        let path = scratch("torn");
        {
            let mut bus = Bus::new().with_journal(Journal::open(path.clone()).unwrap());
            bus.publish(ev(Kind::SessionWaiting));
        }
        // What a journal looks like when it is read mid-append.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(f, "{{\"version\":1,\"seq\":2,\"at\":\"20").unwrap();
        drop(f);
        let all = replay(&path, 0);
        assert_eq!(all.len(), 1, "the torn tail is skipped, not fatal");
    }
}
