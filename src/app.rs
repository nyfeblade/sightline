//! Discovery, refresh, and view state.

use crate::control::{self, Approval, Pane};
use crate::event::{Ev, Filter};
use crate::git;
use crate::notify;
use crate::registry;
use crate::session::{Session, Status};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// What a typed line will do when it is submitted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    Send,
    Broadcast,
    NewSession,
    /// hold the message until the session goes idle
    Queue,
    Search,
    /// start a session on its own branch in its own checkout
    Isolate,
    Merge,
    Discard,
    Stop,
    Adopt,
}

/// A session working in its own checkout.
#[derive(Clone)]
pub struct Iso {
    pub repo: PathBuf,
    pub branch: String,
    pub base: String,
    pub ahead: usize,
}

/// One thing you can do to the selected session, with the reason when you
/// cannot. Shown in the actions menu so nothing has to be memorised.
pub struct Action {
    pub key: char,
    pub label: &'static str,
    pub enabled: bool,
    pub why: String,
}

pub struct Input {
    pub kind: Prompt,
    pub label: String,
    /// the session this line was opened for; the selection may move under it
    pub target: Option<String>,
    pub buf: String,
    /// caret position, counted in characters
    pub pos: usize,
}

impl Input {
    fn byte_at(&self, chars: usize) -> usize {
        self.buf
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(self.buf.len())
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.pos);
        self.buf.insert(at, c);
        self.pos += 1;
    }

    pub fn backspace(&mut self) {
        if self.pos == 0 {
            return;
        }
        let at = self.byte_at(self.pos - 1);
        self.buf.remove(at);
        self.pos -= 1;
    }

    pub fn delete(&mut self) {
        if self.pos < self.buf.chars().count() {
            let at = self.byte_at(self.pos);
            self.buf.remove(at);
        }
    }

    pub fn left(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.pos = (self.pos + 1).min(self.buf.chars().count());
    }

    pub fn home(&mut self) {
        self.pos = 0;
    }

    pub fn end(&mut self) {
        self.pos = self.buf.chars().count();
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.pos = 0;
    }

    /// Delete the word before the caret, as ctrl+w does in a shell.
    pub fn delete_word(&mut self) {
        let chars: Vec<char> = self.buf.chars().collect();
        let mut i = self.pos;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let mut chars = chars;
        chars.drain(i..self.pos);
        self.buf = chars.into_iter().collect();
        self.pos = i;
    }
}

/// What the right-hand pane shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Feed,
    Files,
    Stats,
    Plan,
    Agents,
    Mirror,
    Tree,
    Errors,
    Fleet,
}

pub const VIEWS: [View; 9] = [
    View::Feed,
    View::Files,
    View::Stats,
    View::Plan,
    View::Agents,
    View::Mirror,
    View::Tree,
    View::Errors,
    View::Fleet,
];

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Feed => "feed",
            View::Files => "files",
            View::Stats => "stats",
            View::Plan => "plan",
            View::Agents => "agents",
            View::Mirror => "mirror",
            View::Tree => "tree",
            View::Errors => "errors",
            View::Fleet => "fleet",
        }
    }

    pub fn next(self) -> Self {
        let i = VIEWS.iter().position(|v| *v == self).unwrap_or(0);
        VIEWS[(i + 1) % VIEWS.len()]
    }
}

pub struct App {
    pub root: PathBuf,
    pub sessions_dir: PathBuf,
    pub sessions: Vec<Session>,
    pub sel: usize,
    pub filter: Filter,
    pub view: View,
    /// false = subscription (no dollar figures), true = show API-equivalent cost
    pub show_cost: bool,
    pub file_sel: usize,
    pub file_top: usize,
    pub follow: bool,
    pub feed_sel: usize,
    pub feed_top: usize,
    pub list_top: usize,
    pub popup: bool,
    pub popup_scroll: u16,
    pub help: bool,
    pub only_live: bool,
    pub since: Duration,
    pub last_discover: Instant,
    /// transient message shown in the footer
    pub note: String,
    pub note_at: Instant,
    /// tmux panes, and the pane each live session is running inside
    pub tmux_ok: bool,
    pub steer: HashMap<String, Pane>,
    pub input: Option<Input>,
    /// set when the user asks to hand the terminal over to tmux
    pub attach_to: Option<String>,
    /// sessions blocked on a numbered prompt, by session id
    pub approvals: HashMap<String, Approval>,
    /// last rendered pane text, by session id
    pub mirror: HashMap<String, String>,
    /// working-tree state, by session id
    pub trees: HashMap<String, git::Tree>,
    /// messages to deliver when a session next goes idle
    pub queues: HashMap<String, Vec<String>>,
    /// every key goes to the selected session while this is on
    pub passthrough: bool,
    /// the actions menu for the selected session
    pub menu: bool,
    pub menu_sel: usize,
    pub notify_on: bool,
    pub search: String,
    /// (session index, event slot) for the current search
    pub hits: Vec<(usize, usize)>,
    pub hit_sel: usize,
    /// generic cursor for the simple list views
    pub list_sel: usize,
    /// scroll offset for the right-hand list views
    pub list_top_right: usize,
    /// when a session was last started or adopted, to absorb key repeats
    last_spawn: Instant,
    iso_cache: HashMap<String, (Instant, Option<Iso>)>,
    prev_status: HashMap<String, String>,
    prev_errors: HashMap<String, usize>,
    last_probe: Instant,
}

