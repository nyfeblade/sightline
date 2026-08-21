// The desktop front end. It owns no logic: every command here is a thin
// translation between the window and `scope-core`, so the app and the terminal
// view cannot answer the same question differently.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use crossterm::event::KeyCode;
use scope_core::app::App;
use scope_core::session::Status;
use scope_core::{app as core_app, bootstrap, control, history, screen, usage};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::State;

/// What the window shares with the engine.
///
/// The lock matters more than it looks. Catching up — pumping every transcript,
/// reading the registry, capturing every steerable pane — used to happen at the
/// top of *every* command, so a keystroke queued behind three hundred megabytes
/// of transcript and a dozen tmux calls before it could be sent. Now catching
/// up is a thing that happens on its own rhythm, and typing is allowed to be
/// nothing more than a write to a pane.
struct Shared {
    app: Mutex<App>,
    meter: Mutex<usage::Meter>,
    /// session id to the pane it lives in, kept by the refresh so that typing
    /// never has to ask the engine anything
    panes: Mutex<HashMap<String, String>>,
    caught_up: Mutex<Instant>,
}

/// How stale the engine's view may be. Fast enough that nothing looks frozen,
/// slow enough that it is not re-read for every frame of a moving screen.
const CATCH_UP: Duration = Duration::from_millis(250);

impl Shared {
    fn new(app: App) -> Self {
        Shared {
            app: Mutex::new(app),
            meter: Mutex::new(usage::Meter::default()),
            panes: Mutex::new(HashMap::new()),
            caught_up: Mutex::new(Instant::now() - CATCH_UP),
        }
    }

    /// The engine, without making it catch up first. A poisoned lock is not
    /// worth taking the window down for.
    fn raw<T>(&self, f: impl FnOnce(&mut App) -> T) -> T {
        let mut app = match self.app.lock() {
            Ok(a) => a,
            Err(e) => e.into_inner(),
        };
        f(&mut app)
    }

    /// The engine, caught up — at most as often as that is worth doing.
    fn fresh<T>(&self, f: impl FnOnce(&mut App) -> T) -> T {
        let mut app = match self.app.lock() {
            Ok(a) => a,
            Err(e) => e.into_inner(),
        };
        let mut at = match self.caught_up.lock() {
            Ok(t) => t,
            Err(e) => e.into_inner(),
        };
        if at.elapsed() >= CATCH_UP {
            app.refresh();
            app.probe();
            *at = Instant::now();
            let mut panes = match self.panes.lock() {
                Ok(p) => p,
                Err(e) => e.into_inner(),
            };
            panes.clear();
            for s in &app.sessions {
                if let Some(pane) = app.pane_of(&s.id) {
                    panes.insert(s.id.clone(), pane.id.clone());
                }
            }
        }
        f(&mut app)
    }

    /// Where a session lives, without touching the engine at all. This is the
    /// whole reason a keystroke is fast.
    fn pane(&self, id: &str) -> Option<String> {
        match self.panes.lock() {
            Ok(p) => p.get(id).cloned(),
            Err(e) => e.into_inner().get(id).cloned(),
        }
    }

    /// Run something against one session by id, leaving the selection where it
    /// was. The engine is written around a cursor; the window is not.
    fn on<T>(&self, id: &str, f: impl FnOnce(&mut App) -> T) -> Option<T> {
        self.raw(|app| {
            let was = app.sel;
            let found = app.sessions.iter().position(|s| s.id == id)?;
            app.sel = found;
            let out = f(app);
            app.sel = was.min(app.sessions.len().saturating_sub(1));
            Some(out)
        })
    }
}

#[derive(Serialize)]
struct CheckDto {
    name: String,
    ok: bool,
    required: bool,
    detail: String,
    fix: Option<String>,
}

#[derive(Serialize)]
struct ReadinessDto {
    ready: bool,
    checks: Vec<CheckDto>,
    /// what holds sessions here: "tmux", or "scope" where it hosts its own
    backend: String,
}

