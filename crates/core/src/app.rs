//! Discovery, refresh, and view state.

use crate::agents;
use crate::control::{self, Approval, Pane};
use crate::event::{Ev, Filter};
use crate::git;
use crate::history::{self, Past};
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
    StopAll,
    Rename,
    /// what to call the session about to start
    NameIt,
    Adopt,
}

/// Where things were drawn last frame, so a click can be turned back into the
/// thing that was clicked.
#[derive(Clone, Copy, Default)]
pub struct Regions {
    /// inner area of the session list (x, y, w, h) and the index of its top row
    pub list: (u16, u16, u16, u16),
    pub list_top: usize,
    /// inner area of the right-hand pane and the index of its top row
    pub right: (u16, u16, u16, u16),
    pub right_top: usize,
    /// the actions menu, when it is open
    pub menu: Option<(u16, u16, u16, u16)>,
}

fn inside(area: (u16, u16, u16, u16), col: u16, row: u16) -> bool {
    let (x, y, w, h) = area;
    col >= x && col < x + w && row >= y && row < y + h
}

impl Regions {
    /// Which session row is under the pointer. Rows are two lines tall.
    pub fn session_at(&self, col: u16, row: u16) -> Option<usize> {
        inside(self.list, col, row).then(|| self.list_top + ((row - self.list.1) / 2) as usize)
    }

    /// Which right-pane row is under the pointer.
    pub fn right_at(&self, col: u16, row: u16) -> Option<usize> {
        inside(self.right, col, row).then(|| self.right_top + (row - self.right.1) as usize)
    }

    pub fn menu_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.menu?;
        inside(area, col, row).then(|| (row - area.1) as usize)
    }

    pub fn over_list(&self, col: u16, row: u16) -> bool {
        inside(self.list, col, row)
    }
}

/// What to start, read off the new-session line.
///
/// The line is a path and then anything else you would have typed on the
/// command line: `~/api --model opus --effort high fix the failing tests`.
/// Flags are Claude Code's own, and whatever is left over is the first thing
/// the session is asked — no quoting, because a message is the common case and
/// having to quote it is a papercut every single time.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NewSpec {
    pub path: String,
    /// which agent to run; Claude Code when nothing says otherwise
    pub agent: Option<String>,
    /// what to call the session
    pub name: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
    pub prompt: Option<String>,
}

/// Where scope keeps what it knows between runs.
fn data_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local").join("share"));
    base.join("nyfe-scope")
}

fn names_path() -> PathBuf {
    data_dir().join("names.json")
}

/// Names for sessions that have none of their own, by the tmux session they
/// run in — which is what identifies one of these for as long as it lives.
fn load_names() -> HashMap<String, String> {
    std::fs::read_to_string(names_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_names(names: &HashMap<String, String>) {
    let path = names_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(names) {
        let _ = std::fs::write(path, text);
    }
}

/// Append the record Claude Code writes when a conversation is renamed. Its
/// own `/rename` does exactly this, so a name set here is the same name, read
/// the same way, whenever the conversation is next opened.
fn write_title(path: &std::path::Path, id: &str, name: &str) -> Result<(), String> {
    use std::io::Write;
    let record = serde_json::json!({
        "type": "custom-title",
        "customTitle": name,
        "sessionId": id,
    });
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot write to that transcript: {e}"))?;
    writeln!(f, "{record}").map_err(|e| format!("cannot write to that transcript: {e}"))
}

/// `~` means home, as it does everywhere else a path is typed. Nothing here
/// goes through a shell, so if scope does not expand it nothing will — the
/// fleet file has always documented `~/api` and it never worked.
pub fn expand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => dirs_home().join(rest).to_string_lossy().into_owned(),
        None if path == "~" => dirs_home().to_string_lossy().into_owned(),
        None => path.to_string(),
    }
}