impl App {
    pub fn new(root: PathBuf, sessions_dir: PathBuf, since: Duration, only_live: bool) -> Self {
        let mut app = App {
            root,
            sessions_dir,
            sessions: Vec::new(),
            sel: 0,
            filter: Filter::All,
            view: View::Feed,
            show_cost: false,
            file_sel: 0,
            file_top: 0,
            follow: true,
            feed_sel: 0,
            feed_top: 0,
            list_top: 0,
            popup: false,
            popup_scroll: 0,
            help: false,
            only_live,
            since,
            last_discover: Instant::now(),
            note: String::new(),
            note_at: Instant::now(),
            tmux_ok: control::available(),
            steer: HashMap::new(),
            input: None,
            attach_to: None,
            approvals: HashMap::new(),
            mirror: HashMap::new(),
            trees: HashMap::new(),
            queues: HashMap::new(),
            passthrough: false,
            menu: false,
            menu_sel: 0,
            notify_on: notify::available(),
            search: String::new(),
            hits: Vec::new(),
            hit_sel: 0,
            list_sel: 0,
            list_top_right: 0,
            last_spawn: Instant::now() - Duration::from_secs(60),
            iso_cache: HashMap::new(),
            prev_status: HashMap::new(),
            prev_errors: HashMap::new(),
            last_probe: Instant::now() - Duration::from_secs(10),
        };
        app.discover();
        app.refresh();
        // refresh() preserves the selected session across re-sorts; on the very
        // first pass there is nothing to preserve, so start at the top — the
        // most active session.
        app.sel = 0;
        app.list_top = 0;
        app
    }