#[derive(Serialize)]
struct SessionDto {
    id: String,
    name: String,
    /// working · waiting · running:<tool> · ended
    state: String,
    tool: Option<String>,
    cwd: String,
    branch: String,
    model: String,
    /// seconds since it last did anything
    age_secs: i64,
    /// seconds since it started, which is a different question
    started_secs: i64,
    context: u64,
    window: u64,
    output: u64,
    requests: u64,
    cost: f64,
    errors: usize,
    turns: usize,
    denials: usize,
    input: u64,
    cache_read: u64,
    cache_write: u64,
    version: String,
    effort: String,
    branch_ahead: Option<usize>,
    /// median and slowest tool call, in milliseconds
    latency: (i64, i64),
    /// share of one processor, once there have been two readings to compare
    cpu: Option<f64>,
    /// how many processors there are, so a share of one can be read as a share
    /// of the machine
    cores: f64,
    /// resident bytes across the session's whole process tree
    memory: u64,
    /// the tools it has reached for, most used first
    tools: Vec<String>,
    steerable: bool,
    live: bool,
    /// the question it is blocked on, if any
    asking: Option<AskDto>,
    /// the tmux session or hosted name, when there is one
    pane: Option<String>,
}

#[derive(Serialize)]
struct AskDto {
    question: String,
    options: Vec<String>,
}

#[derive(Serialize)]
struct EventDto {
    at: String,
    kind: String,
    tool: Option<String>,
    head: String,
    body: String,
}

#[derive(Serialize)]
struct PastDto {
    id: String,
    title: String,
    cwd: String,
    age_secs: i64,
    bytes: u64,
    open: bool,
}

fn state_of(s: &scope_core::session::Session) -> (String, Option<String>) {
    match s.status() {
        Status::Running(tool) => ("running".into(), Some(tool)),
        Status::Working => ("working".into(), None),
        Status::Waiting => ("waiting".into(), None),
        Status::Ended => ("ended".into(), None),
    }
}

#[tauri::command]
fn readiness() -> ReadinessDto {
    let checks = bootstrap::assess(&bootstrap::probe(&core_app::default_root()));
    ReadinessDto {
        ready: bootstrap::ready(&checks),
        backend: control::WHERE.to_string(),
        checks: checks
            .into_iter()
            .map(|c| CheckDto {
                name: c.name.to_string(),
                ok: c.ok,
                required: c.weight == bootstrap::Weight::Required,
                detail: c.detail,
                fix: c.fix,
            })
            .collect(),
    }
}

#[tauri::command]
fn sessions(shared: State<Shared>) -> Vec<SessionDto> {
    let mut meter = match shared.meter.lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    let pids: Vec<i64> = shared.fresh(|app| {
        app.sessions
            .iter()
            .filter_map(|s| {
                s.live
                    .as_ref()
                    .map(|l| l.pid)
                    .or_else(|| app.pane_of(&s.id).map(|p| p.pid))
            })
            .collect()
    });
    let used_by = meter.measure_all(&pids);
    shared.raw(|app| {
        app.sessions
            .iter()
            .map(|s| {
                let (state, tool) = state_of(s);
                let pane = app.pane_of(&s.id).map(|p| p.session.clone());
                // What it is costing the machine, measured from its process
                // tree; nothing writes that down.
                let pid = s
                    .live
                    .as_ref()
                    .map(|l| l.pid)
                    .or_else(|| app.pane_of(&s.id).map(|p| p.pid))
                    .unwrap_or(0);
                let used = used_by.get(&pid).copied().unwrap_or_default();
                let mut tools: Vec<(&String, &usize)> = s.tools.iter().collect();
                tools.sort_by(|a, b| b.1.cmp(a.1));
                SessionDto {
                    id: s.id.clone(),
                    name: s.label(),
                    state,
                    tool,
                    cwd: s.cwd.clone(),
                    branch: s.branch.clone(),
                    model: s.model.clone(),
                    age_secs: s.age_secs(),
                    started_secs: s
                        .started
                        .map(|t| (chrono::Utc::now() - t).num_seconds())
                        .unwrap_or(-1),
                    context: s.totals.ctx,
                    window: s.window(),
                    output: s.totals.output,
                    requests: s.totals.requests as u64,
                    cost: s.totals.cost,
                    errors: s.errors,
                    turns: s.turns,
                    denials: s.denials,
                    input: s.totals.input,
                    cache_read: s.totals.cache_read,
                    cache_write: s.totals.cache_write,
                    version: s.version.clone(),
                    effort: s.effort.clone(),
                    branch_ahead: None,
                    latency: s.latency(),
                    cpu: used.cpu,
                    cores: usage::cores(),
                    memory: used.memory,
                    tools: tools.into_iter().take(6).map(|(n, _)| n.clone()).collect(),
                    steerable: pane.is_some(),
                    live: s.live.is_some() || s.in_pane,
                    asking: app.approvals.get(&s.id).map(|a| AskDto {
                        question: a.question.clone(),
                        options: a.options.clone(),
                    }),
                    pane,
                }
            })
            .collect()
    })
}

