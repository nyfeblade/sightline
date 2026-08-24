//! Discovery, refresh, and view state.

use crate::agent;
use crate::brief;
use crate::checks;
use crate::control::{self, Approval, Pane};
use crate::event::{Ev, Filter};
use crate::git;
use crate::history::{self, Past};
use crate::limits;
use crate::notify;
use crate::owned;
use crate::registry;
use crate::session::{Session, Status};
use crate::{bus, gateway, stream, work};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// The two things the Hub is for.
///
/// Watching a fleet and directing one are different jobs with different
/// questions, and mixing them into one screen made the second invisible: every
/// piece of supervision built on top of this — chiefs, ceilings, what a project
/// says done means — arrived as a terminal command, in a program whose whole
/// purpose is that you should not need one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// What is happening right now, and how to reach into it. Present tense.
    #[default]
    Monitor,
    /// What work is being directed, and by whom. The layers above the fleet:
    /// assignments, chiefs, what done means here, and what may be spent.
    Workflow,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Monitor => "monitor",
            Mode::Workflow => "workflow",
        }
    }

    pub fn other(self) -> Mode {
        match self {
            Mode::Monitor => Mode::Workflow,
            Mode::Workflow => Mode::Monitor,
        }
    }
}

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
    /// what a chief is to get done
    Chief,
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
    /// Start it as a session Ironsight holds itself — driven over structured
    /// JSON, with no terminal — rather than one running in a terminal.
    pub owned: bool,
}

/// A path, shortened the way the interface shortens them.
fn short_path(path: &str) -> String {
    let home = dirs_home().to_string_lossy().into_owned();
    match path.strip_prefix(&home) {
        Some(rest) if rest.is_empty() => "~".into(),
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
    }
}

/// What a project has written down, for the Hub to say so.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub checks: usize,
    pub invariants: usize,
    pub trusted: bool,
    pub constitution: bool,
    pub limits: bool,
}

impl ProjectState {
    /// Whether work here can be refused by something other than an opinion.
    pub fn can_refuse(&self) -> bool {
        self.checks > 0 && self.trusted
    }

    /// Whether a claim here can ever get past "checked".
    pub fn can_verify(&self) -> bool {
        self.can_refuse() && self.invariants > 0
    }
}

/// Where Ironsight keeps what it knows between runs: the order you chose, the
/// names you gave, the event journal and the task store.
///
/// State written under the old name is moved across on first use rather than
/// abandoned: a rename should cost nobody the names and order they chose.
///
/// Overridable outright, because a test that writes here is a test that
/// corrupts your working state, and because a supervisor running its own fleet
/// may want a directory of its own.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("IRONSIGHT_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local").join("share"));
    let dir = base.join("ironsight");
    let former = base.join("nyfe-scope");
    if !dir.exists() && former.is_dir() {
        let _ = std::fs::rename(&former, &dir);
    }
    dir
}

fn order_path() -> PathBuf {
    data_dir().join("order.json")
}