    /// Transcript files touched inside the window, plus any file belonging to a
    /// live session however old.
    fn candidates(&self) -> Vec<PathBuf> {
        let live = registry::scan(&self.sessions_dir);
        let adopted = control::adopted_ids(&control::panes());
        let cutoff = SystemTime::now() - self.since;
        let mut out = Vec::new();
        let Ok(projects) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for project in projects.flatten() {
            let Ok(files) = std::fs::read_dir(project.path()) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let is_live = live.contains_key(id) || adopted.contains(id);
                if self.only_live && !is_live {
                    continue;
                }
                let fresh = f
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t >= cutoff)
                    .unwrap_or(false);
                if is_live || fresh {
                    out.push(path);
                }
            }
        }
        out
    }

    /// Pick up sessions that appeared since the last pass, and drop ones whose
    /// file is gone.
    pub fn discover(&mut self) {
        let want = self.candidates();
        let live = registry::scan(&self.sessions_dir);
        let keep: Vec<String> = want
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
            .collect();
        self.sessions
            .retain(|s| keep.contains(&s.id) || live.contains_key(&s.id));
        for path in want {
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            match self.sessions.iter().position(|s| s.id == id) {
                // A session that was known only from the registry has started
                // writing: swap the placeholder for the real thing.
                Some(i) if self.sessions[i].placeholder => {
                    let mut sess = Session::open(path);
                    sess.backfill();
                    self.sessions[i] = sess;
                    continue;
                }
                Some(_) => continue,
                None => {}
            }
            let mut sess = Session::open(path);
            sess.backfill();
            self.sessions.push(sess);
        }
        // Live sessions that have not written anything yet are still real, and
        // they can already be blocked on a prompt.
        for (id, l) in live {
            if !self.sessions.iter().any(|s| s.id == id) {
                self.sessions.push(Session::pending(id, l));
            }
        }
        self.rescan_panes();
        self.last_discover = Instant::now();
    }

    /// Map live sessions to the tmux pane they run in, and represent panes that
    /// no session has claimed — a session started seconds ago has no transcript
    /// and no registry entry, but it is real and it can be typed into.
    pub fn rescan_panes(&mut self) {
        self.steer.clear();
        self.sessions.retain(|s| !s.id.starts_with("pane:"));
        if !self.tmux_ok {
            return;
        }
        let panes = control::panes();
        if panes.is_empty() {
            return;
        }
        for s in &self.sessions {
            // Normally: the registry gives a pid, and the pid leads to a pane.
            let by_pid = s
                .live
                .as_ref()
                .and_then(|live| control::pane_for(live.pid, &panes));
            // Just after adopting there is no registry entry yet — the new
            // process only registers once it is used — but the pane was started
            // as `claude --resume <id>`, which identifies it just as well.
            let pane = by_pid.or_else(|| control::adopted_pane(&s.id, &panes));
            if let Some(p) = pane {
                self.steer.insert(s.id.clone(), p);
            }
        }
        let claimed: Vec<String> = self.steer.values().map(|p| p.id.clone()).collect();
        for p in panes {
            if !p.cmd.starts_with("claude") || claimed.contains(&p.id) {
                continue;
            }
            let session = Session::from_pane(&p);
            self.steer.insert(session.id.clone(), p);
            self.sessions.push(session);
        }
    }

    pub fn pane_of(&self, id: &str) -> Option<&Pane> {
        self.steer.get(id)
    }

    /// Starting a session takes a second or two, and a held-down key must never
    /// turn into a second, third, hundredth process.
    fn may_spawn(&mut self) -> bool {
        if self.last_spawn.elapsed() < Duration::from_secs(3) {
            self.say("still starting the last one — give it a moment");
            return false;
        }
        self.last_spawn = Instant::now();
        true
    }

    /// Why a session cannot be typed into, and what to do about it.
    fn not_steerable(&self) -> String {
        match self.current() {
            Some(s) if s.live.is_none() && !s.in_pane => "that session has ended".into(),
            Some(s) if !self.tmux_ok => {
                format!("{} is not in tmux, and tmux is not installed", s.label())
            }
            Some(s) => format!("{} is not in tmux — press A to adopt it", s.label()),
            None => "no session selected".into(),
        }
    }

    pub fn say(&mut self, msg: impl Into<String>) {
        self.note = msg.into();
        self.note_at = Instant::now();
    }

    /// The note fades so the key hints come back.
    pub fn note_visible(&self) -> bool {
        !self.note.is_empty() && self.note_at.elapsed() < Duration::from_secs(6)
    }

    pub fn open_input(&mut self, kind: Prompt) {
        let label = match kind {
            Prompt::Send => match self.current() {
                Some(s) => format!("send to {}", s.label()),
                None => return,
            },
            Prompt::Broadcast => format!("send to all {} steerable", self.steer.len()),
            Prompt::NewSession => "new session in".into(),
            Prompt::Queue => match self.current() {
                Some(s) => format!("queue for {} (sends when idle)", s.label()),
                None => return,
            },
            Prompt::Search => "search all sessions".into(),
            Prompt::Isolate => "isolated session · branch name".into(),
            Prompt::Merge => match self.isolation() {
                Some(i) => format!("merge {} into {}? type yes", i.branch, i.base),
                None => {
                    self.say("this session is not in a worktree");
                    return;
                }
            },
            Prompt::Adopt => match self.current() {
                Some(s) => format!(
                    "move {} into tmux and close the original window? type yes",
                    s.label()
                ),
                None => return,
            },
            Prompt::Stop => match self.current() {
                Some(s) => format!("stop {}? type yes", s.label()),
                None => return,
            },
            Prompt::Discard => match self.isolation() {
                Some(i) => format!("remove the {} worktree? type yes", i.branch),
                None => {
                    self.say("this session is not in a worktree");
                    return;
                }
            },
        };
        let buf = if kind == Prompt::Isolate {
            String::new()
        } else if kind == Prompt::NewSession {
            self.current()
                .map(|s| s.cwd.clone())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
        } else {
            String::new()
        };
        let pos = buf.chars().count();
        let target = self.current().map(|s| s.id.clone());
        self.input = Some(Input {
            kind,
            label,
            target,
            buf,
            pos,
        });
    }

    /// Run whatever the input bar was collecting.
    pub fn submit_input(&mut self) {
        let Some(input) = self.input.take() else {
            return;
        };
        let text = input.buf.trim().to_string();
        // Sending is rarely a single message, so the line reopens afterwards.
        let kind = input.kind;
        match input.kind {
            Prompt::Send => {
                let Some(id) = input.target.clone() else {
                    return;
                };
                match self.pane_of(&id).cloned() {
                    Some(p) => match control::send_text(&p.id, &text) {
                        Ok(()) => self.say(format!("sent to {}", p.session)),
                        Err(e) => self.say(e),
                    },
                    None => self.say(self.not_steerable()),
                }
            }
            Prompt::Broadcast => {
                let panes: Vec<Pane> = self.steer.values().cloned().collect();
                let mut ok = 0;
                for p in &panes {
                    if control::send_text(&p.id, &text).is_ok() {
                        ok += 1;
                    }
                }
                self.say(format!("sent to {ok} sessions"));
            }
            Prompt::Queue => {
                let Some(id) = input.target.clone() else {
                    return;
                };
                if !self.steer.contains_key(&id) {
                    let msg = self.not_steerable();
                    self.say(msg);
                    return;
                }
                let q = self.queues.entry(id).or_default();
                q.push(text);
                let n = q.len();
                self.say(format!("queued — {n} waiting for the next idle moment"));
            }
            Prompt::Isolate => self.isolate(&text),
            Prompt::Merge => {
                if text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("y") {
                    self.merge_isolated();
                } else {
                    self.say("not merged");
                }
            }
            Prompt::Adopt => {
                if text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("y") {
                    self.adopt();
                } else {
                    self.say("left where it was");
                }
            }
            Prompt::Stop => {
                if text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("y") {
                    self.stop_session();
                } else {
                    self.say("left running");
                }
            }
            Prompt::Discard => {
                if text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("y") {
                    self.discard_isolated();
                } else {
                    self.say("kept");
                }
            }
            Prompt::Search => {
                self.run_search(&text);
                if !self.hits.is_empty() {
                    self.goto_hit();
                }
            }
            Prompt::NewSession => {
                if !self.may_spawn() {
                    return;
                }
                let path = PathBuf::from(if text.is_empty() { ".".into() } else { text });
                match control::new_session(&path, None) {
                    Ok(name) => {
                        self.say(format!("started {name} — it will appear here shortly"));
                        self.discover();
                    }
                    Err(e) => self.say(e),
                }
            }
        }
        self.keep_typing(kind);
    }

    /// Reopen the message line after sending, so a follow-up needs no keys.
    fn keep_typing(&mut self, kind: Prompt) {
        if matches!(kind, Prompt::Send | Prompt::Queue | Prompt::Broadcast) {
            self.open_input(kind);
        }
    }

    /// Escape interrupts the current turn, exactly as pressing it would.
    pub fn interrupt(&mut self) {
        let Some(id) = self.current().map(|s| s.id.clone()) else {
            return;
        };
        match self.pane_of(&id).cloned() {
            Some(p) => match control::send_key(&p.id, "Escape") {
                Ok(()) => self.say("interrupt sent"),
                Err(e) => self.say(e),
            },
            None => self.say("that session is not running in tmux"),
        }
    }

    pub fn attach(&mut self) {
        let Some(id) = self.current().map(|s| s.id.clone()) else {
            return;
        };
        match self.pane_of(&id) {
            Some(p) => self.attach_to = Some(p.session.clone()),
            None => {
                let msg = self.not_steerable();
                self.say(msg);
            }
        }
    }

    /// Read new transcript lines, re-attach liveness, re-sort.
    pub fn refresh(&mut self) {
        let live = registry::scan(&self.sessions_dir);
        let seen = registry::available(&self.sessions_dir);
        for s in &mut self.sessions {
            s.pump();
            s.live = live.get(&s.id).cloned();
            s.registry_seen = seen;
            // A session running in a pane is running, whether or not it has
            // got round to registering itself.
            s.in_pane = self.steer.contains_key(&s.id);
        }
        let selected_id = self.sessions.get(self.sel).map(|s| s.id.clone());
        let blocked: Vec<String> = self.approvals.keys().cloned().collect();
        // Order has to hold still: a list that re-sorts on every tick moves rows
        // out from under the cursor mid-keystroke. Only a session that is
        // blocked on a person jumps the queue; everything else stays in the
        // order it started, like tabs.
        self.sessions.sort_by(|a, b| {
            let blocked_a = !blocked.contains(&a.id) as u8;
            let blocked_b = !blocked.contains(&b.id) as u8;
            let now = chrono::Utc::now();
            blocked_a
                .cmp(&blocked_b)
                .then(a.started.unwrap_or(now).cmp(&b.started.unwrap_or(now)))
                .then(a.id.cmp(&b.id))
        });
        if let Some(id) = selected_id {
            if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                self.sel = i;
            }
        }
        if self.sel >= self.sessions.len() {
            self.sel = self.sessions.len().saturating_sub(1);
        }
    }

    /// Read every steerable session's pane: feeds the mirror, spots sessions
    /// blocked on a prompt, and drives notifications. One tmux call per
    /// steerable session, at most once a second.
    pub fn probe(&mut self) {
        if self.last_probe.elapsed() < Duration::from_millis(900) {
            return;
        }
        self.last_probe = Instant::now();
        let targets: Vec<(String, String, String)> = self
            .sessions
            .iter()
            .filter_map(|s| {
                let p = self.steer.get(&s.id)?;
                Some((s.id.clone(), p.id.clone(), s.label()))
            })
            .collect();
        for (id, pane, label) in targets {
            let Some(text) = control::capture(&pane) else {
                continue;
            };
            let approval = control::pending_approval(&text);
            self.mirror.insert(id.clone(), text);
            match approval {
                Some(a) => {
                    let fresh = self.approvals.get(&id) != Some(&a);
                    if fresh {
                        self.notify(&format!("{label} needs a decision"), &a.question);
                    }
                    self.approvals.insert(id, a);
                }
                None => {
                    self.approvals.remove(&id);
                }
            }
        }
        self.watch_transitions();
        self.drain_queues();
    }

    /// Notify on the two moments worth interrupting someone for: a session
    /// that stopped and wants input, and a session that hit an error.
    fn watch_transitions(&mut self) {
        let snapshot: Vec<(String, String, String, usize)> = self
            .sessions
            .iter()
            .map(|s| {
                let state = match s.status() {
                    Status::Running(_) | Status::Working => "working",
                    Status::Waiting => "waiting",
                    Status::Ended => "ended",
                };
                (s.id.clone(), s.label(), state.to_string(), s.errors)
            })
            .collect();
        for (id, label, state, errors) in snapshot {
            if let Some(prev) = self.prev_status.get(&id) {
                if prev == "working" && state == "waiting" {
                    self.notify(&format!("{label} is waiting on you"), "turn finished");
                }
            }
            if let Some(prev) = self.prev_errors.get(&id) {
                if errors > *prev {
                    self.notify(&format!("{label} hit an error"), "check the errors pane");
                }
            }
            self.prev_status.insert(id.clone(), state);
            self.prev_errors.insert(id, errors);
        }
    }

    /// Deliver queued messages to sessions that have gone idle.
    fn drain_queues(&mut self) {
        let ready: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| matches!(s.status(), Status::Waiting))
            .map(|s| s.id.clone())
            .filter(|id| {
                !self.approvals.contains_key(id)
                    && self.queues.get(id).map(|q| !q.is_empty()).unwrap_or(false)
            })
            .collect();
        for id in ready {
            let Some(pane) = self.steer.get(&id).cloned() else {
                continue;
            };
            let Some(queue) = self.queues.get_mut(&id) else {
                continue;
            };
            let msg = queue.remove(0);
            let left = queue.len();
            match control::send_text(&pane.id, &msg) {
                Ok(()) => self.say(format!("delivered a queued message ({left} left)")),
                Err(e) => self.say(e),
            }
        }
    }

    pub fn notify(&mut self, title: &str, body: &str) {
        self.say(format!("{title} — {body}"));
        if self.notify_on {
            notify::send(title, body);
        }
    }

    /// The prompt to show and answer: the selected session if it is blocked,
    /// otherwise the first blocked session, so an answer is always one key away
    /// no matter where the cursor is.
    pub fn approval(&self) -> Option<(&Session, &Approval)> {
        if let Some(s) = self.current() {
            if let Some(a) = self.approvals.get(&s.id) {
                return Some((s, a));
            }
        }
        self.sessions
            .iter()
            .find_map(|s| self.approvals.get(&s.id).map(|a| (s, a)))
    }

    fn answer_target(&self) -> Option<String> {
        self.approval().map(|(s, _)| s.id.clone())
    }

    /// Answer the prompt shown in the strip with option `n`; 0 means escape.
    pub fn answer(&mut self, n: usize) {
        let Some(id) = self.answer_target() else {
            self.say("nothing is waiting on you");
            return;
        };
        let Some(pane) = self.steer.get(&id).cloned() else {
            let msg = self.not_steerable();
            self.say(msg);
            return;
        };
        let outcome = if n == 0 {
            control::send_key(&pane.id, "Escape")
        } else {
            control::answer(&pane.id, n)
        };
        match outcome {
            Ok(()) => {
                self.approvals.remove(&id);
                self.say(if n == 0 {
                    "declined".into()
                } else {
                    format!("answered {n}")
                });
            }
            Err(e) => self.say(e),
        }
    }

    /// Continue a non-tmux session inside tmux so it becomes steerable.
    pub fn adopt(&mut self) {
        let Some((id, cwd)) = self.current().map(|s| {
            let cwd = if s.cwd.is_empty() {
                ".".to_string()
            } else {
                s.cwd.clone()
            };
            (s.id.clone(), cwd)
        }) else {
            return;
        };
        if self.steer.contains_key(&id) {
            self.say("already steerable");
            return;
        }
        if let Some(p) = control::adopted_pane(&id, &control::panes()) {
            let name = p.session;
            self.say(format!("already adopted as {name} — press a to attach"));
            return;
        }
        if !self.may_spawn() {
            return;
        }
        let original = self.current().and_then(|s| s.live.as_ref().map(|l| l.pid));
        match control::adopt(PathBuf::from(cwd).as_path(), &id) {
            Ok(name) => {
                // Two clients on one conversation would both append to the same
                // transcript, so the original goes as soon as the copy is up.
                let closed = original.map(control::end_process).unwrap_or(false);
                self.say(if closed {
                    format!("moved into tmux as {name} — the old window has closed")
                } else {
                    format!("resumed in tmux as {name} — close the old window yourself")
                });
                self.discover();
            }
            Err(e) => self.say(e),
        }
    }

    /// What can be done to the selected session right now. Disabled entries
    /// carry the reason, which is usually also the fix.
    pub fn actions(&mut self) -> Vec<Action> {
        let Some(s) = self.current() else {
            return Vec::new();
        };
        let (id, cwd) = (s.id.clone(), s.cwd.clone());
        let live = s.live.is_some() || s.in_pane;
        let name = s.label();
        let steerable = self.steer.contains_key(&id);
        let blocked = self.approvals.contains_key(&id);
        let iso = self.isolation();
        let in_repo =
            !cwd.is_empty() && crate::git::repo_root(std::path::Path::new(&cwd)).is_some();

        let why_steer = if !live {
            "this session has ended".to_string()
        } else {
            format!("{name} is not running in tmux — adopt it first")
        };
        let mut v = vec![
            Action {
                key: 'y',
                label: "Answer what it is asking",
                enabled: blocked,
                why: "it is not waiting on you".into(),
            },
            Action {
                key: 's',
                label: "Send it a message",
                enabled: steerable,
                why: why_steer.clone(),
            },
            Action {
                key: 'Q',
                label: "Queue a message for when it is idle",
                enabled: steerable,
                why: why_steer.clone(),
            },
            Action {
                key: 'i',
                label: "Interrupt what it is doing",
                enabled: steerable,
                why: why_steer.clone(),
            },
            Action {
                key: 'm',
                label: "Type into it directly",
                enabled: steerable,
                why: why_steer.clone(),
            },
            Action {
                key: 'a',
                label: "Attach full-screen",
                enabled: steerable,
                why: why_steer,
            },
            Action {
                key: 'A',
                label: "Adopt into tmux so it can be steered",
                enabled: live && !steerable,
                why: if steerable {
                    "already steerable".into()
                } else {
                    "this session has ended".into()
                },
            },
        ];
        if iso.is_some() {
            v.push(Action {
                key: 'M',
                label: "Merge its branch back",
                enabled: true,
                why: String::new(),
            });
            v.push(Action {
                key: 'X',
                label: "Remove its checkout",
                enabled: true,
                why: String::new(),
            });
        }
        v.push(Action {
            key: 'K',
            label: "Stop this session",
            enabled: steerable,
            why: "only sessions scope can reach in tmux can be stopped".into(),
        });
        v.push(Action {
            key: 'P',
            label: "Tidy up finished scope sessions",
            enabled: self.tmux_ok,
            why: "tmux is not installed".into(),
        });
        v.push(Action {
            key: 'n',
            label: "Start a new session",
            enabled: self.tmux_ok,
            why: "tmux is not installed".into(),
        });
        v.push(Action {
            key: 'W',
            label: "Start one isolated on its own branch",
            enabled: self.tmux_ok && in_repo,
            why: if self.tmux_ok {
                "this session is not inside a git repository".into()
            } else {
                "tmux is not installed".into()
            },
        });
        v
    }

    /// Run an action by its key, reporting why when it cannot run.
    pub fn run_action(&mut self, key: char) {
        let action = self.actions().into_iter().find(|a| a.key == key);
        if let Some(a) = &action {
            if !a.enabled {
                let why = a.why.clone();
                self.say(why);
                return;
            }
        }
        self.menu = false;
        match key {
            'y' => self.answer(1),
            'd' => self.answer(0),
            's' => self.open_input(Prompt::Send),
            'Q' => self.open_input(Prompt::Queue),
            'i' => self.interrupt(),
            'm' => self.toggle_passthrough(),
            'a' => self.attach(),
            'A' => self.open_input(Prompt::Adopt),
            'M' => self.open_input(Prompt::Merge),
            'X' => self.open_input(Prompt::Discard),
            'K' => self.open_input(Prompt::Stop),
            'P' => {
                let n = control::prune();
                self.say(format!(
                    "closed {n} finished session{}",
                    if n == 1 { "" } else { "s" }
                ));
                self.discover();
            }
            'n' => self.open_input(Prompt::NewSession),
            'W' => self.open_input(Prompt::Isolate),
            'L' => self.launch_fleet(),
            _ => {}
        }
    }

    /// Move the selection to the next session that is blocked on a question.
    pub fn next_blocked(&mut self) {
        if self.approvals.is_empty() {
            self.say("nothing is waiting on you");
            return;
        }
        let n = self.sessions.len();
        for step in 1..=n {
            let i = (self.sel + step) % n;
            if self.approvals.contains_key(&self.sessions[i].id) {
                self.sel = i;
                return;
            }
        }
    }

    pub fn cycle_hit(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        let n = self.hits.len() as isize;
        self.hit_sel = (((self.hit_sel as isize + delta) % n + n) % n) as usize;
        self.goto_hit();
        let (i, total) = (self.hit_sel + 1, self.hits.len());
        self.say(format!("match {i} of {total}"));
    }

    pub fn toggle_passthrough(&mut self) {
        if !self.passthrough {
            let Some(id) = self.current().map(|s| s.id.clone()) else {
                return;
            };
            if !self.steer.contains_key(&id) {
                let msg = self.not_steerable();
                self.say(msg);
                return;
            }
            self.view = View::Mirror;
            self.passthrough = true;
            self.say("passthrough on — ctrl+] to stop");
        } else {
            self.passthrough = false;
            self.say("passthrough off");
        }
    }

    /// Forward one key press to the selected session.
    pub fn forward_key(&mut self, code: crossterm::event::KeyCode, ctrl: bool) {
        let Some(id) = self.current().map(|s| s.id.clone()) else {
            return;
        };
        let Some(pane) = self.steer.get(&id).cloned() else {
            return;
        };
        if let Some(key) = control::tmux_key(code, ctrl) {
            let _ = control::forward(&pane.id, &key);
        }
        // Show the result immediately rather than waiting for the next probe.
        if let Some(text) = control::capture(&pane.id) {
            self.mirror.insert(id, text);
        }
    }

    /// Whether the selected session works in its own checkout, cached — this
    /// asks git several questions and the answer changes slowly.
    pub fn isolation(&mut self) -> Option<Iso> {
        let s = self.current()?;
        let (id, cwd) = (s.id.clone(), s.cwd.clone());
        if cwd.is_empty() {
            return None;
        }
        let fresh = self
            .iso_cache
            .get(&id)
            .map(|(at, _)| at.elapsed() < Duration::from_secs(5))
            .unwrap_or(false);
        if !fresh {
            let path = PathBuf::from(&cwd);
            let found = git::is_worktree(&path)
                .then(|| {
                    let repo = git::main_repo(&path)?;
                    let branch = git::status(&path)?.branch;
                    let base = git::base_branch(&repo);
                    let ahead = git::ahead_behind(&path, &base).map(|(a, _)| a).unwrap_or(0);
                    Some(Iso {
                        repo,
                        branch,
                        base,
                        ahead,
                    })
                })
                .flatten();
            self.iso_cache.insert(id.clone(), (Instant::now(), found));
        }
        self.iso_cache.get(&id).and_then(|(_, v)| v.clone())
    }

    /// Start a session on a fresh branch in its own checkout, so several
    /// sessions can work the same repository without colliding.
    pub fn isolate(&mut self, branch: &str) {
        if !self.may_spawn() {
            return;
        }
        let branch = if branch.trim().is_empty() {
            "scope-work"
        } else {
            branch.trim()
        };
        let cwd = self
            .current()
            .map(|s| s.cwd.clone())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let Some(repo) = git::repo_root(PathBuf::from(&cwd).as_path()) else {
            self.say("not inside a git repository");
            return;
        };
        match git::create_worktree(&repo, branch) {
            Ok(dir) => match control::new_session_with(&dir, None, None, None, None) {
                Ok(name) => {
                    self.say(format!("{name} on branch {branch} in {}", dir.display()));
                    self.discover();
                }
                Err(e) => self.say(e),
            },
            Err(e) => self.say(e),
        }
    }

    pub fn merge_isolated(&mut self) {
        let Some(iso) = self.isolation() else { return };
        let (repo, branch, base) = (iso.repo, iso.branch, iso.base);
        match git::merge(&repo, &branch, &base) {
            Ok(out) => self.say(format!(
                "merged {branch} into {base} · {}",
                out.lines().next().unwrap_or("")
            )),
            Err(e) => self.say(e),
        }
    }

    pub fn discard_isolated(&mut self) {
        let Some(iso) = self.isolation() else { return };
        let (repo, branch) = (iso.repo, iso.branch);
        let Some(cwd) = self.current().map(|s| s.cwd.clone()) else {
            return;
        };
        match git::remove_worktree(&repo, &cwd) {
            Ok(()) => self.say(format!("removed the {branch} worktree")),
            Err(e) => self.say(e),
        }
    }

    /// End a session by closing the tmux session it runs in. Claude Code exits
    /// with it; the transcript stays on disk.
    pub fn stop_session(&mut self) {
        let Some(id) = self.current().map(|s| s.id.clone()) else {
            return;
        };
        let Some(pane) = self.steer.get(&id).cloned() else {
            let msg = self.not_steerable();
            self.say(msg);
            return;
        };
        match control::kill_session(&pane.session) {
            Ok(()) => {
                self.say(format!("stopped {}", pane.session));
                self.discover();
            }
            Err(e) => self.say(e),
        }
    }

    pub fn tree(&mut self) -> Option<&git::Tree> {
        let s = self.current()?;
        let (id, cwd) = (s.id.clone(), s.cwd.clone());
        if cwd.is_empty() {
            return None;
        }
        let stale = self
            .trees
            .get(&id)
            .map(|t| t.fetched.elapsed() > Duration::from_secs(5))
            .unwrap_or(true);
        if stale {
            if let Some(t) = git::status(PathBuf::from(&cwd).as_path()) {
                self.trees.insert(id.clone(), t);
            }
        }
        self.trees.get(&id)
    }

    /// Substring search across every loaded session.
    pub fn run_search(&mut self, needle: &str) {
        self.search = needle.to_string();
        self.hits.clear();
        self.hit_sel = 0;
        if needle.is_empty() {
            return;
        }
        let n = needle.to_lowercase();
        for (si, s) in self.sessions.iter().enumerate() {
            for (ei, ev) in s.events.iter().enumerate() {
                if ev.head.to_lowercase().contains(&n) || ev.body.to_lowercase().contains(&n) {
                    self.hits.push((si, ei));
                }
            }
        }
        let count = self.hits.len();
        self.say(format!("{count} matches for \"{needle}\""));
    }

    /// Jump to the selected search hit.
    pub fn goto_hit(&mut self) {
        let Some((si, ei)) = self.hits.get(self.hit_sel).copied() else {
            return;
        };
        self.sel = si;
        self.view = View::Feed;
        self.filter = Filter::All;
        self.follow = false;
        let slot = self
            .feed_indices()
            .iter()
            .position(|i| *i == ei)
            .unwrap_or(0);
        self.feed_sel = slot;
        self.feed_top = slot.saturating_sub(5);
    }

    /// One timeline across every session, newest last.
    pub fn fleet(&self) -> Vec<(usize, usize)> {
        let mut all: Vec<(usize, usize, chrono::DateTime<chrono::Utc>)> = Vec::new();
        for (si, s) in self.sessions.iter().enumerate() {
            let start = s.events.len().saturating_sub(400);
            for (ei, ev) in s.events.iter().enumerate().skip(start) {
                if let Some(ts) = ev.ts {
                    all.push((si, ei, ts));
                }
            }
        }
        all.sort_by_key(|(_, _, ts)| *ts);
        all.into_iter().map(|(si, ei, _)| (si, ei)).collect()
    }

    /// Events that failed, for the errors pane.
    pub fn errors(&self) -> Vec<usize> {
        match self.current() {
            Some(s) => s
                .events
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.ok)
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Launch every session described in ~/.config/nyfe-scope/fleet.json.
    pub fn launch_fleet(&mut self) {
        if !self.may_spawn() {
            return;
        }
        let path = fleet_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            self.say(format!("no fleet file at {}", path.display()));
            return;
        };
        let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
            self.say("fleet file is not a JSON array");
            return;
        };
        let mut started = 0;
        for item in items {
            let g = |k: &str| item.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let mut cwd = g("cwd").unwrap_or_else(|| ".".into());
            if let Some(branch) = g("worktree") {
                match git::repo_root(PathBuf::from(&cwd).as_path())
                    .ok_or_else(|| format!("{cwd} is not a git repository"))
                    .and_then(|repo| git::create_worktree(&repo, &branch))
                {
                    Ok(dir) => cwd = dir.to_string_lossy().into_owned(),
                    Err(e) => {
                        self.say(e);
                        continue;
                    }
                }
            }
            match control::new_session_with(
                PathBuf::from(cwd).as_path(),
                g("model").as_deref(),
                g("effort").as_deref(),
                g("permission_mode").as_deref(),
                g("prompt").as_deref(),
            ) {
                Ok(_) => started += 1,
                Err(e) => self.say(e),
            }
        }
        self.say(format!("launched {started} sessions"));
        self.discover();
    }

    pub fn current(&self) -> Option<&Session> {
        self.sessions.get(self.sel)
    }

    /// Positions in the selected session's ring buffer that pass the filter.
    /// Indices rather than references, so callers can still mutate view state.
    pub fn feed_indices(&self) -> Vec<usize> {
        match self.current() {
            Some(s) => s
                .events
                .iter()
                .enumerate()
                .filter(|(_, e)| self.filter.keeps(e))
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn feed_len(&self) -> usize {
        self.feed_indices().len()
    }

    /// The nth event that passes the filter.
    pub fn event_at(&self, nth: usize) -> Option<&Ev> {
        let idx = *self.feed_indices().get(nth)?;
        self.current()?.events.get(idx)
    }

    /// Files the selected session touched, most recent first.
    pub fn file_keys(&self) -> Vec<String> {
        let Some(s) = self.current() else {
            return Vec::new();
        };
        let mut keys: Vec<(&String, &crate::session::FileTouch)> = s.files.iter().collect();
        keys.sort_by(|a, b| b.1.last.cmp(&a.1.last));
        keys.into_iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn move_files(&mut self, delta: isize) {
        let len = self.file_keys().len();
        if len == 0 {
            return;
        }
        let next = (self.file_sel as isize + delta).clamp(0, len as isize - 1);
        self.file_sel = next as usize;
    }

    /// Every recorded change to the selected file, newest first.
    pub fn file_history(&self) -> Option<(String, String)> {
        let key = self.file_keys().into_iter().nth(self.file_sel)?;
        let s = self.current()?;
        let touch = s.files.get(&key)?;
        let mut out = String::new();
        for abs in touch.changes.iter().rev() {
            let Some(slot) = s.slot_of(*abs) else {
                continue;
            };
            let Some(ev) = s.events.get(slot) else {
                continue;
            };
            let when = ev
                .ts
                .map(|t| {
                    t.with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            out.push_str(&format!("── {when} ──\n{}\n\n", ev.body));
        }
        if out.is_empty() {
            out.push_str("no recorded diffs for this file (read-only, or trimmed from the buffer)");
        }
        Some((key, out))
    }

    pub fn select_session(&mut self, delta: isize) {
        if self.sessions.is_empty() {
            return;
        }
        let n = self.sessions.len() as isize;
        let next = (self.sel as isize + delta).clamp(0, n - 1);
        if next as usize != self.sel {
            self.sel = next as usize;
            self.follow = true;
            self.feed_top = 0;
            self.file_sel = 0;
            self.file_top = 0;
        }
    }

    pub fn move_feed(&mut self, delta: isize) {
        let len = self.feed_len();
        if len == 0 {
            return;
        }
        let cur = if self.follow {
            len as isize - 1
        } else {
            self.feed_sel as isize
        };
        let next = (cur + delta).clamp(0, len as isize - 1);
        self.feed_sel = next as usize;
        self.follow = next as usize == len - 1;
    }

    /// (output tokens, estimated dollars, sessions currently working)
    /// A warning to show once when this Claude Code install looks newer or
    /// stranger than what scope was built against.
    pub fn compatibility(&self) -> Option<String> {
        for s in &self.sessions {
            if s.unreadable() {
                return Some(format!(
                    "transcript format not recognised ({}) — figures may be incomplete",
                    if s.version.is_empty() {
                        "unknown version"
                    } else {
                        &s.version
                    }
                ));
            }
        }
        for s in &self.sessions {
            if let Some(v) = s.client_version() {
                if v != crate::session::TESTED {
                    return Some(format!(
                        "Claude Code {} is newer than this build was tested against ({}.{}.x)",
                        s.version,
                        crate::session::TESTED.0,
                        crate::session::TESTED.1
                    ));
                }
            }
        }
        None
    }

    pub fn totals(&self) -> (u64, f64, usize) {
        let mut tokens = 0;
        let mut cost = 0.0;
        let mut working = 0;
        for s in &self.sessions {
            tokens += s.totals.output;
            cost += s.totals.cost;
            if matches!(s.status(), Status::Running(_) | Status::Working) {
                working += 1;
            }
        }
        (tokens, cost, working)
    }
}

/// Home directory across platforms; falls back to the working directory so a
/// missing HOME degrades to a clear "no transcripts" message, not a panic.
pub fn home() -> PathBuf {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(dir) = std::env::var(key) {
            if !dir.is_empty() {
                return PathBuf::from(dir);
            }
        }
    }
    PathBuf::from(".")
}

/// Claude Code's config directory, honouring CLAUDE_CONFIG_DIR.
pub fn config_dir() -> PathBuf {
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(".claude"),
    }
}

/// Where the fleet template lives.
pub fn fleet_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("nyfe-scope").join("fleet.json");
        }
    }
    home().join(".config").join("nyfe-scope").join("fleet.json")
}

pub fn default_root() -> PathBuf {
    config_dir().join("projects")
}

pub fn default_sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}
