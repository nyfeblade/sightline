// The desktop front end. It owns no logic: every command here is a thin
// translation between the window and `scope-core`, so the app and the terminal
// view cannot answer the same question differently.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use crossterm::event::KeyCode;
use scope_core::app::App;
use scope_core::session::Status;
use scope_core::{app as core_app, bootstrap, control, history, screen, usage};
use serde::Serialize;
use std::sync::Mutex;
use std::time::Duration;
use tauri::State;

struct Shared(Mutex<App>, Mutex<usage::Meter>);

impl Shared {
    /// Every command starts by catching up, exactly as the terminal view's tick
    /// does. A poisoned lock is not worth taking the window down for.
    fn with<T>(&self, f: impl FnOnce(&mut App) -> T) -> T {
        let mut app = match self.0.lock() {
            Ok(a) => a,
            Err(e) => e.into_inner(),
        };
        app.refresh();
        app.probe();
        f(&mut app)
    }

    /// Run something against one session by id, leaving the selection where it
    /// was. The engine is written around a cursor; the window is not.
    fn on<T>(&self, id: &str, f: impl FnOnce(&mut App) -> T) -> Option<T> {
        self.with(|app| {
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
    age_secs: i64,
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
    let table = usage::table();
    let mut meter = match shared.1.lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    shared.with(|app| {
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
                let used = if pid > 0 {
                    meter.measure(pid, &table)
                } else {
                    usage::Usage::default()
                };
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
    let pane = shared.with(|app| app.pane_of(&id).map(|p| p.id.clone()))?;
    control::frame(&pane, cols, rows)
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
        .with(|app| app.pane_of(&id).map(|p| p.id.clone()))
        .ok_or("that session cannot be typed into")?;
    control::forward_key(&pane, code, ctrl)
}

/// Open a session in a terminal window of its own.
#[tauri::command]
fn window(shared: State<Shared>, id: String) -> Result<String, String> {
    let pane = shared
        .with(|app| app.pane_of(&id).map(|p| p.session.clone()))
        .ok_or("scope has no terminal for that session")?;
    control::open_window(&pane)
}

/// Stop holding a session at the window's size, when the window stops showing
/// it.
#[tauri::command]
fn release_frame(shared: State<Shared>, id: String) {
    if let Some(pane) = shared.with(|app| app.pane_of(&id).map(|p| p.id.clone())) {
        control::release_frame(&pane);
    }
}

/// What a session's terminal is showing, as plain text.
#[tauri::command]
fn screen(shared: State<Shared>, id: String) -> Option<String> {
    shared.with(|app| app.mirror.get(&id).cloned())
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
    shared.with(|app| app.start_session(&spec))
}

/// Bring a conversation somewhere scope can steer it — the one it is showing,
/// or any conversation on the machine by id.
#[tauri::command]
fn reopen(shared: State<Shared>, id: String, cwd: String) -> Result<String, String> {
    bootstrap::ensure_backend()?;
    shared.with(|app| {
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
    shared.with(|app| {
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
    shared.with(|app| app.rename(&id, &name))
}

/// Put the list in the order it was dragged into.
#[tauri::command]
fn reorder(shared: State<Shared>, ids: Vec<String>) {
    shared.with(|app| app.reorder(ids));
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
    let app = App::new(
        core_app::default_root(),
        core_app::default_sessions_dir(),
        Duration::from_secs(24 * 3600),
        false,
    );
    tauri::Builder::default()
        .manage(Shared(Mutex::new(app), Mutex::new(usage::Meter::default())))
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
            tree
        ])
        .run(tauri::generate_context!())
        .expect("scope failed to start");
}