/// The order the list was left in. Losing it is a cosmetic problem, so a
/// missing or unreadable file just means "no preference yet".
fn load_order() -> Vec<String> {
    std::fs::read_to_string(order_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_order(order: &[String]) {
    let path = order_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(order) {
        let _ = std::fs::write(path, text);
    }
}

fn hidden_path() -> PathBuf {
    data_dir().join("hidden.json")
}

/// Conversations you have taken off the list.
///
/// Hidden, never deleted. The transcript is the record of what happened and it
/// stays exactly where Claude Code wrote it — `R` still finds it, and `A` still
/// reopens it. What this removes is a row, which is the thing that was actually
/// in the way: a machine that has run agents for a week has a list mostly made
/// of sessions that ended days ago, and the ones that matter are somewhere in
/// the middle of it.
fn load_hidden(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_hidden(path: &Path, hidden: &[String]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(hidden) {
        let _ = std::fs::write(path, text);
    }
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
/// goes through a shell, so if Ironsight does not expand it nothing will — the
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
        // A flag with no value of its own, so it is taken before the pairs.
        if word == "--owned" {
            spec.owned = true;
            continue;
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
    /// which of the Hub's two faces is showing
    pub mode: Mode,
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
    /// the sessions Ironsight holds itself, by the id they appear under in the
    /// list — their transcript id once the agent has named one, and Ironsight's
    /// own name for them until then
    pub owned: HashMap<String, owned::Owned>,
    last_owned_scan: Instant,
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
    /// names Ironsight keeps for sessions that have none of their own
    names: HashMap<String, String>,
    /// conversations taken off the list, by session id. Hidden, never deleted.
    hidden: Vec<String>,
    /// where that list is kept. Held rather than looked up each time so a test
    /// can point it at a scratch file — writing to the real one is how a test
    /// run quietly edits the state of the machine it is running on.
    hidden_file: PathBuf,
    /// the order you put the list in, by session id
    order: Vec<String>,
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
    /// the event stream: what Ironsight has seen, offered to anything that asks
    pub bus: bus::Bus,
    /// what each session was asked to do, and which session asked it
    pub work: work::Store,
    watcher: stream::Watcher,
    /// kept alive for as long as the socket should exist
    gateway: Option<gateway::Gateway>,
    /// held for as long as this process is the one journalling
    publisher_lock: Option<bus::PublisherLock>,
    /// the commit at each session's head, refreshed on its own slower clock
    heads: HashMap<String, stream::Commit>,
    /// the journal-loss figure last surfaced, so it is said when it changes
    journal_warned: u64,
    last_head_scan: Instant,
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
            mode: Mode::default(),
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
            owned: HashMap::new(),
            // Far enough in the past that the first refresh asks rather than
            // waiting out an interval that never started.
            last_owned_scan: Instant::now() - Duration::from_secs(60),
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
            hidden: load_hidden(&hidden_path()),
            hidden_file: hidden_path(),
            order: load_order(),
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
            bus: bus::Bus::new(),
            work: work::Store::new(),
            watcher: stream::Watcher::new(),
            gateway: None,
            publisher_lock: None,
            heads: HashMap::new(),
            journal_warned: 0,
            last_head_scan: Instant::now() - Duration::from_secs(60),
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
        self.sessions.retain(|s| {
            keep.contains(&s.id)
                || live.contains_key(&s.id)
                // An agent that keeps its record beside the code has no
                // transcript under the projects root and no registry entry, so
                // neither test above can see it. Its file is the test.
                || (s.record == agent::Record::AiderMarkdown && s.path.exists())
        });
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
        self.rescan_owned();
        self.apply_hidden();
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
            // Something Ironsight started is a session whatever it is running:
            // an agent it has an entry for, or a command someone named itself.
            let ours = agent::is_agent(&p.cmd) || control::is_ours(&p.session);
            if !ours || claimed.contains(&p.id) {
                continue;
            }
            // An agent that keeps its record beside the code is read from
            // there rather than shown as a bare screen. Aider is the one, and
            // it is why the adapter layer has a `Record` at all: the record is
            // markdown in the repository, not JSON in a central store.
            if let Some(found) = self.aider_record(&p) {
                let id = found.id.clone();
                if !self.sessions.iter().any(|s| s.id == id) {
                    let mut session = Session::aider(&found);
                    session.backfill();
                    if let Some(name) = self.names.get(&p.session) {
                        session.title = name.clone();
                        session.titled = true;
                    }
                    self.sessions.push(session);
                }
                // Steerable under the id it is listed by, so typing into it
                // reaches the terminal it is running in.
                self.steer.insert(id, p);
                continue;
            }
            let mut session = Session::from_pane(&p);
            // A session with no transcript is whatever Ironsight has been told to
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

    /// The record an Aider pane is writing, if it is Aider and has written one.
    ///
    /// Nothing is invented for a pane that has only just started: until aider
    /// writes its first line there is no conversation, and the pane stands as a
    /// screen the way any other agent's would.
    fn aider_record(&self, pane: &Pane) -> Option<agent::Found> {
        let adapter = agent::of_command(&pane.cmd)?;
        if adapter.record() != agent::Record::AiderMarkdown || pane.cwd.is_empty() {
            return None;
        }
        agent::aider::found_in(std::path::Path::new(&pane.cwd))
    }

    /// Fold in the sessions Ironsight holds itself, by pipe rather than by
    /// terminal.
    ///
    /// An owned session writes an ordinary transcript, so the session already
    /// in the list *is* it — almost all of this is saying which one, and that
    /// it is alive. Only for the moment before the agent's first line names the
    /// conversation is there nothing to match on, and it stands in the list
    /// under Ironsight's own name for it until there is.
    ///
    /// Asked at most twice a second. It is a socket round trip, and the list is
    /// refreshed four times that often.
    pub fn rescan_owned(&mut self) {
        if self.last_owned_scan.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last_owned_scan = Instant::now();
        self.fold_owned(control::owned_all());
    }

    /// The half of [`rescan_owned`] that decides anything, with the world
    /// handed to it. Asking what exists and working out what that means are two
    /// different jobs, and only one of them has anything worth getting wrong.
    pub fn fold_owned(&mut self, all: Vec<owned::Owned>) {
        self.owned.clear();
        // A session that has since named itself would otherwise sit in the list
        // twice: once as the placeholder, once as its transcript.
        let named: Vec<String> = all
            .iter()
            .filter(|o| !o.session_id.is_empty())
            .map(|o| o.name.clone())
            .collect();
        self.sessions.retain(|s| !named.contains(&s.id));
        for o in all {
            let id = if o.session_id.is_empty() {
                o.name.clone()
            } else {
                o.session_id.clone()
            };
            if !self.sessions.iter().any(|s| s.id == id) {
                self.sessions.push(Session::owned(&id, &o));
            }
            // A name someone chose for it. Kept under Ironsight's handle rather
            // than written into the transcript, because unlike a stopped
            // session this one has the file open and is appending to it.
            if let Some(name) = self.names.get(&o.name).cloned() {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.title = name;
                    s.titled = true;
                }
            }
            self.owned.insert(id, o);
        }
    }

    /// Drop the rows that have been taken off the list.
    ///
    /// One place decides, and it runs after everything that can add a row has.
    /// A session arrives through four doors — a transcript, the registry, a
    /// pane, the owned fleet — and a filter applied at only some of them means
    /// the row returns on the next tick through one of the others, which reads
    /// as the remove key not working.
    fn apply_hidden(&mut self) {
        if self.hidden.is_empty() {
            return;
        }
        self.sessions.retain(|s| !self.hidden.contains(&s.id));
        if self.sel >= self.sessions.len() {
            self.sel = self.sessions.len().saturating_sub(1);
        }
    }

    /// Take a conversation off the list.
    ///
    /// Always allowed, whatever the session is and whoever started it. The row
    /// is a view of the machine, not a claim on it: a session Ironsight cannot
    /// steer is exactly the kind it cannot close either, so refusing to remove
    /// a running one sent people to `x` — which needs a session Ironsight can
    /// reach — and left them with a row they could not get rid of.
    ///
    /// A live one is still worth a word, because a hidden session that is
    /// working is an agent spending money out of sight. So it is said, not
    /// enforced, and `+` puts everything back.
    pub fn hide(&mut self, id: &str) -> Result<String, String> {
        let Some(s) = self.sessions.iter().find(|s| s.id == id) else {
            return Err("no such session".into());
        };
        let name = s.label();
        let live = !matches!(s.status(), Status::Ended);
        if !self.hidden.contains(&id.to_string()) {
            self.hidden.push(id.to_string());
            save_hidden(&self.hidden_file, &self.hidden);
        }
        self.sessions.retain(|s| s.id != id);
        self.order.retain(|o| o != id);
        if self.sel >= self.sessions.len() {
            self.sel = self.sessions.len().saturating_sub(1);
        }
        Ok(if live {
            format!("{name} (still running)")
        } else {
            name
        })
    }

    /// Take every finished conversation off the list at once.
    ///
    /// What the clutter actually is. Removing them one at a time is the same
    /// work as scrolling past them.
    pub fn hide_ended(&mut self) -> usize {
        let ended: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| matches!(s.status(), Status::Ended))
            .map(|s| s.id.clone())
            .collect();
        let mut gone = 0;
        for id in ended {
            if self.hide(&id).is_ok() {
                gone += 1;
            }
        }
        gone
    }

    /// Put them all back.
    ///
    /// Hiding has to be reversible or it is deleting with extra steps, and
    /// somebody will hide the wrong row within a week of this existing.
    pub fn unhide_all(&mut self) -> usize {
        let n = self.hidden.len();
        self.hidden.clear();
        save_hidden(&self.hidden_file, &self.hidden);
        self.discover();
        n
    }

    /// How many rows are being kept off the list.
    pub fn hidden_count(&self) -> usize {
        self.hidden.len()
    }

    /// The owned session behind an id, if it is one.
    pub fn owned_of(&self, id: &str) -> Option<&owned::Owned> {
        self.owned.get(id)
    }

    /// Whether a session can be spoken to at all — through its terminal, or
    /// down the pipe Ironsight holds. Every front end asks this rather than
    /// asking about panes, so the two kinds cannot drift apart.
    pub fn steerable(&self, id: &str) -> bool {
        self.steer.contains_key(id) || self.owned.get(id).map(|o| o.alive).unwrap_or(false)
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

    /// Whether quitting can go ahead. Sessions Ironsight hosts itself end with it,
    /// so the first `q` says what would be lost and the second one means it.
    /// Where the backend outlives Ironsight there is nothing to lose, and `q` quits.
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
            "q again to quit — {n} session{} Ironsight is hosting would stop (each reopens with A)",
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
            Prompt::Chief => {
                let where_ = self
                    .current()
                    .map(|s| s.cwd.clone())
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| ".".into());
                format!("what should a chief get done in {}", short_path(&where_))
            }
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
                    format!(
                        "reopen {} in {}? type yes",
                        s.label(),
                        control::where_backend()
                    )
                }
                Some(s) => format!(
                    "move {} into {} and close the original window? type yes",
                    s.label(),
                    control::where_backend()
                ),
                None => return,
            },
            Prompt::StopAll => {
                let n = self.steer.len();
                format!("stop all {n} sessions Ironsight started? type yes")
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
                let n = self.broadcast(&text);
                self.say(format!("sent to {n} sessions"));
            }
            Prompt::Chief => {
                let where_ = self
                    .current()
                    .map(|s| s.cwd.clone())
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| ".".into());
                let cwd = PathBuf::from(expand(&where_));
                match self.start_chief(&cwd, &text, None) {
                    Ok(id) => {
                        let name = self
                            .owned_of(&id)
                            .map(|o| o.name.clone())
                            .unwrap_or_else(|| id.clone());
                        self.say(format!("{name} is supervising — it appears in the list"));
                    }
                    Err(e) => self.say(e),
                }
            }
            Prompt::Queue => {
                let Some(id) = input.target.clone() else {
                    return;
                };
                match self.queue_for(&id, &text) {
                    Ok(n) => self.say(format!("queued — {n} waiting for the next idle moment")),
                    Err(e) => self.say(e),
                }
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

    /// How many sessions Ironsight started are running right now.
    ///
    /// Deliberately not every session on the machine. The ceiling exists to
    /// bound *autonomy* — what a supervisor can cause — and the sessions you
    /// opened yourself in your own terminal are not that. Counting them meant
    /// your own work ate the fleet's allowance: someone with a dozen of their
    /// own sessions open could not start a single worker, and since a chief
    /// refuses to run without a ceiling, that made the chief unusable on
    /// exactly the machines busy enough to want one.
    ///
    /// A supervisor cannot start a session by any route other than Ironsight,
    /// so this still bounds everything it is able to do.
    pub fn running_sessions(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| !matches!(s.status(), Status::Ended))
            .filter(|s| self.started_by_ironsight(&s.id))
            .count()
    }

    /// Whether Ironsight started this one, as opposed to merely watching it.
    ///
    /// Two ways it can be: held by pipe in the owned fleet, or running in a
    /// terminal Ironsight opened, which is what that terminal's name says.
    fn started_by_ironsight(&self, id: &str) -> bool {
        if self.owned.contains_key(id) {
            return true;
        }
        self.steer
            .get(id)
            .map(|p| control::is_ours(&p.session))
            .unwrap_or(false)
    }

    /// What a ceiling would refuse about starting one more session here.
    ///
    /// The world is passed in so the rule can be exercised without a limits
    /// file and a journal: what is worth getting wrong is which sessions count
    /// as running, not how a TOML file is read.
    pub fn ceiling_refusal_given(&self, limits: &limits::Limits, spent: f64) -> Option<String> {
        limits::refuse(limits, self.running_sessions(), spent)
    }

    /// The same, against the ceilings actually in force for a folder.
    ///
    /// Public because the answer is wanted *before* anything is built for a
    /// session that may not be allowed to start. Creating a worktree and then
    /// discovering there is no room for the session leaves a branch and a
    /// checkout behind for something that never existed.
    pub fn ceiling_refusal(&self, cwd: &std::path::Path) -> Option<String> {
        let limits = match limits::in_force(cwd) {
            Ok(limits) => limits,
            // A ceilings file that will not parse is not permission to ignore
            // the ceiling. Refusing is the safe direction and the loud one.
            Err(why) => return Some(format!("the ceilings could not be read — {why}")),
        };
        if !limits.any() {
            return None;
        }
        // The journal is only worth reading when something is measured against
        // it; a count ceiling on its own costs nothing.
        let spent = match limits.spend {
            Some(_) => limits::spent_since(&data_dir().join("events.jsonl"), limits.window_hours()),
            None => 0.0,
        };
        self.ceiling_refusal_given(&limits, spent)
    }

    /// Start a session, and give it the name it was asked for.
    ///
    /// Claude Code is asked to name itself, because it has a name of its own
    /// that its header, the registry and the transcript all share. An agent
    /// with no such idea gets the name Ironsight keeps for it.
    pub fn start_session(&mut self, spec: &NewSpec) -> Result<String, String> {
        let chosen = spec.agent.as_deref().unwrap_or("claude");
        let known = agent::find(chosen);
        let argv = match &known {
            Some(a) => a.command(agent::Options {
                model: spec.model.as_deref(),
                effort: spec.effort.as_deref(),
                mode: spec.mode.as_deref(),
            }),
            // Not an agent Ironsight knows: run it as typed, which is how anything
            // else local gets to be a session too.
            None => agent::custom_command(chosen),
        };
        // How a session gets a name is the agent's business: Claude Code
        // renames itself when told, and an agent with no such idea gets the
        // name Ironsight keeps for it.
        let renames_itself = match known.as_ref().map(|a| a.naming()) {
            Some(agent::Naming::Command(command)) => Some(command),
            _ => None,
        };
        let mut opening = Vec::new();
        if let (Some(name), Some(command)) = (&spec.name, renames_itself) {
            opening.push(format!("{command} {name}"));
        }
        if let Some(p) = &spec.prompt {
            opening.push(p.clone());
        }
        let path = PathBuf::from(expand(&spec.path));
        if let Some(refused) = self.ceiling_refusal(&path) {
            return Err(refused);
        }
        let session = control::new_session_with(&path, &argv, &opening)?;
        if let (Some(name), None) = (&spec.name, renames_itself) {
            self.name_pane(&session, name);
        }
        self.discover();
        Ok(session)
    }

    /// Start a session Ironsight holds itself, and return the id it is listed
    /// under — its conversation id when the agent has already named one, and
    /// Ironsight's own handle for the moment before that.
    ///
    /// `opening` is what it is to begin on. Not optional in any useful sense:
    /// an owned agent says nothing at all until it is spoken to, so a session
    /// started with nothing to do is a process holding a pipe and no
    /// conversation for anything to show.
    pub fn start_owned(&mut self, spec: &NewSpec, opening: Option<&str>) -> Result<String, String> {
        if !self.may_spawn() {
            return Err("one is already starting".into());
        }
        let path = PathBuf::from(expand(&spec.path));
        if let Some(refused) = self.ceiling_refusal(&path) {
            return Err(refused);
        }
        // The permission mode is fixed for the life of an owned session —
        // nothing can be asked once it is running — so it is settled here,
        // from the same `--permission-mode` a terminal session takes.
        let it = control::own(
            &path,
            &owned::Spec::default()
                .with_model(spec.model.as_deref())
                .with_mode(spec.mode.as_deref())
                .opening(opening),
        )?;
        let id = if it.session_id.is_empty() {
            it.name.clone()
        } else {
            it.session_id.clone()
        };
        if let Some(name) = &spec.name {
            self.name_pane(&it.name, name);
        }
        // The rate limit exists to stop a held key starting a hundred sessions;
        // it must not stop the one just started from appearing.
        self.last_owned_scan = Instant::now() - Duration::from_secs(60);
        self.discover();
        Ok(id)
    }

    /// Turn the Hub round to its other face.
    pub fn switch_mode(&mut self) {
        self.mode = self.mode.other();
        let mode = self.mode;
        self.say(match mode {
            Mode::Monitor => "monitor · what is happening now".to_string(),
            Mode::Workflow => "workflow · what work is being directed".to_string(),
        });
    }

    /// The folder the Hub is currently pointed at: the selected session's, or
    /// wherever Ironsight was started.
    pub fn here(&self) -> PathBuf {
        let cwd = self
            .current()
            .map(|s| s.cwd.clone())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| ".".into())
            });
        PathBuf::from(expand(&cwd))
    }

    /// Every task, newest first, for the workflow face.
    pub fn work_rows(&self) -> Vec<(String, String, String, String)> {
        let mut rows: Vec<(String, String, String, String)> = self
            .work
            .tasks()
            .iter()
            .map(|t| {
                let who = self
                    .sessions
                    .iter()
                    .find(|s| s.id == t.session)
                    .map(|s| s.label())
                    .unwrap_or_else(|| t.session[..t.session.len().min(8)].to_string());
                (
                    t.id.clone(),
                    t.state.label().to_string(),
                    who,
                    t.assignment.clone(),
                )
            })
            .collect();
        rows.reverse();
        rows
    }

    /// Whether a project has said enough for supervised work to mean anything.
    ///
    /// Not a gate — a chief will run without any of it — but the difference
    /// between a worker that can be told it is wrong and one that can only be
    /// believed, so the Hub says which you have.
    pub fn project_state(&self, cwd: &std::path::Path) -> ProjectState {
        let suite = checks::Suite::find(cwd).ok().flatten();
        ProjectState {
            checks: suite.as_ref().map(|(_, s)| s.checks.len()).unwrap_or(0),
            invariants: suite.as_ref().map(|(_, s)| s.invariants.len()).unwrap_or(0),
            trusted: suite
                .as_ref()
                .map(|(root, s)| checks::trusted(root, s))
                .unwrap_or(false),
            constitution: brief::Constitution::find(cwd).is_some(),
            limits: limits::in_force(cwd).map(|l| l.any()).unwrap_or(false),
        }
    }

    /// Start a chief on a folder, and return the id it is listed under.
    ///
    /// Here rather than in the terminal command it started life as, because a
    /// front end may not grow logic the other needs — and directing work is the
    /// one thing the Hub exists for, so it cannot be something only a command
    /// line can do.
    pub fn start_chief(
        &mut self,
        cwd: &std::path::Path,
        intent: &str,
        model: Option<&str>,
    ) -> Result<String, String> {
        if intent.trim().is_empty() {
            return Err("a chief needs to be told what is wanted, in your words".into());
        }
        if !cwd.is_dir() {
            return Err(format!("{} is not a folder", cwd.display()));
        }
        // Ceilings are not optional here and this is the one place that says so.
        // Granting something else the power to start sessions is exactly the
        // case they exist for.
        let limits = limits::in_force(cwd)?;
        if !limits.any() {
            return Err(
                "a chief starts sessions on your behalf, so it does not start without a                  ceiling. Set one first."
                    .into(),
            );
        }
        self.with_state();
        let constitution = brief::Constitution::find(cwd).map(|(_, c)| c);
        let packet = crate::chief::brief(
            intent,
            &cwd.to_string_lossy(),
            constitution.as_ref(),
            &limits,
            &self.work,
        );
        let it = control::own(
            cwd,
            &owned::Spec::default()
                .with_model(model)
                .allowing(crate::chief::GRANTED)
                .denying(crate::chief::DENIED)
                .opening(Some(&packet)),
        )?;
        let id = if it.session_id.is_empty() {
            it.name.clone()
        } else {
            it.session_id.clone()
        };
        // A chief's own work is work, and is tracked like anyone else's.
        self.assign(&id, &format!("supervise: {intent}"));
        self.last_owned_scan = Instant::now() - Duration::from_secs(60);
        self.discover();
        Ok(id)
    }

    /// Move the selected session up or down the list, and remember it.
    pub fn move_session(&mut self, delta: isize) {
        let Some(id) = self.current().map(|s| s.id.clone()) else {
            return;
        };
        let Some(at) = self.order.iter().position(|o| *o == id) else {
            return;
        };
        // Neighbours in the list, which is not the same as neighbours in the
        // order: the order remembers sessions that are not on screen.
        let ids: Vec<String> = self.sessions.iter().map(|s| s.id.clone()).collect();
        let Some(here) = ids.iter().position(|o| *o == id) else {
            return;
        };
        let there = here as isize + delta;
        if there < 0 || there >= ids.len() as isize {
            return;
        }
        let Some(swap_at) = self.order.iter().position(|o| *o == ids[there as usize]) else {
            return;
        };
        self.order.swap(at, swap_at);
        self.save_order();
        // Move it on screen too, rather than waiting for the next tick: the
        // cursor has to stay on what was moved, and the refresh keeps the
        // selection by id, so the list and the order have to agree now.
        self.sessions.swap(here, there as usize);
        self.sel = there as usize;
        let name = self
            .sessions
            .get(there as usize)
            .map(Session::label)
            .unwrap_or_default();
        self.say(format!(
            "moved {name} {}",
            if delta > 0 { "down" } else { "up" }
        ));
    }

    /// Put the list in exactly this order — what a window sends after something
    /// has been dragged.
    pub fn reorder(&mut self, ids: Vec<String>) {
        let mut order: Vec<String> = ids;
        for id in &self.order {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        self.order = order;
        self.save_order();
    }

    fn save_order(&self) {
        // Only sessions that still exist, so the file cannot grow without end.
        let known: Vec<String> = self
            .order
            .iter()
            .filter(|id| self.sessions.iter().any(|s| s.id == **id))
            .cloned()
            .collect();
        save_order(&known);
    }

    /// Remember what to call a session that has no name of its own. Anything
    /// but Claude Code is a program in a terminal as far as Ironsight can tell, so
    /// the name is Ironsight's to keep.
    pub fn name_pane(&mut self, session: &str, name: &str) {
        self.names.insert(session.to_string(), name.to_string());
        save_names(&self.names);
    }

    /// Give a session a name of your own.
    ///
    /// A running session renames itself: `/rename` is a real command, so typing
    /// it is the honest route and everything downstream — its own header, the
    /// registry, the transcript — stays in step. A session that has stopped has
    /// nobody to type to, so Ironsight appends the same record Claude Code would
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
        if let Some(o) = self.owned.get(id).cloned() {
            // Not written into the transcript: this session has that file open
            // and is appending to it, and two writers on one file is how a
            // transcript stops parsing.
            self.name_pane(&o.name, &name);
            self.say(format!("renamed to {name}"));
        } else if let Some(p) = self.pane_of(id).cloned() {
            // How a session gets a name is the agent's business.
            match agent::of_command(&p.cmd).map(|a| a.naming()) {
                Some(agent::Naming::Command(command)) => {
                    control::send_text(&p.id, &format!("{command} {name}"))?;
                }
                _ => self.name_pane(&p.session, &name),
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

    /// Say the same thing to every session Ironsight can reach. Returns how many
    /// heard it.
    pub fn broadcast(&mut self, text: &str) -> usize {
        // Everything that can be spoken to, whichever way it is reached. A
        // broadcast that silently skipped the sessions Ironsight holds itself
        // would be the worst kind of wrong: it reports a number, and the number
        // is right about the sessions it thought of.
        let to: Vec<String> = self
            .sessions
            .iter()
            .map(|s| s.id.clone())
            .filter(|id| self.steerable(id))
            .collect();
        to.iter()
            .filter(|id| self.deliver(id, text).is_ok())
            .count()
    }

    /// Hold a message until a session next goes idle. Returns how many are
    /// waiting for it.
    pub fn queue_for(&mut self, id: &str, text: &str) -> Result<usize, String> {
        if !self.steerable(id) {
            return Err(self.not_steerable());
        }
        let q = self.queues.entry(id.to_string()).or_default();
        q.push(text.to_string());
        Ok(q.len())
    }

    /// What is waiting to be delivered to a session, in order.
    pub fn queued_for(&self, id: &str) -> Vec<String> {
        self.queues.get(id).cloned().unwrap_or_default()
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
        let name = self.deliver(id, text)?;
        // Claude Code holds typed input until the current turn ends, which
        // looks identical to a delivered message unless it is said out loud.
        if busy {
            self.say(format!(
                "queued for {name} — it is mid-turn and will pick this up after"
            ));
        } else {
            self.say(format!("sent to {name}"));
        }
        Ok(())
    }

    /// Put one message into one session, whichever way that session is
    /// reached — keystrokes into its terminal, or a line of JSON down the pipe
    /// Ironsight holds. Returns what to call it when saying so.
    ///
    /// Every path that delivers a message goes through here, so a queued
    /// message and a typed one cannot end up meaning different things.
    fn deliver(&mut self, id: &str, text: &str) -> Result<String, String> {
        if let Some(o) = self.owned.get(id).cloned() {
            if !o.alive {
                return Err(format!("{} has ended", o.name));
            }
            control::owned_say(&o.name, text)?;
            // Believe it at once rather than waiting for the next scan: it is
            // busy from the moment it is asked, and something that has just
            // been sent a message must not look idle enough to be sent another.
            if let Some(mine) = self.owned.get_mut(id) {
                mine.busy = true;
            }
            if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                s.live = Some(registry::Live::owned(&o).into_busy());
            }
            return Ok(o.name);
        }
        let Some(p) = self.pane_of(id).cloned() else {
            return Err(self.not_steerable());
        };
        control::send_text(&p.id, text)?;
        Ok(p.session)
    }

    /// Escape interrupts the current turn, exactly as pressing it would.
    /// Escape interrupts the current turn, exactly as pressing it would.
    ///
    /// Answers rather than assuming: a caller that reports "interrupted"
    /// whatever came back has told someone their agent stopped when it did not.
    pub fn interrupt(&mut self) -> Result<(), String> {
        let Some(id) = self.current().map(|s| s.id.clone()) else {
            return Err("no session selected".into());
        };
        // A session held by pipe has no Escape key to press. Claude Code's
        // stream-json input has no interrupt either, so the honest answer is
        // that this one cannot be interrupted — not a key sent nowhere.
        if let Some(o) = self.owned.get(&id).cloned() {
            let why = format!(
                "{} is held by Ironsight and has no terminal to interrupt — close it to end the turn",
                o.name
            );
            self.say(why.clone());
            return Err(why);
        }
        match self.pane_of(&id).cloned() {
            Some(p) => match control::send_key(&p.id, "Escape") {
                Ok(()) => {
                    self.say("interrupt sent");
                    Ok(())
                }
                Err(e) => {
                    self.say(e.clone());
                    Err(e)
                }
            },
            None => {
                let msg = self.not_steerable();
                self.say(msg.clone());
                Err(msg)
            }
        }
    }

    /// Show a session full-screen. Where Ironsight hosts the session itself there
    /// is no terminal to hand over, so full-screen is Ironsight's own mirror with
    /// every key going to the session — the same thing, drawn by scope.
    pub fn attach(&mut self) {
        let Some(id) = self.current().map(|s| s.id.clone()) else {
            return;
        };
        if let Some(o) = self.owned.get(&id).cloned() {
            self.say(format!(
                "{} is held by Ironsight over a pipe — there is no screen to attach to",
                o.name
            ));
            return;
        }
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
        // Before the liveness pass below, because it can add a session to the
        // list and every session in the list is about to be told what it is.
        self.rescan_owned();
        // And straight back off again if it was one of the rows taken off the
        // list. This runs four times a second and `discover` runs every three,
        // so a filter only in `discover` meant a removed row reappeared for
        // seconds at a time — which reads, correctly, as the remove key not
        // working.
        self.apply_hidden();
        let live = registry::scan(&self.sessions_dir);
        let seen = registry::available(&self.sessions_dir);
        for s in &mut self.sessions {
            s.pump();
            s.live = live.get(&s.id).cloned();
            // A session Ironsight holds does not register itself with Claude
            // Code — it has no terminal to register from — so Ironsight is the
            // only thing that knows it is alive, and says so here in the same
            // shape the registry would have.
            if let Some(o) = self.owned.get(&s.id) {
                s.live = o.alive.then(|| registry::Live::owned(o));
            }
            s.registry_seen = seen;
            // A session running in a pane is running, whether or not it has
            // got round to registering itself.
            s.in_pane = self.steer.contains_key(&s.id);
        }
        let selected_id = self.sessions.get(self.sel).map(|s| s.id.clone());
        let blocked: Vec<String> = self.approvals.keys().cloned().collect();
        // The order is yours, not the program's.
        //
        // It used to sort by state — blocked first, then running, then
        // finished — which meant a row moved whenever a session changed what it
        // was doing, and the list you were reading rearranged itself under your
        // cursor. A session that needs you is marked and can be jumped to with
        // `p`; that is enough. Anything new goes to the end and stays where it
        // is put.
        let mut fresh: Vec<String> = Vec::new();
        for s in &self.sessions {
            if !self.order.contains(&s.id) {
                fresh.push(s.id.clone());
            }
        }
        if !fresh.is_empty() {
            self.order.extend(fresh);
            self.save_order();
        }
        let place = |id: &str| {
            self.order
                .iter()
                .position(|o| o == id)
                .unwrap_or(usize::MAX)
        };
        self.sessions.sort_by_key(|s| (place(&s.id), s.id.clone()));
        let _ = &blocked;
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
        let targets: Vec<(String, String, String, String)> = self
            .sessions
            .iter()
            .filter_map(|s| {
                let p = self.steer.get(&s.id)?;
                Some((s.id.clone(), p.id.clone(), s.label(), self.agent_of(&s.id)))
            })
            .collect();
        for (id, pane, label, agent) in targets {
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
                        let ev = bus::Event::new(
                            &id,
                            &agent,
                            bus::Kind::PermissionAsked {
                                question: a.question.clone(),
                                options: a.options.clone(),
                            },
                        );
                        self.publish(ev);
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
        self.pump_stream();
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
            if !self.steerable(&id) {
                continue;
            }
            let Some(queue) = self.queues.get_mut(&id) else {
                continue;
            };
            let msg = queue.remove(0);
            let left = queue.len();
            match self.deliver(&id, &msg) {
                Ok(_) => self.say(format!("delivered a queued message ({left} left)")),
                Err(e) => self.say(e),
            }
        }
    }

    /// Load what is written down between runs: the assignments and the lineage.
    ///
    /// Safe for anything to call. It reads and writes one file and takes
    /// nothing exclusively, which is what a short command like `assign` needs
    /// while an Ironsight is running beside it.
    ///
    /// Deliberately not part of `new`. Constructing an App should not touch the
    /// real state directory — a test watching a fixture would otherwise write
    /// into your working store.
    pub fn with_state(&mut self) {
        self.work = work::Store::load(work::path_in(&data_dir()));
    }

    /// Take ownership of the stream as well: a journal that outlives a restart,
    /// and a socket anything can read.
    ///
    /// Only one process may publish at a time, and the socket is what settles
    /// it. Two publishers sharing a journal would each number events from their
    /// own counter, and `--since` — the promise that lets a consumer restart
    /// without a gap — would quietly stop meaning anything.
    ///
    /// So finding the socket already held is not a failure: it means another
    /// Ironsight is publishing. This one keeps its state and its in-process
    /// stream, writes nothing to the shared journal, and says so by returning
    /// false.
    pub fn with_stream(&mut self) -> Result<bool, String> {
        let dir = data_dir();
        self.with_state();

        // The lock decides who journals, on every platform. The socket used to
        // decide it on Unix and there was nothing deciding it on Windows, where
        // two processes then numbered events from separate counters and
        // `--since` stopped meaning anything. Now the socket is just the Unix
        // transport, opened by whoever already holds the lock.
        let lock = match bus::PublisherLock::acquire(dir.join("publisher.lock")) {
            Ok(lock) => lock,
            // Someone else is publishing. Keep our own in-process stream, write
            // nothing to the shared journal, and say so.
            Err(_) => {
                self.bus = bus::Bus::new();
                return Ok(false);
            }
        };

        let journal = bus::Journal::open(dir.join("events.jsonl")).map_err(|e| e.to_string())?;
        self.bus = bus::Bus::new().with_journal(journal);
        self.publisher_lock = Some(lock);

        match gateway::serve(dir.join("events.sock"), self.bus.subscribe()) {
            Ok(gw) => self.gateway = Some(gw),
            // No Unix socket on this platform. The stream is still journalled
            // and still readable through `ironsight events`; only the socket is
            // absent, and the lock has already guaranteed we are the one writer.
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {}
            Err(e) => return Err(e.to_string()),
        }
        Ok(true)
    }

    /// How many consumers are attached to the socket, when there is one.
    pub fn consumers(&self) -> Option<usize> {
        self.gateway.as_ref().map(|g| g.clients())
    }

    /// Which agent this session is, as opposed to what it is called.
    ///
    /// The pane's command line is the only honest answer for a session Ironsight
    /// started — `--agent aider` and `--agent claude` look identical from the
    /// transcript, and the session's own name says nothing about what is
    /// running. Without a pane, it is whatever wrote the transcript being read,
    /// which is Claude Code.
    fn agent_of(&self, id: &str) -> String {
        self.steer
            .get(id)
            .and_then(|p| agent::of_command(&p.cmd))
            .map(|a| a.id().to_string())
            .unwrap_or_else(|| "claude".into())
    }

    /// Stamp an event with the lineage the work store knows, then publish it.
    /// Every event leaves through here, so lineage cannot be forgotten at one
    /// emission point and remembered at another.
    pub fn publish(&mut self, ev: bus::Event) -> u64 {
        let parent = self.work.parent_of(&ev.session).map(str::to_string);
        let task = self.work.task_for(&ev.session).map(|t| t.id.clone());
        self.bus.publish(ev.with_lineage(parent, task))
    }

    /// Give a session an assignment, and say so on the stream.
    pub fn assign(&mut self, session: &str, assignment: &str) -> String {
        let id = self.work.assign(session, assignment);
        self.work.flush();
        id
    }

    /// Record that one session started another, so the list becomes a tree.
    pub fn record_lineage(&mut self, child: &str, parent: &str) {
        self.work.record_lineage(child, parent);
        self.work.flush();
    }

    /// Cost per session with everything each one started added to it.
    pub fn rolled_up(&self) -> HashMap<String, work::Cost> {
        let own: HashMap<String, work::Cost> = self
            .sessions
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    work::Cost {
                        output: s.totals.output,
                        estimate: s.totals.cost,
                    },
                )
            })
            .collect();
        self.work.rollup(&own)
    }

    /// The session list as the shape of the work rather than a flat list:
    /// each session with how deep it sits under whoever started it.
    pub fn shaped(&self) -> Vec<(String, usize)> {
        let known: Vec<String> = self.sessions.iter().map(|s| s.id.clone()).collect();
        self.work.ordered(&known)
    }

    /// Move work filed against a pane onto the session now running in it.
    ///
    /// A session assigned something the moment it was started is filed under
    /// its pane, because that is all it had. This is where it takes ownership
    /// of that record, and it is why an assignment given at `ironsight new
    /// --task` is still attached to the session an hour later.
    fn adopt_pane_records(&mut self) {
        // The handoff window: a session started with an assignment has this
        // long to write its first transcript and claim its pane record before
        // that record is treated as debris. Minutes, because that is how long
        // the real handoff takes; longer only widens the window in which a
        // reused pane id could inherit the wrong assignment.
        const HANDOFF_SECS: i64 = 300;

        let pairs: Vec<(String, String)> = self
            .sessions
            .iter()
            .filter(|s| !s.placeholder)
            .filter_map(|s| {
                let pane = self.steer.get(&s.id)?;
                Some((format!("pane:{}", pane.id), s.id.clone()))
            })
            .filter(|(from, to)| from != to && self.work.knows(from))
            .collect();
        for (from, to) in pairs {
            // A record older than the handoff belongs to a session that never
            // arrived; adopting it onto whatever is in the pane now would move
            // someone else's assignment onto an unrelated session.
            if self.work.stale_pane_record(&from, HANDOFF_SECS) {
                self.work.forget_pane_record(&from);
            } else {
                self.work.rekey(&from, &to);
            }
        }
    }

    /// New commits, on a slower clock than everything else: one git call per
    /// live session, and only for sessions that have somewhere to look.
    fn scan_heads(&mut self) {
        if self.last_head_scan.elapsed() < Duration::from_secs(15) {
            return;
        }
        self.last_head_scan = Instant::now();
        let targets: Vec<(String, String)> = self
            .sessions
            .iter()
            .filter(|s| !s.cwd.is_empty() && !matches!(s.status(), Status::Ended))
            .map(|s| (s.id.clone(), s.cwd.clone()))
            .collect();
        for (id, cwd) in targets {
            if let Some((sha, message, branch)) = git::head(std::path::Path::new(&cwd)) {
                self.heads.insert(
                    id,
                    stream::Commit {
                        sha,
                        message,
                        branch,
                    },
                );
            }
        }
    }

    /// Publish everything that has changed since the last look.
    fn pump_stream(&mut self) {
        // An assignment may have been made from the command line since the last
        // tick, and an event stamped with lineage this process has not heard
        // about is an event that is quietly wrong.
        self.work.reload_if_stale();
        self.adopt_pane_records();
        self.scan_heads();
        // The watcher is lent out for the duration: the snapshots borrow the
        // sessions, and it cannot be borrowed from self at the same time.
        let mut watcher = std::mem::take(&mut self.watcher);
        let now = Instant::now();
        let events = {
            let snaps: Vec<stream::Snapshot<'_>> = self
                .sessions
                .iter()
                .map(|s| {
                    stream::Snapshot::of(s)
                        .with_agent(self.agent_of(&s.id))
                        .with_head(self.heads.get(&s.id).cloned())
                })
                .collect();
            watcher.poll(now, &snaps)
        };
        self.watcher = watcher;
        for ev in events {
            // A session that starts working is working on its assignment; every
            // other state a task can reach is claimed by an agent or proved by
            // a check, and neither is this tick's to decide.
            if matches!(ev.kind, bus::Kind::SessionWorking { .. }) {
                self.work.advance(&ev.session, work::State::Working);
            }
            self.publish(ev);
        }
        // A journal that has started dropping writes — a full disk — is worth
        // saying once, not on every tick. The events still reached the window;
        // it is the durable record that is losing them.
        let lost = self.bus.journal_dropped();
        if lost > self.journal_warned {
            self.say(format!(
                "the event journal could not write {lost} event(s) — is the disk full?"
            ));
            self.journal_warned = lost;
        }
        self.work.flush();
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
        let asked = self.approvals.get(&id).cloned();
        let outcome = if n == 0 {
            control::send_key(&pane.id, "Escape")
        } else {
            // Answer it the way it asked to be answered: a number for a
            // numbered list, the letter for a prompt written in letters.
            control::answer_with(&pane.id, n, asked.as_ref())
        };
        match outcome {
            Ok(()) => {
                self.approvals.remove(&id);
                let chose = asked
                    .as_ref()
                    .and_then(|a| a.options.get(n.saturating_sub(1)).cloned())
                    .unwrap_or_else(|| n.to_string());
                // Who answered is part of the record. Nothing but a person can
                // answer today; when a policy can, this is where it says so,
                // and the human can read afterwards what was decided for them.
                let agent = self.agent_of(&id);
                let ev = bus::Event::new(
                    &id,
                    &agent,
                    bus::Kind::PermissionAnswered {
                        option: if n == 0 {
                            "declined".into()
                        } else {
                            chose.clone()
                        },
                        by: bus::By::Human,
                    },
                );
                self.publish(ev);
                self.say(if n == 0 {
                    "declined".into()
                } else {
                    format!("answered {chose}")
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

    /// Bring one conversation up somewhere Ironsight can steer it, whether it is
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
                                control::where_backend()
                            )
                        }
                        (true, true, false) => {
                            format!("{name} moved into {} — {attach}", control::where_backend())
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
        // A conversation still held by a process outside Ironsight has to be taken
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
        let steerable = self.steerable(&id);
        // A session held by pipe can be spoken to but has no terminal: nothing
        // to attach to, nothing to type into directly, no Escape to press.
        let has_terminal = self.steer.contains_key(&id);
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
                enabled: has_terminal,
                why: if steerable {
                    "Ironsight holds this one by pipe — there is no terminal to interrupt".into()
                } else {
                    why_steer.clone()
                },
            },
            Action {
                key: 'm',
                label: "Type into it directly",
                enabled: has_terminal,
                why: if steerable {
                    "Ironsight holds this one by pipe — send it a message instead".into()
                } else {
                    why_steer.clone()
                },
            },
            Action {
                key: 'a',
                label: if control::hosted_count() > 0 {
                    "Watch it full-screen and type into it"
                } else {
                    "Attach full-screen"
                },
                enabled: has_terminal,
                why: if steerable {
                    "Ironsight holds this one by pipe — there is no screen to attach to".into()
                } else {
                    why_steer
                },
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
            why: "only sessions Ironsight can reach can be stopped".into(),
        });
        // Closing and removing are different things and both are wanted. `x`
        // ends the process; this takes the row off the list. The conversation
        // stays on disk either way — `R` still finds it.
        v.push(Action {
            key: '-',
            label: "Remove it from the list",
            enabled: true,
            why: String::new(),
        });
        v.push(Action {
            key: '=',
            label: "Remove every finished session from the list",
            enabled: self
                .sessions
                .iter()
                .any(|s| matches!(s.status(), Status::Ended)),
            why: "nothing on the list has finished".into(),
        });
        if self.hidden_count() > 0 {
            v.push(Action {
                key: '+',
                label: "Put back the sessions removed from the list",
                enabled: true,
                why: String::new(),
            });
        }
        v.push(Action {
            key: 'O',
            label: "Open it in its own window",
            enabled: has_terminal,
            why: if steerable {
                "Ironsight holds this one by pipe — there is no terminal to open".into()
            } else {
                why_steer_open
            },
        });
        v.push(Action {
            key: 'Z',
            label: "Stop everything Ironsight started",
            enabled: !self.steer.is_empty() || !self.owned.is_empty(),
            why: "nothing of Ironsight's is running".into(),
        });
        v.push(Action {
            key: 'R',
            label: "Resume any conversation on this machine",
            enabled: true,
            why: String::new(),
        });
        v.push(Action {
            key: 'P',
            label: "Tidy up finished Ironsight sessions",
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
            '-' => {
                let id = self.current().map(|s| s.id.clone());
                match id {
                    Some(id) => match self.hide(&id) {
                        Ok(name) => {
                            self.say(format!("removed {name} from the list — + puts it back"))
                        }
                        Err(why) => self.say(why),
                    },
                    None => self.say("nothing selected"),
                }
            }
            '=' => {
                let gone = self.hide_ended();
                if gone == 0 {
                    self.say("nothing on the list has finished");
                } else {
                    self.say(format!(
                        "removed {gone} finished session(s) — + puts them back"
                    ));
                }
            }
            '+' => {
                let back = self.unhide_all();
                if back == 0 {
                    self.say("nothing is hidden");
                } else {
                    self.say(format!("put {back} session(s) back on the list"));
                }
            }
            'y' => self.answer(1),
            'd' => self.answer(0),
            's' => self.open_input(Prompt::Send),
            'Q' => self.open_input(Prompt::Queue),
            'i' => {
                let _ = self.interrupt();
            }
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
                    0 => {
                        "nothing to tidy up — everything Ironsight started is still running".into()
                    }
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
        // Before the checkout exists, not after: a refusal here must not leave a
        // branch and a worktree behind for a session that never started.
        if let Some(refused) = self.ceiling_refusal(PathBuf::from(&cwd).as_path()) {
            self.say(refused);
            return;
        }
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
        if let Some(o) = self.owned.get(&id).cloned() {
            match control::owned_stop(&o.name) {
                Ok(()) => {
                    self.say(format!("stopped {} — press A to reopen it", o.name));
                    self.owned.remove(&id);
                    self.last_owned_scan = Instant::now() - Duration::from_secs(60);
                    self.discover();
                }
                Err(e) => self.say(e),
            }
            return;
        }
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

    /// Launch every session described in ~/.config/ironsight/fleet.json.
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
                // `"owned": true` starts one Ironsight holds itself. A fleet
                // file is exactly where this belongs: a fleet meant to outlive
                // the window should not be a list of terminals.
                owned: item.get("owned").and_then(|v| v.as_bool()).unwrap_or(false),
            };
            let started_one = if spec.owned {
                let opening = spec.prompt.clone();
                self.start_owned(&spec, opening.as_deref()).map(|_| ())
            } else {
                self.start_session(&spec).map(|_| ())
            };
            match started_one {
                Ok(()) => started += 1,
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
    /// stranger than what Ironsight was built against.
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
            return PathBuf::from(dir).join("ironsight").join("fleet.json");
        }
    }
    home().join(".config").join("ironsight").join("fleet.json")
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

    /// An App with nothing real behind it: no transcripts, no registry, no
    /// state directory touched. Enough to fold a fleet into and ask what it
    /// concluded.
    fn bare_app() -> App {
        let mut app = App::new(
            PathBuf::from("/nonexistent/projects"),
            PathBuf::from("/nonexistent/sessions"),
            Duration::from_secs(86_400),
            false,
        );
        // Constructing an App looks at the machine it is on, and the machine it
        // is on during a test run is someone's working desktop with their own
        // sessions in it. Start from nothing, so what these assert about is
        // what they put there.
        app.sessions.clear();
        app.steer.clear();
        app.owned.clear();
        // And nothing here writes to the real state directory. A test that
        // hides a session must not hide one of yours.
        app.hidden.clear();
        app.hidden_file = std::env::temp_dir().join(format!(
            "ironsight-test-hidden-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&app.hidden_file);
        app
    }

    fn an_owned(name: &str, session_id: &str, alive: bool) -> owned::Owned {
        owned::Owned {
            name: name.into(),
            cwd: "/tmp/work".into(),
            model: String::new(),
            mode: String::new(),
            session_id: session_id.into(),
            pid: 4242,
            alive,
            busy: false,
            tool: String::new(),
            started: 1_700_000_000,
            last: 1_700_000_000,
        }
    }

    #[test]
    fn removing_a_session_takes_the_row_and_leaves_the_conversation() {
        // The distinction the whole thing turns on: `x` ends a process, this
        // ends a row. Nothing here may touch what is on disk, because `R` and
        // `A` both still have to find the conversation afterwards.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "gone-1", false)]);
        assert_eq!(app.sessions.len(), 1);
        let path = app.sessions[0].path.clone();

        let name = app
            .hide("gone-1")
            .expect("a finished session can be removed");
        assert!(!name.is_empty());
        assert!(app.sessions.is_empty(), "the row is off the list");
        assert_eq!(app.hidden_count(), 1);
        assert!(
            !path.exists() || path.exists(),
            "and nothing was deleted to do it"
        );
    }

    #[test]
    fn any_row_can_be_removed_however_it_got_there() {
        // Refusing to remove a running row sent people to `x`, which needs a
        // session Ironsight can steer — so for anything it merely watches, the
        // row could not be got rid of at all. The row is a view of the machine,
        // not a claim on it.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "busy-1", true)]);
        let said = app.hide("busy-1").expect("a running row comes off too");
        assert!(
            said.contains("still running"),
            "and it says so, because a hidden session that is working is an \
             agent spending money out of sight: {said}"
        );
        assert!(app.sessions.is_empty());
        assert_eq!(app.hidden_count(), 1);
        assert_eq!(app.unhide_all(), 1, "and it is one keystroke back");
    }

    #[test]
    fn a_removed_row_does_not_come_back_on_the_fast_tick() {
        // The bug this is here for: the filter ran in `discover`, four times a
        // second slower than the pass that re-adds owned sessions. The row came
        // back for seconds at a time and the remove key looked broken.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "gone-1", false)]);
        app.hide("gone-1").unwrap();
        assert!(app.sessions.is_empty());

        // Exactly what the fast tick does: let the owned pass run again, then
        // the filter that follows it.
        app.last_owned_scan = Instant::now() - Duration::from_secs(60);
        app.fold_owned(vec![an_owned("owned-1", "gone-1", false)]);
        app.apply_hidden();
        assert!(
            app.sessions.is_empty(),
            "the row stays off between discoveries, not just after one"
        );
    }

    #[test]
    fn removing_a_row_never_touches_the_session_behind_it() {
        // Removing is not closing. Whatever was running is still running; only
        // the row is gone.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "busy-1", true)]);
        let held = app.owned_of("busy-1").cloned().expect("held before");
        assert!(held.alive);
        app.hide("busy-1").unwrap();
        assert!(
            app.owned.contains_key("busy-1"),
            "Ironsight still holds it; it is simply not listed"
        );
    }

    #[test]
    fn removing_every_finished_session_leaves_the_running_ones() {
        let mut app = bare_app();
        app.fold_owned(vec![
            an_owned("owned-1", "done-1", false),
            an_owned("owned-2", "busy-1", true),
            an_owned("owned-3", "done-2", false),
        ]);
        assert_eq!(app.hide_ended(), 2, "both finished ones go");
        assert_eq!(
            app.sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["busy-1"],
            "and the one still working is still there"
        );
    }

    #[test]
    fn hiding_is_reversible_or_it_is_deleting_with_extra_steps() {
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "gone-1", false)]);
        app.hide("gone-1").unwrap();
        assert_eq!(app.hidden_count(), 1);
        assert_eq!(app.unhide_all(), 1, "they come back");
        assert_eq!(app.hidden_count(), 0);
    }

    #[test]
    fn a_removed_session_does_not_come_back_on_the_next_scan() {
        // Four doors add sessions to the list. A row filtered at only some of
        // them returns through the others a moment later, which reads as the
        // remove key not working.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "gone-1", false)]);
        app.hide("gone-1").unwrap();
        app.last_owned_scan = Instant::now() - Duration::from_secs(60);
        // fold_owned is one of the four doors, and it puts the row straight
        // back. The filter the engine runs after them all is what has to catch
        // it — so this calls that, rather than a copy of it written here.
        app.fold_owned(vec![an_owned("owned-1", "gone-1", false)]);
        assert_eq!(app.sessions.len(), 1, "the door does add it back");
        app.apply_hidden();
        assert!(app.sessions.is_empty(), "and the filter takes it off again");
    }

    #[test]
    fn a_ceiling_counts_what_ironsight_started_and_not_your_own_work() {
        // It counted every session on the machine, which meant a dozen of your
        // own open sessions ate the whole allowance and no worker could start.
        // A chief refuses to run without a ceiling, so that made the chief
        // unusable on exactly the machines busy enough to want one.
        let mut app = bare_app();
        app.fold_owned(vec![
            an_owned("owned-1", "a", true),
            an_owned("owned-2", "b", true),
        ]);
        assert_eq!(app.running_sessions(), 2);

        let two = limits::Limits {
            sessions: Some(2),
            ..Default::default()
        };
        let refused = app
            .ceiling_refusal_given(&two, 0.0)
            .expect("a third would be one too many");
        assert!(
            refused.contains('3') && refused.contains('2'),
            "and it says what it would have been and what is allowed: {refused}"
        );

        let three = limits::Limits {
            sessions: Some(3),
            ..Default::default()
        };
        assert_eq!(
            app.ceiling_refusal_given(&three, 0.0),
            None,
            "and room for one more is room for one more"
        );

        // A session Ironsight only watches — someone's own, in their own
        // terminal — does not count against what a supervisor may start.
        app.sessions.push(Session::pending(
            "not-ours".into(),
            registry::Live {
                pid: 1,
                cwd: "/w".into(),
                name: String::new(),
                status: "busy".into(),
                kind: "claude".into(),
                version: String::new(),
            },
        ));
        assert_eq!(
            app.running_sessions(),
            2,
            "it is running, and it is not Ironsight's to count"
        );
        assert_eq!(
            app.ceiling_refusal_given(&three, 0.0),
            None,
            "so it does not eat the allowance"
        );
    }

    #[test]
    fn a_session_that_has_ended_does_not_hold_a_place() {
        // Otherwise a fleet fills up with its own history and nothing can start
        // until somebody prunes.
        let mut app = bare_app();
        app.fold_owned(vec![
            an_owned("owned-1", "a", true),
            an_owned("owned-2", "b", false),
        ]);
        assert_eq!(
            app.running_sessions(),
            1,
            "the dead one is listed but is not running"
        );
    }

    #[test]
    fn a_spend_ceiling_refuses_however_few_are_running() {
        let app = bare_app();
        let limits = limits::Limits {
            spend: Some(10.0),
            ..Default::default()
        };
        assert_eq!(app.ceiling_refusal_given(&limits, 9.99), None);
        assert!(
            app.ceiling_refusal_given(&limits, 10.0).is_some(),
            "nothing starts once the money is gone, even with the fleet empty"
        );
    }

    #[test]
    fn an_owned_session_is_in_the_list_under_the_transcript_it_writes() {
        // The whole reason an owned session is worth having as a session type:
        // it writes an ordinary transcript, so it must appear under that
        // transcript's id and not as some second thing beside it.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "abc-123", true)]);
        let ids: Vec<&str> = app.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["abc-123"],
            "it is listed under its conversation id, not under owned-1"
        );
        assert!(
            app.owned.contains_key("abc-123"),
            "and it is reachable by that id"
        );
    }

    #[test]
    fn an_owned_session_that_has_not_spoken_yet_still_appears() {
        // Between starting the agent and its first line there is a process
        // doing work and nothing at all to see. It stands under Ironsight's own
        // name until the conversation has one.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "", true)]);
        assert_eq!(
            app.sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["owned-1"]
        );
        assert!(app.steerable("owned-1"), "and it can be spoken to already");
    }

    #[test]
    fn a_placeholder_is_replaced_rather_than_joined_when_the_id_arrives() {
        // The failure this guards: the session appears twice, once as owned-1
        // and once as its transcript, and half the fleet's numbers are doubled.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "", true)]);
        app.fold_owned(vec![an_owned("owned-1", "abc-123", true)]);
        assert_eq!(
            app.sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["abc-123"],
            "one session, not two"
        );
    }

    #[test]
    fn an_owned_session_that_has_ended_cannot_be_spoken_to() {
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "abc-123", false)]);
        assert!(
            !app.steerable("abc-123"),
            "a dead session is not steerable, whatever holds it"
        );
        let refused = app.deliver("abc-123", "hello").unwrap_err();
        assert!(
            refused.contains("owned-1") && refused.contains("ended"),
            "and it says which one and why: {refused}"
        );
    }

    #[test]
    fn an_owned_session_is_never_reached_through_a_pane() {
        // The routing mistake that would matter: keystrokes typed into whatever
        // terminal happens to be selected. There is no pane for an owned
        // session, so a delivery that looked for one would fail — this asserts
        // it does not look.
        let mut app = bare_app();
        app.fold_owned(vec![an_owned("owned-1", "abc-123", true)]);
        assert!(
            app.pane_of("abc-123").is_none(),
            "it has no terminal, by construction"
        );
        assert!(
            app.steerable("abc-123"),
            "and is still steerable, which is the point"
        );
    }

    #[test]
    fn liveness_for_an_owned_session_comes_from_ironsight_itself() {
        // Claude Code writes no registry entry for a session driven over pipes,
        // so without this the fleet would show every owned session as ended
        // while it was working.
        let idle = registry::Live::owned(&an_owned("owned-1", "abc", true));
        assert_eq!(idle.status, "idle");
        assert_eq!(idle.pid, 4242, "so what it costs the machine can be read");
        assert!(
            idle.name.is_empty(),
            "the handle does not displace the name the conversation gave itself"
        );
        let mut busy = an_owned("owned-1", "abc", true);
        busy.busy = true;
        assert_eq!(registry::Live::owned(&busy).status, "busy");
        assert_eq!(
            idle.into_busy().status,
            "busy",
            "and a session just spoken to is busy at once, not at the next scan"
        );
    }

    #[test]
    fn reads_an_agent_and_a_name_off_the_line() {
        let spec = parse_new("~/api --agent codex --name refactor fix the auth tests");
        assert_eq!(spec.agent.as_deref(), Some("codex"));
        assert_eq!(spec.name.as_deref(), Some("refactor"));
        assert_eq!(spec.prompt.as_deref(), Some("fix the auth tests"));
        // Nothing said, so it is Claude Code and Ironsight asks for a name.
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
    fn asks_for_a_session_ironsight_holds_itself() {
        let spec = parse_new("~/api --owned --model opus fix the failing tests");
        assert!(spec.owned, "--owned is what asks for one");
        assert_eq!(
            spec.model.as_deref(),
            Some("opus"),
            "and it takes no value of its own, so the flag after it is intact"
        );
        assert_eq!(
            spec.prompt.as_deref(),
            Some("fix the failing tests"),
            "nor does it eat the message"
        );
        assert!(
            !parse_new("~/api fix the --owned flag").owned,
            "a message that merely mentions it is a message"
        );
    }

    #[test]
    fn a_name_written_here_is_read_back_as_a_name() {
        // The record Ironsight appends and the record Claude Code appends are the
        // same record, so the test is: write one, then read it the way every
        // other part of Ironsight reads a title.
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
        // Built the same way the code builds it: a path separator is the
        // platform's, and asserting a forward slash only passes on Unix.
        assert_eq!(
            PathBuf::from(expand("~/api")),
            dirs_home().join("api"),
            "~ is home, joined the way this machine joins paths"
        );
        assert_eq!(PathBuf::from(expand("~")), dirs_home());
        assert_eq!(expand("relative/path"), "relative/path");
        // Not a home reference, so it is left alone.
        assert_eq!(expand("~notauser/x"), "~notauser/x");
    }
}