pub fn parse_new(text: &str) -> NewSpec {
    let mut spec = NewSpec::default();
    let mut words = text.split_whitespace().peekable();
    if let Some(first) = words.peek() {
        if !first.starts_with('-') {
            spec.path = words.next().unwrap_or_default().to_string();
        }
    }
    let mut rest: Vec<&str> = Vec::new();
    while let Some(word) = words.next() {
        // Everything after a bare `--` is the message, whatever it looks like.
        if word == "--" {
            rest.extend(words.by_ref());
            break;
        }
        let field = match word {
            "--agent" | "-a" => Some(&mut spec.agent),
            "--name" | "-n" => Some(&mut spec.name),
            "--model" | "-m" => Some(&mut spec.model),
            "--effort" | "-e" => Some(&mut spec.effort),
            "--mode" | "--permission-mode" | "-p" => Some(&mut spec.mode),
            "--prompt" => Some(&mut spec.prompt),
            _ => None,
        };
        match field {
            Some(slot) => {
                if let Some(v) = words.next() {
                    *slot = Some(v.to_string());
                }
            }
            None => {
                rest.push(word);
                rest.extend(words.by_ref());
                break;
            }
        }
    }
    if !rest.is_empty() {
        spec.prompt = Some(rest.join(" "));
    }
    if spec.path.is_empty() {
        spec.path = ".".into();
    }
    spec
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
    Read,
}

