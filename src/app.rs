//! Discovery, refresh, and view state.

use crate::event::{Ev, Filter};
use crate::registry;
use crate::session::{Session, Status};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sessions,
    Feed,
}

/// What the right-hand pane shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Feed,
    Files,
    Stats,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Feed => "feed",
            View::Files => "files",
            View::Stats => "stats",
        }
    }

    pub fn next(self) -> Self {
        match self {
            View::Feed => View::Files,
            View::Files => View::Stats,
            View::Stats => View::Feed,
        }
    }
}

pub struct App {
    pub root: PathBuf,
    pub sessions_dir: PathBuf,
    pub sessions: Vec<Session>,
    pub sel: usize,
    pub filter: Filter,
    pub focus: Focus,
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
    pub note: String,
}

impl App {
    pub fn new(root: PathBuf, sessions_dir: PathBuf, since: Duration, only_live: bool) -> Self {
        let mut app = App {
            root,
            sessions_dir,
            sessions: Vec::new(),
            sel: 0,
            filter: Filter::All,
            focus: Focus::Sessions,
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
        let cutoff = SystemTime::now() - self.since;
        let mut out = Vec::new();
        let Ok(projects) = std::fs::read_dir(&self.root) else { return out };
        for project in projects.flatten() {
            let Ok(files) = std::fs::read_dir(project.path()) else { continue };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let is_live = live.contains_key(id);
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
        let keep: Vec<String> = want
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
            .collect();
        self.sessions.retain(|s| keep.contains(&s.id));
        for path in want {
            let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            if self.sessions.iter().any(|s| s.id == id) {
                continue;
            }
            let mut sess = Session::open(path);
            sess.pump();
            self.sessions.push(sess);
        }
        self.last_discover = Instant::now();
    }

    /// Read new transcript lines, re-attach liveness, re-sort.
    pub fn refresh(&mut self) {
        let live = registry::scan(&self.sessions_dir);
        for s in &mut self.sessions {
            s.pump();
            s.live = live.get(&s.id).cloned();
        }
        let selected_id = self.sessions.get(self.sel).map(|s| s.id.clone());
        self.sessions.sort_by(|a, b| {
            fn rank(s: &Session) -> u8 {
                match s.status() {
                    Status::Running(_) | Status::Working => 0,
                    Status::Waiting => 1,
                    Status::Ended => 2,
                }
            }
            rank(a)
                .cmp(&rank(b))
                .then(b.last.cmp(&a.last))
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
        let Some(s) = self.current() else { return Vec::new() };
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
            let Some(slot) = s.slot_of(*abs) else { continue };
            let Some(ev) = s.events.get(slot) else { continue };
            let when = ev
                .ts
                .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
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
        let cur = if self.follow { len as isize - 1 } else { self.feed_sel as isize };
        let next = (cur + delta).clamp(0, len as isize - 1);
        self.feed_sel = next as usize;
        self.follow = next as usize == len - 1;
    }

    /// (output tokens, estimated dollars, sessions currently working)
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

pub fn default_root() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Path::new(&dir).join("projects");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    Path::new(&home).join(".claude").join("projects")
}

pub fn default_sessions_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Path::new(&dir).join("sessions");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    Path::new(&home).join(".claude").join("sessions")
}