#[tauri::command]
fn feed(shared: State<Shared>, id: String, limit: usize) -> Vec<EventDto> {
    shared
        .on(&id, |app| {
            let Some(s) = app.sessions.get(app.sel) else {
                return Vec::new();
            };
            s.events
                .iter()
                .rev()
                .take(limit.clamp(1, 2000))
                .rev()
                .map(|e| EventDto {
                    at: e.ts.map(|t| t.to_rfc3339()).unwrap_or_default(),
                    kind: format!("{:?}", e.kind).to_lowercase(),
                    tool: e.tool.clone(),
                    head: e.head.clone(),
                    body: e.body.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Serialize)]
struct TreeDto {
    branch: String,
    insertions: usize,
    deletions: usize,
    entries: Vec<TreeEntryDto>,
    /// how far ahead of its base branch, when it is working on its own
    ahead: Option<usize>,
    base: Option<String>,
}

#[derive(Serialize)]
struct TreeEntryDto {
    path: String,
    state: String,
}

#[derive(Serialize)]
struct FileDto {
    path: String,
    reads: usize,
    writes: usize,
    edits: usize,
    added: usize,
    removed: usize,
}

#[derive(Serialize)]
struct TodoDto {
    text: String,
    state: String,
}

#[derive(Serialize)]
struct AgentRunDto {
    kind: String,
    description: String,
    model: String,
    state: String,
}

/// Every file the session touched, most recently changed first.
#[tauri::command]
fn files(shared: State<Shared>, id: String) -> Vec<FileDto> {
    shared
        .on(&id, |app| {
            let Some(s) = app.sessions.get(app.sel) else {
                return Vec::new();
            };
            let mut out: Vec<FileDto> = s
                .files
                .iter()
                .map(|(path, t)| FileDto {
                    path: path.clone(),
                    reads: t.reads,
                    writes: t.writes,
                    edits: t.edits,
                    added: t.added,
                    removed: t.removed,
                })
                .collect();
            out.sort_by(|a, b| (b.edits + b.writes).cmp(&(a.edits + a.writes)));
            out
        })
        .unwrap_or_default()
}

/// The state of the working tree it is editing, and how far ahead its branch
/// is when it has one of its own.
#[tauri::command]
fn tree(shared: State<Shared>, id: String) -> Option<TreeDto> {
    shared.on(&id, |app| {
        let iso = app.isolation();
        let tree = app.tree()?;
        Some(TreeDto {
            branch: tree.branch.clone(),
            insertions: tree.insertions,
            deletions: tree.deletions,
            entries: tree
                .entries
                .iter()
                .map(|e| TreeEntryDto {
                    path: e.path.clone(),
                    state: e.code.clone(),
                })
                .collect(),
            ahead: iso.as_ref().map(|i| i.ahead),
            base: iso.map(|i| i.base),
        })
    })?
}

/// Its current plan, from the last time it wrote one.
#[tauri::command]
fn plan(shared: State<Shared>, id: String) -> Vec<TodoDto> {
    shared
        .on(&id, |app| {
            app.sessions
                .get(app.sel)
                .map(|s| {
                    s.todos
                        .iter()
                        .map(|t| TodoDto {
                            text: t.text.clone(),
                            state: t.status.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// The subagents it has launched.
#[tauri::command]
fn agents(shared: State<Shared>, id: String) -> Vec<AgentRunDto> {
    shared
        .on(&id, |app| {
            app.sessions
                .get(app.sel)
                .map(|s| {
                    s.agents
                        .iter()
                        .map(|a| AgentRunDto {
                            kind: a.kind.clone(),
                            description: a.description.clone(),
                            model: a.model.clone(),
                            state: a.status.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// What went wrong, newest last.
#[tauri::command]
fn errors(shared: State<Shared>, id: String) -> Vec<EventDto> {
    shared
        .on(&id, |app| {
            let Some(s) = app.sessions.get(app.sel) else {
                return Vec::new();
            };
            s.events
                .iter()
                .filter(|e| !e.ok)
                .map(|e| EventDto {
                    at: e.ts.map(|t| t.to_rfc3339()).unwrap_or_default(),
                    kind: format!("{:?}", e.kind).to_lowercase(),
                    tool: e.tool.clone(),
                    head: e.head.clone(),
                    body: e.body.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A session's screen, drawn as cells rather than handed over as a terminal.
#[tauri::command]
fn frame(shared: State<Shared>, id: String, cols: u16, rows: u16) -> Option<screen::Frame> {
    let pane = shared.pane(&id)?;
    control::frame(&pane, cols, rows)
}

#[derive(Serialize)]
struct HitDto {
    id: String,
    session: String,
    at: String,
    head: String,
}

/// Every mention of this across every session scope is watching — the same
/// search the terminal view does with `/`.
#[tauri::command]
fn search(shared: State<Shared>, text: String) -> Vec<HitDto> {
    shared.raw(|app| {
        app.run_search(&text);
        app.hits
            .iter()
            .filter_map(|(si, ei)| {
                let s = app.sessions.get(*si)?;
                let e = s.events.get(*ei)?;
                Some(HitDto {
                    id: s.id.clone(),
                    session: s.label(),
                    at: e.ts.map(|t| t.to_rfc3339()).unwrap_or_default(),
                    head: e.head.clone(),
                })
            })
            .take(300)
            .collect()
    })
}

/// Messages held for a session until it next goes idle.
#[tauri::command]
fn queued(shared: State<Shared>, id: String) -> Vec<String> {
    shared.raw(|app| app.queued_for(&id))
}

#[tauri::command]
fn queue(shared: State<Shared>, id: String, text: String) -> Result<usize, String> {
    shared.raw(|app| app.queue_for(&id, &text))
}

/// Say the same thing to every session scope can reach.
#[tauri::command]
fn broadcast(shared: State<Shared>, text: String) -> usize {
    shared.raw(|app| app.broadcast(&text))
}

/// Start a session on a branch of its own, in its own checkout.
#[tauri::command]
fn isolate(shared: State<Shared>, id: String, branch: String) -> String {
    shared
        .on(&id, |app| {
            app.isolate(&branch);
            app.note.clone()
        })
        .unwrap_or_else(|| "no such session".into())
}

/// Merge an isolated session's branch back, or throw its checkout away.
#[tauri::command]
fn merge(shared: State<Shared>, id: String) -> String {
    shared
        .on(&id, |app| {
            app.merge_isolated();
            app.note.clone()
        })
        .unwrap_or_else(|| "no such session".into())
}

#[tauri::command]
fn discard(shared: State<Shared>, id: String) -> String {
    shared
        .on(&id, |app| {
            app.discard_isolated();
            app.note.clone()
        })
        .unwrap_or_else(|| "no such session".into())
}

/// Start everything the fleet file describes.
#[tauri::command]
fn launch_fleet(shared: State<Shared>) -> String {
    bootstrap::ensure_backend().ok();
    shared.raw(|app| {
        app.launch_fleet();
        app.note.clone()
    })
}

/// Where the fleet file is, and what it says.
#[tauri::command]
fn fleet() -> (String, String) {
    let path = core_app::fleet_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    (path.to_string_lossy().into_owned(), text)
}

/// Close everything scope started, or just what has already finished.
#[tauri::command]
fn close_all() -> Vec<String> {
    control::stop_all()
}

#[tauri::command]
fn prune() -> Vec<String> {
    control::prune()
}

/// Desktop notifications, on or off; returns where it landed.
#[tauri::command]
fn notifications(shared: State<Shared>, on: bool) -> bool {
    shared.raw(|app| {
        app.notify_on = on;
        app.notify_on
    })
}

/// Look again now, rather than at the next beat.
#[tauri::command]
fn rescan(shared: State<Shared>) {
    shared.raw(|app| {
        app.discover();
        app.refresh();
    });
}

/// Send one key press to a session, so its own screen can be typed into.
/// Browsers name keys their own way; this is the translation, and anything
/// without a terminal meaning is dropped rather than guessed at.
#[tauri::command]
fn key(shared: State<Shared>, id: String, key: String, ctrl: bool) -> Result<(), String> {
    let code = match key.as_str() {
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Esc,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return Ok(()),
            }
        }
    };
    let pane = shared
        .pane(&id)
        .ok_or("that session cannot be typed into")?;
    control::forward_key(&pane, code, ctrl)
}

/// Open a session in a terminal window of its own.
#[tauri::command]
fn window(shared: State<Shared>, id: String) -> Result<String, String> {
    let pane = shared
        .raw(|app| app.pane_of(&id).map(|p| p.session.clone()))
        .ok_or("scope has no terminal for that session")?;
    control::open_window(&pane)
}

/// Stop holding a session at the window's size, when the window stops showing
/// it.
#[tauri::command]
fn release_frame(shared: State<Shared>, id: String) {
    if let Some(pane) = shared.pane(&id) {
        control::release_frame(&pane);
    }
}

/// What a session's terminal is showing, as plain text.
#[tauri::command]
fn screen(shared: State<Shared>, id: String) -> Option<String> {
    shared.raw(|app| app.mirror.get(&id).cloned())
}

#[tauri::command]
fn send(shared: State<Shared>, id: String, text: String) -> Result<(), String> {
    shared
        .on(&id, |app| app.send_to(&id, &text))
        .unwrap_or_else(|| Err("no such session".into()))
}

#[tauri::command]
fn answer(shared: State<Shared>, id: String, option: usize) -> Result<(), String> {
    shared
        .on(&id, |app| {
            app.answer(option);
            Ok(())
        })
        .unwrap_or_else(|| Err("no such session".into()))
}

#[tauri::command]
fn interrupt(shared: State<Shared>, id: String) -> Result<(), String> {
    shared
        .on(&id, |app| {
            app.interrupt();
            Ok(())
        })
        .unwrap_or_else(|| Err("no such session".into()))
}

/// Start a session. `line` is the same thing the terminal view accepts — a
/// folder and any flags — and `name` is what to call it, which the app asks
/// for in a field rather than a second prompt.
#[tauri::command]
fn start(shared: State<Shared>, line: String, name: Option<String>) -> Result<String, String> {
    bootstrap::ensure_backend()?;
    let mut spec = core_app::parse_new(&line);
    if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
        spec.name = Some(n);
    }
    shared.raw(|app| app.start_session(&spec))
}

/// Bring a conversation somewhere scope can steer it — the one it is showing,
/// or any conversation on the machine by id.
#[tauri::command]
fn reopen(shared: State<Shared>, id: String, cwd: String) -> Result<String, String> {
    bootstrap::ensure_backend()?;
    shared.raw(|app| {
        let original = app
            .sessions
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.live.as_ref().map(|l| l.pid));
        app.reopen(id, cwd, original);
        app.note.clone()
    });
    Ok(String::new())
}

#[tauri::command]
fn past(shared: State<Shared>) -> Vec<PastDto> {
    let all = history::scan(&core_app::default_root());
    shared.raw(|app| {
        all.into_iter()
            .map(|p| PastDto {
                open: app
                    .sessions
                    .iter()
                    .any(|s| s.id == p.id && (s.live.is_some() || s.in_pane)),
                age_secs: p.age_secs(),
                title: p.label(),
                id: p.id,
                cwd: p.cwd,
                bytes: p.bytes,
            })
            .collect()
    })
}

/// Give a session a name. A running one renames itself; a stopped one has the
/// name written to its transcript, which is where a name lives.
#[tauri::command]
fn rename(shared: State<Shared>, id: String, name: String) -> Result<(), String> {
    shared.raw(|app| app.rename(&id, &name))
}

/// Put the list in the order it was dragged into.
#[tauri::command]
fn reorder(shared: State<Shared>, ids: Vec<String>) {
    shared.raw(|app| app.reorder(ids));
}

#[tauri::command]
fn stop(shared: State<Shared>, id: String) -> Result<(), String> {
    shared
        .on(&id, |app| {
            app.stop_session();
            Ok(())
        })
        .unwrap_or_else(|| Err("no such session".into()))
}

/// Hand the whole thing over to the terminal view, for anyone who would rather
/// drive it that way. The window stays where it is.
#[tauri::command]
fn open_tui() -> Result<String, String> {
    control::open_terminal_with("scope")
}

fn main() {
    // Whatever holds sessions is started before the window is drawn, so the
    // first thing anyone clicks does not have to wait for it.
    let _ = bootstrap::ensure_backend();
    // The same key, for the same reason: a session opened from the window has
    // to have a way back to it.
    let way_back = control::hold_way_back();
    let app = App::new(
        core_app::default_root(),
        core_app::default_sessions_dir(),
        Duration::from_secs(24 * 3600),
        false,
    );
    tauri::Builder::default()
        .manage(Shared::new(app))
        .invoke_handler(tauri::generate_handler![
            readiness,
            sessions,
            feed,
            screen,
            send,
            answer,
            interrupt,
            start,
            reopen,
            past,
            rename,
            reorder,
            stop,
            open_tui,
            files,
            plan,
            agents,
            errors,
            frame,
            release_frame,
            key,
            window,
            tree,
            search,
            queued,
            queue,
            broadcast,
            isolate,
            merge,
            discard,
            launch_fleet,
            fleet,
            close_all,
            prune,
            notifications,
            rescan
        ])
        .build(tauri::generate_context!())
        .expect("scope failed to start")
        .run(move |_app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                control::drop_way_back(way_back);
            }
        });
}