pub const VIEWS: [View; 10] = [
    View::Feed,
    View::Files,
    View::Stats,
    View::Plan,
    View::Agents,
    View::Mirror,
    View::Tree,
    View::Errors,
    View::Fleet,
    View::Read,
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
            View::Read => "read",
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
    /// the view passthrough took over, to be put back when it stops
    view_before: Option<View>,
    /// when quitting was last asked about, where quitting ends sessions
    quit_asked: Option<Instant>,
    /// which session was asked about stopping, and when
    stop_asked: Option<(String, Instant)>,
    /// names scope keeps for sessions that have none of their own
    names: HashMap<String, String>,
    /// a session waiting on the name it is about to be given
    pending_new: Option<NewSpec>,
    /// the conversation browser: everything on the machine, resumable
    pub past: Vec<Past>,
    pub past_open: bool,
    pub past_sel: usize,
    pub past_top: usize,
    pub past_filter: String,
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
    /// what was drawn where last frame, for mouse hit-testing
    pub regions: Regions,
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
            view_before: None,
            quit_asked: None,
            stop_asked: None,
            names: load_names(),
            pending_new: None,
            past: Vec::new(),
            past_open: false,
            past_sel: 0,
            past_top: 0,
            past_filter: String::new(),
            menu: false,
            menu_sel: 0,
            notify_on: notify::available(),
            search: String::new(),
            hits: Vec::new(),
            hit_sel: 0,
            list_sel: 0,
            list_top_right: 0,
            regions: Regions::default(),
            last_spawn: Instant::now() - Duration::from_secs(60),
            iso_cache: HashMap::new(),
            prev_status: HashMap::new(),
            prev_errors: HashMap::new(),
            last_probe: Instant::now() - Duration::from_secs(10),
        };
        app.discover();
        app.refresh();
        // Start on something that is actually running: opening on a session
        // that finished hours ago is a poor first impression.
        app.sel = app
            .sessions
            .iter()
            .position(|s| !matches!(s.status(), Status::Ended))
            .unwrap_or(0);
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
                .and_then(|live| control::pane_for(live.pid, &s.cwd, &panes));
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
            // Something scope started is a session whatever it is running:
            // an agent it has an entry for, or a command someone named itself.
            let ours = agents::is_agent(&p.cmd) || p.session.starts_with("scope-");
            if !ours || claimed.contains(&p.id) {
                continue;
            }
            let mut session = Session::from_pane(&p);
            // A session with no transcript is whatever scope has been told to
            // call it, since nothing else will ever name it.
            if let Some(name) = self.names.get(&p.session) {
                session.title = name.clone();
                session.titled = true;
            }
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
            Some(s) if s.live.is_none() && !s.in_pane => {
                if s.placeholder {
                    "that session has ended".into()
                } else {
                    "that session has ended — press A to reopen the conversation".into()
                }
            }
            Some(s) if !self.tmux_ok => {
                format!(
                    "{} cannot be steered — {}",
                    s.label(),
                    control::unavailable_hint()
                )
            }
            Some(s) => control::steer_hint(&s.label()),
            None => "no session selected".into(),
        }
    }

    /// Whether quitting can go ahead. Sessions scope hosts itself end with it,
    /// so the first `q` says what would be lost and the second one means it.
    /// Where the backend outlives scope there is nothing to lose, and `q` quits.
    pub fn may_quit(&mut self) -> bool {
        let n = control::hosted_count();
        let asked = self
            .quit_asked
            .map(|t| t.elapsed() < Duration::from_secs(5))
            .unwrap_or(false);
        if n == 0 || asked {
            return true;
        }
        self.quit_asked = Some(Instant::now());
        self.say(format!(
            "q again to quit — {n} session{} scope is hosting would stop (each reopens with A)",
            if n == 1 { "" } else { "s" }
        ));
        false
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
            Prompt::NewSession => {
                "new session · path [--agent a] [--model m] [--effort e] [--mode p] [first message]"
                    .into()
            }
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
                Some(s) if s.live.is_none() && !s.in_pane => {
                    format!("reopen {} in {}? type yes", s.label(), control::WHERE)
                }
                Some(s) => format!(
                    "move {} into {} and close the original window? type yes",
                    s.label(),
                    control::WHERE
                ),
                None => return,
            },
            Prompt::StopAll => {
                let n = self.steer.len();
                format!("stop all {n} sessions scope started? type yes")
            }
            Prompt::Rename => match self.current() {
                Some(s) => format!("rename {}", s.label()),
                None => return,
            },
            Prompt::NameIt => "name it (enter to skip)".into(),
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
                if let Err(e) = self.send_to(&id, &text) {
                    self.say(e);
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
            Prompt::StopAll => {
                if text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("y") {
                    let closed = control::stop_all();
                    self.say(format!(
                        "closed {} session{} — each one can be reopened with A",
                        closed.len(),
                        if closed.len() == 1 { "" } else { "s" }
                    ));
                    self.discover();
                } else {
                    self.say("left running");
                }
            }
            Prompt::Rename => {
                let Some(id) = input.target.clone() else {
                    return;
                };
                if let Err(e) = self.rename(&id, &text) {
                    self.say(e);
                }
            }
            Prompt::NameIt => {
                let Some(mut spec) = self.pending_new.take() else {
                    return;
                };
                let name = text.trim();
                if !name.is_empty() {
                    spec.name = Some(name.to_string());
                }
                match self.start_session(&spec) {
                    Ok(session) => {
                        let called = spec.name.unwrap_or(session.clone());
                        self.say(format!("started {called} — it will appear here shortly"))
                    }
                    Err(e) => self.say(e),
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
                let spec = parse_new(&text);
                // Nothing said what to call it, so ask before starting: naming
                // it at birth is one line typed into it, naming it later is a
                // second command.
                if spec.name.is_none() {
                    self.pending_new = Some(spec);
                    self.open_input(Prompt::NameIt);
                    return;
                }
                match self.start_session(&spec) {
                    Ok(name) => self.say(format!("started {name} — it will appear here shortly")),
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

    /// Start a session, and give it the name it was asked for.
    ///
    /// Claude Code is asked to name itself, because it has a name of its own
    /// that its header, the registry and the transcript all share. An agent
    /// with no such idea gets the name scope keeps for it.
    pub fn start_session(&mut self, spec: &NewSpec) -> Result<String, String> {
        let chosen = spec.agent.as_deref().unwrap_or(agents::CLAUDE.id);
        let known = agents::find(chosen);
        let agent = known.unwrap_or(agents::CLAUDE);
        let argv = match known {
            Some(a) => agents::command(
                a,
                spec.model.as_deref(),
                spec.effort.as_deref(),
                spec.mode.as_deref(),
            ),
            // Not an agent scope knows: run it as typed, which is how anything
            // else local gets to be a session too.
            None => agents::custom_command(chosen),
        };
        let names_itself = known.map(|a| a.transcripts).unwrap_or(false);
        let mut opening = Vec::new();
        if let (Some(name), true) = (&spec.name, names_itself) {
            opening.push(format!("/rename {name}"));
        }
        if let Some(p) = &spec.prompt {
            opening.push(p.clone());
        }
        let path = PathBuf::from(expand(&spec.path));
        let session = control::new_session_with(&path, &argv, &opening)?;
        if let (Some(name), false) = (&spec.name, names_itself) {
            self.name_pane(&session, name);
        }
        let _ = agent;
        self.discover();
        Ok(session)
    }

    /// Remember what to call a session that has no name of its own. Anything
    /// but Claude Code is a program in a terminal as far as scope can tell, so
    /// the name is scope's to keep.
    pub fn name_pane(&mut self, session: &str, name: &str) {
        self.names.insert(session.to_string(), name.to_string());
        save_names(&self.names);
    }

    /// Give a session a name of your own.
    ///
    /// A running session renames itself: `/rename` is a real command, so typing
    /// it is the honest route and everything downstream — its own header, the
    /// registry, the transcript — stays in step. A session that has stopped has
    /// nobody to type to, so scope appends the same record Claude Code would
    /// have written, which is where the name actually lives.
    pub fn rename(&mut self, id: &str, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a name cannot be empty".into());
        }
        if name.contains(['\n', '\r']) {
            return Err("a name has to be one line".into());
        }
        let name = crate::event::clip(name, 80);
        if let Some(p) = self.pane_of(id).cloned() {
            if agents::of_command(&p.cmd)
                .map(|a| a.transcripts)
                .unwrap_or(false)
            {
                control::send_text(&p.id, &format!("/rename {name}"))?;
            } else {
                self.name_pane(&p.session, &name);
            }
            self.say(format!("renamed to {name}"));
        } else {
            let Some(path) = self
                .sessions
                .iter()
                .find(|s| s.id == id && !s.placeholder)
                .map(|s| s.path.clone())
            else {
                return Err("that session has nothing to rename yet".into());
            };
            write_title(&path, id, &name)?;
            self.say(format!(
                "renamed to {name} — it will show that when reopened"
            ));
        }
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
            s.title = name.clone();
            s.titled = true;
        }
        Ok(())
    }

    /// Close a session, one key.
    ///
    /// Stopping used to want the word "yes" typed, from when a stopped session
    /// was gone for good. It is not: the conversation reopens with `A`. The
    /// only thing a keystroke can still cost is a turn in flight, so that is
    /// the only case that asks twice.
    pub fn stop_now(&mut self) -> Result<(), String> {
        let Some((id, name, busy)) = self.current().map(|s| {
            (
                s.id.clone(),
                s.label(),
                matches!(s.status(), Status::Running(_) | Status::Working),
            )
        }) else {
            return Err("no session selected".into());
        };
        let confirmed = self
            .stop_asked
            .as_ref()
            .map(|(asked, at)| *asked == id && at.elapsed() < Duration::from_secs(5))
            .unwrap_or(false);
        if busy && !confirmed {
            self.stop_asked = Some((id, Instant::now()));
            return Err(format!("{name} is mid-turn — x again to close it"));
        }
        self.stop_asked = None;
        self.stop_session();
        Ok(())
    }

    /// Type a line into one session and submit it. Every front end sends this
    /// way, so what "sent" means cannot differ between them.
    pub fn send_to(&mut self, id: &str, text: &str) -> Result<(), String> {
        let busy = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| matches!(s.status(), Status::Running(_) | Status::Working))
            .unwrap_or(false);
        let Some(p) = self.pane_of(id).cloned() else {
            return Err(self.not_steerable());
        };
        control::send_text(&p.id, text)?;
        // Claude Code holds typed input until the current turn ends, which
        // looks identical to a delivered message unless it is said out loud.
        if busy {
            self.say(format!(
                "queued for {} — it is mid-turn and will pick this up after",
                p.session
            ));
        } else {
            self.say(format!("sent to {}", p.session));
        }
        Ok(())
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
            None => {
                let msg = self.not_steerable();
                self.say(msg)
            }
        }
    }

    /// Show a session full-screen. Where scope hosts the session itself there
    /// is no terminal to hand over, so full-screen is scope's own mirror with
    /// every key going to the session — the same thing, drawn by scope.
    pub fn attach(&mut self) {
        let Some(id) = self.current().map(|s| s.id.clone()) else {
            return;
        };
        match self.pane_of(&id) {
            Some(_) if control::hosted_count() > 0 => {
                if !self.passthrough {
                    self.toggle_passthrough();
                }
            }
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
            // Three groups, each ordered by when it started: whatever is
            // blocked on a person, then everything still running, then what has
            // finished. Within a group nothing moves, so the cursor stays put.
            let group = |s: &Session| -> u8 {
                if blocked.contains(&s.id) {
                    0
                } else if matches!(s.status(), Status::Ended) {
                    2
                } else {
                    1
                }
            };
            let now = chrono::Utc::now();
            group(a)
                .cmp(&group(b))
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

    /// Continue a conversation inside tmux so it becomes steerable.
    ///
    /// This is also the way back into a session that has stopped: `claude
    /// --resume` picks up a finished conversation exactly as it picks up a
    /// running one, so nothing that has a transcript is ever a dead end.
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
        let original = self.current().and_then(|s| s.live.as_ref().map(|l| l.pid));
        self.reopen(id, cwd, original);
    }

    /// Bring one conversation up somewhere scope can steer it, whether it is
    /// still running or finished months ago. `original` is the process holding
    /// it now, if any: two clients on one conversation would both append to the
    /// same transcript, so it goes as soon as the new one is up.
    pub fn reopen(&mut self, id: String, cwd: String, original: Option<i64>) {
        if self.steer.contains_key(&id) {
            self.say("already steerable");
            return;
        }
        if let Some(p) = control::adopted_pane(&id, &control::panes()) {
            let name = p.session;
            self.say(format!("already open as {name} — press a to watch it"));
            return;
        }
        if !self.may_spawn() {
            return;
        }
        let was_running = original.is_some();
        // A session's directory may have been removed since — an isolated
        // checkout, or a folder that simply moved. Older tmux refuses to start
        // in a folder that is gone and newer tmux quietly opens somewhere else;
        // a conversation is worth more than the directory it began in, so pick
        // home deliberately and say so.
        let (dir, moved) = match PathBuf::from(&cwd) {
            p if p.is_dir() => (p, String::new()),
            _ => (
                dirs_home(),
                format!(
                    " · {cwd} is gone, so it opened in {}",
                    dirs_home().display()
                ),
            ),
        };
        match control::adopt(dir.as_path(), &id) {
            Ok(name) => {
                let closed = original.map(control::end_process).unwrap_or(false);
                // The window that just closed was probably the one being
                // watched, so put an equivalent one straight back.
                let reopened = control::open_window(&name).is_ok();
                let attach = control::attach_hint(&name);
                self.say(
                    match (was_running, closed, reopened) {
                        (false, _, true) => format!("reopened as {name} in a new window"),
                        (false, _, false) => format!("reopened as {name} — {attach}"),
                        (true, true, true) => {
                            format!(
                                "{name} moved into {} — reopened in a new window",
                                control::WHERE
                            )
                        }
                        (true, true, false) => {
                            format!("{name} moved into {} — {attach}", control::WHERE)
                        }
                        (true, false, _) => {
                            format!("resumed as {name} — close the old window yourself")
                        }
                    } + &moved,
                );
                self.discover();
                // Put the cursor on what was just brought back.
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                    self.sel = i;
                }
            }
            Err(e) => self.say(e),
        }
    }

    /// Open the conversation browser: everything on this machine, whenever it
    /// happened. Scanning is done here rather than on a timer — it reads every
    /// transcript directory, and it is only worth doing when it is asked for.
    pub fn open_past(&mut self) {
        self.past = history::scan(&self.root);
        self.past_open = true;
        self.past_sel = 0;
        self.past_top = 0;
        self.past_filter.clear();
        let n = self.past.len();
        self.say(format!("{n} conversation{}", if n == 1 { "" } else { "s" }));
    }

    /// The conversations the filter leaves, newest first.
    pub fn past_hits(&self) -> Vec<&Past> {
        history::matching(&self.past, &self.past_filter)
    }

    pub fn move_past(&mut self, delta: isize) {
        let n = self.past_hits().len();
        if n == 0 {
            self.past_sel = 0;
            return;
        }
        let next = self.past_sel as isize + delta;
        self.past_sel = next.clamp(0, n as isize - 1) as usize;
    }

    pub fn filter_past(&mut self, edit: impl FnOnce(&mut String)) {
        edit(&mut self.past_filter);
        self.past_sel = 0;
        self.past_top = 0;
    }

    /// Bring the highlighted conversation back. One that is already open is
    /// selected rather than started twice.
    pub fn resume_past(&mut self) {
        let picked = self
            .past_hits()
            .get(self.past_sel)
            .map(|p| (p.id.clone(), p.cwd.clone()));
        let Some((id, cwd)) = picked else {
            self.say("nothing to resume");
            return;
        };
        self.past_open = false;
        if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
            if self.steer.contains_key(&id) {
                self.sel = i;
                self.say("that one is already open");
                return;
            }
        }
        // A conversation still held by a process outside scope has to be taken
        // from it, exactly as adopting does.
        let original = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.live.as_ref().map(|l| l.pid));
        let cwd = if cwd.is_empty() { ".".to_string() } else { cwd };
        self.reopen(id, cwd, original);
    }

    /// What can be done to the selected session right now. Disabled entries
    /// carry the reason, which is usually also the fix.
    pub fn actions(&mut self) -> Vec<Action> {
        let Some(s) = self.current() else {
            return Vec::new();
        };
        let (id, cwd) = (s.id.clone(), s.cwd.clone());
        let live = s.live.is_some() || s.in_pane;
        // A conversation with a transcript on disk can always be reopened,
        // running or not: `claude --resume` continues it either way. Only a
        // session that never wrote anything has nothing to go back to.
        let has_transcript = !s.placeholder;
        let name = s.label();
        let steerable = self.steer.contains_key(&id);
        let blocked = self.approvals.contains_key(&id);
        let iso = self.isolation();
        let in_repo =
            !cwd.is_empty() && crate::git::repo_root(std::path::Path::new(&cwd)).is_some();

        let why_not_steerable = if !live && has_transcript {
            "this session has ended — reopen it first".to_string()
        } else if !live {
            "this session has ended".to_string()
        } else {
            control::steer_hint(&name)
        };
        let why_steer_open = why_not_steerable.clone();
        let why_steer = why_not_steerable;
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
                label: if control::hosted_count() > 0 {
                    "Watch it full-screen and type into it"
                } else {
                    "Attach full-screen"
                },
                enabled: steerable,
                why: why_steer,
            },
            Action {
                key: 'A',
                label: if live {
                    "Adopt into tmux so it can be steered"
                } else {
                    "Reopen this conversation in tmux"
                },
                enabled: !steerable && (live || has_transcript),
                why: if steerable {
                    "already steerable".into()
                } else {
                    "this session never wrote anything to reopen".into()
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
            key: 'N',
            label: "Rename it",
            enabled: true,
            why: String::new(),
        });
        v.push(Action {
            key: 'x',
            label: "Close this session",
            enabled: steerable,
            why: "only sessions scope can reach can be stopped".into(),
        });
        v.push(Action {
            key: 'O',
            label: "Open it in its own window",
            enabled: steerable,
            why: why_steer_open,
        });
        v.push(Action {
            key: 'Z',
            label: "Stop everything scope started",
            enabled: !self.steer.is_empty(),
            why: "nothing of scope's is running".into(),
        });
        v.push(Action {
            key: 'R',
            label: "Resume any conversation on this machine",
            enabled: true,
            why: String::new(),
        });
        v.push(Action {
            key: 'P',
            label: "Tidy up finished scope sessions",
            enabled: self.tmux_ok,
            why: control::unavailable_hint().into(),
        });
        v.push(Action {
            key: 'n',
            label: "Start a new session",
            enabled: self.tmux_ok,
            why: control::unavailable_hint().into(),
        });
        v.push(Action {
            key: 'W',
            label: "Start one isolated on its own branch",
            enabled: self.tmux_ok && in_repo,
            why: if self.tmux_ok {
                "this session is not inside a git repository".into()
            } else {
                control::unavailable_hint().into()
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
            'x' => {
                if let Err(e) = self.stop_now() {
                    self.say(e);
                }
            }
            'N' => self.open_input(Prompt::Rename),
            'Z' => self.open_input(Prompt::StopAll),
            'R' => self.open_past(),
            'O' => {
                let Some(id) = self.current().map(|s| s.id.clone()) else {
                    return;
                };
                match self.steer.get(&id).cloned() {
                    Some(p) => match control::open_window(&p.session) {
                        Ok(term) => self.say(format!("opened {} in {term}", p.session)),
                        Err(e) => self.say(e),
                    },
                    None => {
                        let msg = self.not_steerable();
                        self.say(msg);
                    }
                }
            }
            'P' => {
                let closed = control::prune();
                self.say(match closed.len() {
                    0 => "nothing to tidy up — everything scope started is still running".into(),
                    _ => format!("closed {}", closed.join(", ")),
                });
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
            // Typing into a session means watching it, so passthrough takes
            // over the view; stopping gives back the one that was there.
            self.view_before = Some(self.view);
            self.view = View::Mirror;
            self.passthrough = true;
            self.say("passthrough on — ctrl+] or F12 to stop");
        } else {
            self.passthrough = false;
            if let Some(v) = self.view_before.take() {
                self.view = v;
            }
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
        let _ = control::forward_key(&pane.id, code, ctrl);
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
            Ok(dir) => {
                let spec = NewSpec {
                    path: dir.to_string_lossy().into_owned(),
                    ..Default::default()
                };
                match self.start_session(&spec) {
                    Ok(name) => self.say(format!("{name} on branch {branch} in {}", dir.display())),
                    Err(e) => self.say(e),
                }
            }
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
                self.say(format!("closed {} — press A to reopen it", pane.session));
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
            let mut cwd = expand(&g("cwd").unwrap_or_else(|| ".".into()));
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
            let spec = NewSpec {
                path: cwd,
                agent: g("agent"),
                name: g("name"),
                model: g("model"),
                effort: g("effort"),
                mode: g("permission_mode"),
                prompt: g("prompt"),
            };
            match self.start_session(&spec) {
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

    /// Move the cursor in whichever right-hand pane is showing.
    pub fn move_right(&mut self, delta: isize) {
        match self.view {
            View::Files => self.move_files(delta),
            View::Feed => self.move_feed(delta),
            View::Plan | View::Stats | View::Mirror => {}
            // the reading view scrolls by lines rather than by item
            View::Read => {
                let next = (self.list_top_right as isize + delta * 3).max(0);
                self.list_top_right = next as usize;
            }
            _ => {
                let next = (self.list_sel as isize + delta).max(0);
                self.list_sel = next as usize;
            }
        }
    }

    /// Put the right-hand cursor on a specific row, as a click does.
    pub fn point_right(&mut self, index: usize) -> bool {
        match self.view {
            View::Feed => {
                let len = self.feed_len();
                if index >= len {
                    return false;
                }
                let already = !self.follow && self.feed_sel == index;
                self.feed_sel = index;
                self.follow = index + 1 == len;
                already
            }
            View::Files => {
                if index >= self.file_keys().len() {
                    return false;
                }
                let already = self.file_sel == index;
                self.file_sel = index;
                already
            }
            View::Plan | View::Stats | View::Mirror | View::Read => false,
            _ => {
                let already = self.list_sel == index;
                self.list_sel = index;
                already
            }
        }
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

/// Home, or the current directory when there is no home to speak of.
fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_root() -> PathBuf {
    config_dir().join("projects")
}

pub fn default_sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_agent_and_a_name_off_the_line() {
        let spec = parse_new("~/api --agent codex --name refactor fix the auth tests");
        assert_eq!(spec.agent.as_deref(), Some("codex"));
        assert_eq!(spec.name.as_deref(), Some("refactor"));
        assert_eq!(spec.prompt.as_deref(), Some("fix the auth tests"));
        // Nothing said, so it is Claude Code and scope asks for a name.
        let plain = parse_new("~/api");
        assert!(plain.agent.is_none() && plain.name.is_none());
    }

    #[test]
    fn reads_a_new_session_line() {
        // Just a folder.
        assert_eq!(
            parse_new("~/api"),
            NewSpec {
                path: "~/api".into(),
                ..Default::default()
            }
        );
        // Folder, options, and a message that needs no quoting.
        assert_eq!(
            parse_new("~/api --model opus --effort high fix the failing tests"),
            NewSpec {
                path: "~/api".into(),
                model: Some("opus".into()),
                effort: Some("high".into()),
                prompt: Some("fix the failing tests".into()),
                ..Default::default()
            }
        );
        // No folder given: here, which is what an empty line has always meant.
        assert_eq!(parse_new("").path, ".");
        assert_eq!(
            parse_new("--mode plan"),
            NewSpec {
                path: ".".into(),
                mode: Some("plan".into()),
                ..Default::default()
            }
        );
        // A message that starts with a dash still survives, after --.
        assert_eq!(
            parse_new("~/api -- --model is what I meant to say").prompt,
            Some("--model is what I meant to say".into())
        );
        // A flag with nothing after it is ignored rather than eating the line.
        assert_eq!(parse_new("~/api --model").model, None);
    }

    #[test]
    fn a_name_written_here_is_read_back_as_a_name() {
        // The record scope appends and the record Claude Code appends are the
        // same record, so the test is: write one, then read it the way every
        // other part of scope reads a title.
        let dir = std::env::temp_dir().join(format!("scope-rename-{}", std::process::id()));
        let project = dir.join("-home-someone");
        std::fs::create_dir_all(&project).unwrap();
        let id = "aaaa1111-0000-0000-0000-000000000000";
        let path = project.join(format!("{id}.jsonl"));
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"cwd\":\"/home/someone/api\",\"message\":{\"role\":\"user\",\"content\":\"start\"}}\n\
             {\"type\":\"ai-title\",\"aiTitle\":\"Something Claude thought of\"}\n",
        )
        .unwrap();

        write_title(&path, id, "the rate limiter one").expect("append");

        let found = crate::history::scan(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].title, "the rate limiter one",
            "a chosen name outranks the derived one"
        );

        // And the transcript is still a transcript: every line parses.
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            serde_json::from_str::<serde_json::Value>(line).expect("valid json line");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expands_home_in_a_typed_path() {
        let home = dirs_home().to_string_lossy().into_owned();
        assert_eq!(expand("~/api"), format!("{home}/api"));
        assert_eq!(expand("~"), home);
        assert_eq!(expand("/tmp/x"), "/tmp/x");
        assert_eq!(expand("relative/path"), "relative/path");
        // Not a home reference, so it is left alone.
        assert_eq!(expand("~notauser/x"), "~notauser/x");
    }
}
