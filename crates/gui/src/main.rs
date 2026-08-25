// The desktop front end. It owns no logic: every command here is a thin
// translation between the window and `sightline-core`, so the app and the terminal
// view cannot answer the same question differently.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use sightline_core::app::App;
use sightline_core::session::Status;
use sightline_core::{app as core_app, bootstrap, brief, bus, control, history, usage, work};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::State;

/// What the window shares with the engine.
///
/// Catching up — pumping every transcript, reading the registry, capturing
/// every steerable pane — used to happen at the top of *every* command, so a
/// question the window asked queued behind three hundred megabytes of
/// transcript before it could be answered. It now happens on a clock of its
/// own, and every command here is a read.
struct Shared {
    app: Mutex<App>,
    meter: Mutex<usage::Meter>,
    caught_up: Mutex<Instant>,
}

/// How stale the engine's view may be. Fast enough that nothing looks frozen,
/// slow enough that it is not re-read for every frame of a moving screen.
const CATCH_UP: Duration = Duration::from_millis(250);

/// How often the engine reads the world of its own accord.
const ENGINE_TICK: Duration = Duration::from_millis(400);

impl Shared {
    fn new(app: App) -> Self {
        Shared {
            app: Mutex::new(app),
            meter: Mutex::new(usage::Meter::default()),
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

    /// Read the world: transcripts, the registry, the pane of every steerable
    /// session — and, as a consequence, publish everything that has changed.
    ///
    /// This runs on a clock of the engine's own, which is the point. It used to
    /// happen only when the window asked a question, so the stream stopped the
    /// moment nobody was looking at it — and a stream that only moves while
    /// someone is watching is not a stream, it is a redraw.
    fn catch_up(&self) {
        let mut at = match self.caught_up.lock() {
            Ok(t) => t,
            Err(e) => e.into_inner(),
        };
        if at.elapsed() < CATCH_UP {
            return;
        }
        let mut app = match self.app.lock() {
            Ok(a) => a,
            Err(e) => e.into_inner(),
        };
        app.refresh();
        app.probe();
        *at = Instant::now();
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
    /// what holds sessions here: "tmux", or "Sightline" where it hosts its own
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
    /// It can be spoken to at all — through a terminal, or down a pipe
    /// Sightline holds.
    steerable: bool,
    /// It has a terminal behind it. Everything that needs a screen — opening it
    /// in a window, attaching, typing keys — needs this rather than the above.
    terminal: bool,
    live: bool,
    /// how deep this session sits under whoever started it
    depth: usize,
    /// the session that started it, when one did
    parent: Option<String>,
    /// what it was asked to do, and how that is going
    task: Option<TaskDto>,
    /// output tokens and cost with everything it started added in
    rolled_output: u64,
    rolled_cost: f64,
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
    /// whether it succeeded — a failed tool call renders red in the Talk view,
    /// which it could not before because this field was dropped on the way out
    ok: bool,
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

fn state_of(s: &sightline_core::session::Session) -> (String, Option<String>) {
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
        backend: control::where_backend().to_string(),
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

#[derive(Serialize, Clone)]
struct TaskDto {
    id: String,
    session: String,
    parent: Option<String>,
    assignment: String,
    /// assigned · working · blocked · claimed · verified · abandoned
    state: String,
    /// why, when it is blocked
    why: Option<String>,
    notes: Vec<String>,
    depth: usize,
}

fn task_dto(store: &work::Store, task: &work::Task) -> TaskDto {
    TaskDto {
        id: task.id.clone(),
        session: task.session.clone(),
        parent: task.parent.clone(),
        assignment: task.assignment.clone(),
        state: task.state.label().to_string(),
        why: match &task.state {
            work::State::Blocked { why } => Some(why.clone()),
            _ => None,
        },
        notes: task.notes.iter().map(|n| n.text.clone()).collect(),
        depth: store.depth_of(&task.session),
    }
}

#[tauri::command]
fn sessions(shared: State<Shared>) -> Vec<SessionDto> {
    let mut meter = match shared.meter.lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    // Deliberately not a catch-up. The engine keeps itself current on its own
    // thread, so asking for the list is now a read rather than an errand.
    let pids: Vec<i64> = shared.raw(|app| {
        app.sessions
            .iter()
            .filter_map(|s| {
                s.live
                    .as_ref()
                    .map(|l| l.pid)
                    .or_else(|| app.pane_of(&s.id).map(|p| p.pid))
                    .or_else(|| app.owned_of(&s.id).map(|o| o.pid as i64))
            })
            .collect()
    });
    let used_by = meter.measure_all(&pids);
    // Cost that includes what each session's workers spent. Computed once for
    // the whole list rather than per row, because it walks every tree.
    let rolled = shared.raw(|app| app.rolled_up());
    shared.raw(|app| {
        app.sessions
            .iter()
            .map(|s| {
                let (state, tool) = state_of(s);
                let pane = app.pane_of(&s.id).map(|p| p.session.clone());
                // A session Sightline holds has no terminal, but it is held
                // somewhere and saying where is the same fact.
                let held = pane
                    .clone()
                    .or_else(|| app.owned_of(&s.id).map(|o| o.name.clone()));
                // What it is costing the machine, measured from its process
                // tree; nothing writes that down.
                let pid = s
                    .live
                    .as_ref()
                    .map(|l| l.pid)
                    .or_else(|| app.pane_of(&s.id).map(|p| p.pid))
                    .or_else(|| app.owned_of(&s.id).map(|o| o.pid as i64))
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
                    steerable: app.steerable(&s.id),
                    terminal: pane.is_some(),
                    live: s.live.is_some() || s.in_pane,
                    depth: app.work.depth_of(&s.id),
                    parent: app.work.parent_of(&s.id).map(str::to_string),
                    task: app.work.task_for(&s.id).map(|t| task_dto(&app.work, t)),
                    rolled_output: rolled
                        .get(&s.id)
                        .map(|c| c.output)
                        .unwrap_or(s.totals.output),
                    rolled_cost: rolled
                        .get(&s.id)
                        .map(|c| c.estimate)
                        .unwrap_or(s.totals.cost),
                    asking: app.approvals.get(&s.id).map(|a| AskDto {
                        question: a.question.clone(),
                        options: a.options.clone(),
                    }),
                    pane: held,
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
                    ok: e.ok,
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
                    ok: e.ok,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every mention of this across every session Sightline is watching — the same
/// search the terminal view does with `/`.
#[derive(Serialize)]
struct HitDto {
    id: String,
    session: String,
    at: String,
    head: String,
}

#[tauri::command]
fn search(shared: State<Shared>, text: String) -> Vec<HitDto> {
    shared.raw(|app| {
        app.run_search(&text);
        app.hits
            .iter()
            .filter_map(|(si, ei)| {
                let s = app.sessions.get(*si)?;
                // A session can match on its own name while having said
                // nothing, and dropping those was half the reason searching for
                // a session by name came back empty.
                let e = s.events.get(*ei);
                Some(HitDto {
                    id: s.id.clone(),
                    session: s.label(),
                    at: e
                        .and_then(|e| e.ts)
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_default(),
                    head: e
                        .map(|e| e.head.clone())
                        .unwrap_or_else(|| format!("{} — nothing said yet", s.where_())),
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

/// Say the same thing to every session Sightline can reach.
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

/// Close everything Sightline started, or just what has already finished.
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

/// Open a session in a terminal window of its own.
#[tauri::command]
fn window(shared: State<Shared>, id: String) -> Result<String, String> {
    let pane = shared
        .raw(|app| app.pane_of(&id).map(|p| p.session.clone()))
        .ok_or("scope has no terminal for that session")?;
    control::open_window(&pane)
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

/// Take an image off the system clipboard, without going through the webview.
///
/// WebKitGTK does not hand image data to the page: a paste fires an event whose
/// `clipboardData` carries no image item, so the obvious implementation looks
/// correct, runs, and finds nothing. Every part of it works except the part
/// that would have had the bytes.
///
/// So the clipboard is read where it can actually be read. `wl-paste` on
/// Wayland, `xclip` on X11 — the app is a client of whichever is running, so it
/// inherits the environment either needs.
/// What the window should paint behind everything, ready for the page.
///
/// The reading and the size limit live in core, because both front ends need
/// the same answer and because a file the person named is a decision with rules
/// attached. This hands over a kind and, for a picture, a data URI — the
/// content security policy permits `data:` and no other source, so a path on
/// disk is not something the page could load even if it were given one.
#[tauri::command]
fn backdrop() -> (String, Option<String>) {
    sightline_core::backdrop::painted()
}

/// Take a picture the person picked in the file chooser, and keep a copy.
///
/// A copy rather than a reference, and that is the interesting decision. The
/// file chooser in a webview hands over bytes and a name; it deliberately does
/// not hand over a path, so there is nothing to point at even if pointing were
/// wanted. It turns out to be the better arrangement anyway: a backdrop chosen
/// from a downloads folder that is later tidied up would otherwise vanish, and
/// the window would open one day looking wrong for a reason nobody could see.
#[tauri::command]
fn set_backdrop_image(name: String, data: String) -> Result<(String, Option<String>), String> {
    use base64::Engine;
    let payload = data.rsplit(',').next().unwrap_or_default();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| format!("that picture did not arrive intact: {e}"))?;
    if bytes.len() as u64 > sightline_core::backdrop::LARGEST {
        return Err(format!(
            "that image is {:.1} MB, and a backdrop may be at most {} MB.",
            bytes.len() as f64 / 1_048_576.0,
            sightline_core::backdrop::LARGEST / 1_048_576,
        ));
    }
    let ext = std::path::Path::new(&name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "png".into());
    let kept = sightline_core::app::data_dir().join(format!("backdrop.{ext}"));
    std::fs::write(&kept, &bytes).map_err(|e| format!("{}: {e}", kept.display()))?;
    sightline_core::backdrop::save(&sightline_core::backdrop::Choice::Image(kept))?;
    Ok(sightline_core::backdrop::painted())
}

/// Choose it. `bloom`, `none`, or a path to a picture.
///
/// Validated before it is remembered: a file that is missing, too large, or of
/// a kind the webview cannot decode is refused here, while somebody is looking
/// at the thing they just chose — rather than saved and then found to be broken
/// every time the window opens.
#[tauri::command]
fn set_backdrop(choice: String) -> Result<(String, Option<String>), String> {
    use sightline_core::backdrop::Choice;
    let choice = match choice.as_str() {
        "bloom" => Choice::Bloom,
        "none" => Choice::None,
        path => Choice::Image(std::path::PathBuf::from(sightline_core::app::expand(
            path.trim(),
        ))),
    };
    sightline_core::backdrop::save(&choice)?;
    Ok(sightline_core::backdrop::painted())
}

#[tauri::command]
fn clipboard_image() -> Result<String, String> {
    use std::process::Command;

    // What the clipboard is offering. Asking for a type it does not have gets
    // an empty read and no explanation, so the offer is checked first.
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let (list, read): (Vec<&str>, Vec<&str>) = if wayland {
        (vec!["wl-paste", "--list-types"], vec!["wl-paste", "--type"])
    } else {
        (
            vec!["xclip", "-selection", "clipboard", "-t", "TARGETS", "-o"],
            vec!["xclip", "-selection", "clipboard", "-o", "-t"],
        )
    };

    let offered = Command::new(list[0])
        .args(&list[1..])
        .output()
        .map_err(|e| format!("could not read the clipboard ({}): {e}", list[0]))?;
    let offered = String::from_utf8_lossy(&offered.stdout);
    let kind = offered
        .lines()
        .map(str::trim)
        .find(|t| t.starts_with("image/"))
        .ok_or("there is no image on the clipboard")?;

    let out = Command::new(read[0])
        .args(&read[1..])
        .arg(kind)
        .output()
        .map_err(|e| format!("could not read the clipboard: {e}"))?;
    if out.stdout.is_empty() {
        return Err(format!(
            "the clipboard offered {kind} and then gave nothing"
        ));
    }
    let ext = kind.rsplit('/').next().unwrap_or("png");
    let ext = match ext {
        "jpeg" => "jpg",
        "svg+xml" => "svg",
        other => other,
    };
    write_pasted(&out.stdout, ext)
}

/// Write image bytes where a session can read them, and say where.
fn write_pasted(bytes: &[u8], ext: &str) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("that image was empty".into());
    }
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(format!(
            "that image is {} MB, which is more than 32",
            bytes.len() / 1_048_576
        ));
    }
    let dir = core_app::data_dir().join("pasted");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let ext = if ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        ext
    } else {
        "png"
    };
    let path = dir.join(format!("{stamp}.{ext}"));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Put a pasted image somewhere a session can read it, and say where.
///
/// Sending the bytes themselves would only work for the sessions Sightline holds
/// over a pipe; a session in a terminal has no way to receive them. Every agent
/// can read a file, so the image is written to one and the path is what gets
/// sent — which works the same for both kinds of session, and leaves the image
/// on disk afterwards rather than only inside a conversation.
#[tauri::command]
fn attach_image(name: String, data: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("that did not decode as an image: {e}"))?;
    if bytes.is_empty() {
        return Err("that image was empty".into());
    }
    // A quarter of a gigabyte of clipboard is a mistake, not a screenshot.
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(format!(
            "that image is {} MB, which is more than 32",
            bytes.len() / 1_048_576
        ));
    }
    let ext = std::path::Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    write_pasted(&bytes, ext)
}

/// Keep a file somebody attached, and hand back where it went.
///
/// A copy, in Sightline's own directory, under the name it arrived with. The
/// session is told a path and reads the file itself, which is the whole reason
/// this is a copy and not a reference: the picker in a webview hands over bytes
/// and a name and deliberately never a path, so there is nothing to point at.
/// Keeping the name matters more than it looks — a worker reading
/// `crash-2026-08-25.log` knows something about it that `attachment-3` does not
/// say.
/// The shape of one supervised project.
///
/// A chief and its workers are separate sessions and appear as separate rows,
/// which is accurate and is not how anyone thinks about the work: a person hands
/// over a project, and what comes back should be the project with the sessions
/// inside it.
/// What an agent changed in one file, as a patch.
///
/// Fetched when asked for rather than carried on the event. A `fileChanged`
/// says a path and two counts, which is what a feed line needs; a patch is
/// kilobytes, arrives for every write, and would be read in perhaps one case in
/// twenty. The event vocabulary stays cheap and this stays a question.
#[tauri::command]
fn diff(shared: State<Shared>, id: String, path: String) -> Result<String, String> {
    shared.raw(|app| {
        let session = app
            .sessions
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("no session {id}"))?;
        let cwd = std::path::PathBuf::from(&session.cwd);
        // Relative to the session's own directory, which is what the transcript
        // records and what git wants.
        let rel = std::path::Path::new(&path)
            .strip_prefix(&cwd)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.clone());
        sightline_core::git::diff(&cwd, &rel)
            .ok_or_else(|| format!("nothing changed in {rel}, or it is not in a repository"))
    })
}

/// Say a file has been read, as it stands now.
///
/// This is what "accept" honestly means. The change is already on disk, so
/// nothing is applied; what is recorded is that a person looked. The value is
/// entirely in the next edit — a fleet writing across a hundred files produces a
/// feed nobody can read, and a feed that can say "this one you have read, and
/// this one has moved since" is a different instrument.
#[tauri::command]
fn mark_reviewed(shared: State<Shared>, id: String, path: String) -> Result<String, String> {
    let file = shared.raw(|app| resolve(app, &id, &path))?;
    let now = chrono::Utc::now().timestamp();
    let mut store = sightline_core::reviewed::Store::load(&core_app::data_dir());
    store.mark(&file, now)?;
    Ok(format!(
        "{} marked read — a later edit to it will say so",
        file.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(path)
    ))
}

/// Every file that has been read, and whether it has moved since.
///
/// One call for the whole feed rather than one per line: a feed is hundreds of
/// rows and most of them are about the same handful of files.
#[tauri::command]
fn reviewed() -> serde_json::Value {
    let store = sightline_core::reviewed::Store::load(&core_app::data_dir());
    serde_json::to_value(store.all()).unwrap_or(serde_json::Value::Null)
}

/// A path an agent reported, as somewhere on this machine.
///
/// The transcript records paths relative to the session's directory sometimes
/// and absolute at others, and every one of these commands needs the same
/// answer. One place to be wrong about it rather than four.
fn resolve(app: &mut App, id: &str, path: &str) -> Result<std::path::PathBuf, String> {
    let session = app
        .sessions
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("no session {id}"))?;
    let cwd = std::path::PathBuf::from(&session.cwd);
    let given = std::path::Path::new(path);
    Ok(if given.is_absolute() {
        given.to_path_buf()
    } else {
        cwd.join(given)
    })
}

/// Put a file back the way it was before an agent touched it.
///
/// The one destructive thing in this window, and it is here because the
/// alternative is a person leaving Sightline to undo something Sightline showed
/// them. It discards uncommitted work in that file and cannot be undone, so the
/// front end asks first and the reason it asks is this comment.
///
/// There is deliberately no counterpart. "Accept" would do nothing: the change
/// is already on disk, and a button that changes nothing while implying it
/// blessed something is worse than no button.
#[tauri::command]
fn revert(shared: State<Shared>, id: String, path: String) -> Result<String, String> {
    shared.raw(|app| {
        let session = app
            .sessions
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("no session {id}"))?;
        let cwd = std::path::PathBuf::from(&session.cwd);
        let rel = std::path::Path::new(&path)
            .strip_prefix(&cwd)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.clone());
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&cwd)
            .args(["checkout", "--", &rel])
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "could not put {rel} back: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        // It is no longer at the state somebody was content with, so saying it
        // is unchanged would be a lie in the direction that matters.
        let mut seen = sightline_core::reviewed::Store::load(&core_app::data_dir());
        seen.forget(&cwd.join(&rel));
        Ok(format!("{rel} put back"))
    })
}

#[tauri::command]
fn mission(shared: State<Shared>, id: String) -> Result<serde_json::Value, String> {
    shared.raw(|app| {
        let chart = app.work.chart(&id);
        serde_json::to_value(chart).map_err(|e| e.to_string())
    })
}

#[tauri::command]
fn attach_file(name: String, data: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("that file did not decode: {e}"))?;
    if bytes.is_empty() {
        return Err("that file was empty".into());
    }
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(format!(
            "that file is {} MB, which is more than 64",
            bytes.len() / 1_048_576
        ));
    }
    let dir = core_app::data_dir().join("attached");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // Only the last component, and nothing that climbs. The name comes from a
    // file chooser rather than from a person typing, but a path separator in it
    // would write outside this directory and that is not a thing to leave to
    // the goodwill of the source.
    let safe: String = std::path::Path::new(&name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "attachment".into())
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '\0' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = if safe.trim().is_empty() || safe == "." || safe == ".." {
        "attachment".to_string()
    } else {
        safe
    };

    // Two files of the same name in one conversation is normal. Numbered rather
    // than overwritten, or the second one silently replaces the first and the
    // session reads the wrong thing.
    let mut path = dir.join(&safe);
    if path.exists() {
        let stem = std::path::Path::new(&safe)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let ext = std::path::Path::new(&safe)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        for n in 2..1000 {
            let next = dir.join(format!("{stem}-{n}{ext}"));
            if !next.exists() {
                path = next;
                break;
            }
        }
    }
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
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
        .on(&id, |app| app.interrupt())
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
    if spec.owned {
        // A session Sightline holds itself. Its first message is what names the
        // conversation, so the line's prompt is sent as the opening rather than
        // typed in afterwards.
        let opening = spec.prompt.clone();
        return shared.raw(|app| {
            let id = app.start_owned(&spec, opening.as_deref())?;
            Ok(app
                .owned_of(&id)
                .map(|o| o.name.clone())
                .unwrap_or_else(|| id.clone()))
        });
    }
    shared.raw(|app| app.start_session(&spec))
}

/// Bring a conversation somewhere Sightline can steer it — the one it is showing,
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

/// Take a row off the list. The conversation stays on disk; Resume still finds
/// it. Closing a session and removing its row are different things and both are
/// wanted.
#[tauri::command]
fn remove(shared: State<Shared>, id: String) -> Result<String, String> {
    shared.raw(|app| app.hide(&id))
}

/// Take every finished session off the list at once, which is what the clutter
/// actually is.
#[tauri::command]
fn remove_ended(shared: State<Shared>) -> usize {
    shared.raw(|app| app.hide_ended())
}

/// Put them all back. Hiding has to be reversible or it is deleting with extra
/// steps.
#[tauri::command]
fn restore_removed(shared: State<Shared>) -> usize {
    shared.raw(|app| app.unhide_all())
}

/// How many rows are being kept off the list, so the window can offer to put
/// them back rather than leaving them lost.
#[tauri::command]
fn removed_count(shared: State<Shared>) -> usize {
    shared.raw(|app| app.hidden_count())
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

#[derive(Serialize)]
struct FileTextDto {
    path: String,
    text: String,
    lines: usize,
    bytes: u64,
    /// only the head of the file is here, because the whole of it is too much
    truncated: bool,
}

/// A file the size of a thing a person reads. Beyond this the window would
/// spend longer laying it out than anyone would spend looking at it, so the
/// head is shown and the truncation is stated rather than hidden.
const READABLE: u64 = 4 * 1024 * 1024;
const READABLE_LINES: usize = 20_000;

/// Read a file so it can be looked at without leaving the window.
///
/// `base` is the session's working directory, because a path from a git status
/// is relative to the repository and a path from a transcript is not.
#[tauri::command]
fn open_file(path: String, base: Option<String>) -> Result<FileTextDto, String> {
    let mut full = std::path::PathBuf::from(core_app::expand(&path));
    if full.is_relative() {
        let Some(base) = base.filter(|b| !b.is_empty()) else {
            return Err(format!(
                "{path} is relative and there is nowhere to read it from"
            ));
        };
        full = std::path::Path::new(&base).join(full);
    }
    let meta = std::fs::metadata(&full).map_err(|e| format!("{}: {e}", full.display()))?;
    if meta.is_dir() {
        return Err(format!("{} is a directory", full.display()));
    }
    let bytes = meta.len();
    let raw = std::fs::read(&full).map_err(|e| format!("{}: {e}", full.display()))?;
    // A NUL in the first few kilobytes is what every other tool uses to decide
    // this is not text, and it is right often enough.
    if raw.iter().take(8192).any(|b| *b == 0) {
        return Err(format!("{} is not text ({} bytes)", full.display(), bytes));
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let total = text.lines().count();
    let truncated = bytes > READABLE || total > READABLE_LINES;
    let text = if truncated {
        text.lines()
            .take(READABLE_LINES)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text
    };
    Ok(FileTextDto {
        path: full.to_string_lossy().into_owned(),
        text,
        lines: total,
        bytes,
        truncated,
    })
}

/// What a session has changed in one file, as a diff. Untracked files come back
/// as their own contents, which is what there is to show.
#[tauri::command]
fn file_diff(shared: State<Shared>, id: String, path: String) -> Option<String> {
    shared.raw(|app| {
        let cwd = app.sessions.iter().find(|s| s.id == id)?.cwd.clone();
        sightline_core::git::diff(std::path::Path::new(&cwd), &path)
    })
}

/// Where a session is working, so the window can resolve a relative path.
#[tauri::command]
fn session_cwd(shared: State<Shared>, id: String) -> Option<String> {
    shared.raw(|app| {
        app.sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.cwd.clone())
            .filter(|c| !c.is_empty())
    })
}

/// The stream, replayed from a point, so a window that has just opened — or one
/// that was closed while sessions carried on — shows what happened rather than
/// starting blank.
#[tauri::command]
fn stream(since: u64) -> Vec<bus::Event> {
    let journal = core_app::data_dir().join("events.jsonl");
    let mut events = bus::replay(&journal, since);
    // A window does not want a day of history; it wants the recent past and
    // then the live feed, which arrives by push.
    if events.len() > 400 {
        events.drain(..events.len() - 400);
    }
    events
}

#[tauri::command]
fn tasks(shared: State<Shared>) -> Vec<TaskDto> {
    shared.raw(|app| {
        // One entry per session, not one per historical task: verified and
        // abandoned tasks are kept, so a reassigned session would otherwise
        // render a duplicate card for each. Dedupe before ordering.
        let sessions: Vec<String> = app
            .work
            .tasks()
            .iter()
            .map(|t| t.session.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let order = app.work.ordered(&sessions);
        let mut out = Vec::new();
        for (session, depth) in order {
            let Some(task) = app
                .work
                .task_for(&session)
                .or_else(|| app.work.tasks().iter().rev().find(|t| t.session == session))
            else {
                continue;
            };
            let mut dto = task_dto(&app.work, task);
            dto.depth = depth;
            out.push(dto);
        }
        out
    })
}

#[tauri::command]
fn assign(shared: State<Shared>, id: String, text: String) -> String {
    shared.raw(|app| app.assign(&id, &text))
}

#[tauri::command]
fn note(shared: State<Shared>, task: String, text: String) -> Result<(), String> {
    shared.raw(|app| {
        let out = app.work.note(&task, &text);
        app.work.flush();
        out
    })
}

/// What a session was told when it was given its work.
///
/// The brief is rendered rather than stored: it is the constitution as it
/// stands now plus the task as it stands now, so reading it here answers "what
/// would this session be told today", which is the question worth asking when
/// its work has drifted.
#[tauri::command]
fn brief(shared: State<Shared>, id: String) -> Option<String> {
    shared.raw(|app| {
        let cwd = app.sessions.iter().find(|s| s.id == id)?.cwd.clone();
        let task = app.work.task_for(&id)?.clone();
        let constitution = brief::Constitution::find(std::path::Path::new(&cwd)).map(|(_, c)| c);
        Some(brief::render(constitution.as_ref(), &task))
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConstitutionDto {
    /// Where it is, or where it would go if it were written.
    path: String,
    /// The markdown itself; empty when there is none yet.
    text: String,
    /// Whether that file exists today.
    exists: bool,
}

/// The project constitution behind a session, as text.
///
/// Deliberately the raw markdown rather than the parsed sections: it is a
/// document a person wrote, and handing back a reassembled version of it would
/// quietly lose whatever the parser does not model.
#[tauri::command]
fn constitution(shared: State<Shared>, id: String) -> Option<ConstitutionDto> {
    let cwd = shared.raw(|app| {
        app.sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.cwd.clone())
    })?;
    if cwd.is_empty() {
        return None;
    }
    let here = std::path::Path::new(&cwd);
    if let Some((root, _)) = brief::Constitution::find(here) {
        let path = root.join(brief::FILE);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        return Some(ConstitutionDto {
            path: path.to_string_lossy().into_owned(),
            text,
            exists: true,
        });
    }
    // None yet. Say where one would go, so writing the first is one step
    // rather than a question about where it belongs: the repository, because a
    // constitution is about the project and not about one folder in it.
    let root = sightline_core::git::repo_root(here).unwrap_or_else(|| here.to_path_buf());
    Some(ConstitutionDto {
        path: root.join(brief::FILE).to_string_lossy().into_owned(),
        text: String::new(),
        exists: false,
    })
}

/// Write the constitution back.
///
/// The one thing in the window that edits a file in your repository, so it is
/// explicit: it writes exactly the path `constitution` reported, and it says so
/// afterwards.
#[tauri::command]
fn save_constitution(path: String, text: String) -> Result<String, String> {
    let path = std::path::PathBuf::from(&path);
    if !path.ends_with(brief::FILE) {
        return Err(format!("that is not a {}", brief::FILE));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("saved {}", path.display()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowDto {
    /// the project the Hub is pointed at
    where_: String,
    checks: usize,
    invariants: usize,
    trusted: bool,
    constitution: bool,
    /// what a fleet here may do, in words
    ceilings: String,
    has_ceilings: bool,
    running: usize,
    /// whether anything here can tell a worker it is wrong
    can_refuse: bool,
    can_verify: bool,
}

/// Everything the workflow face needs, in one ask.
///
/// One round trip rather than five, because it is drawn on a tick and five
/// separate questions about the same folder is four more chances for the answer
/// to be half of two different states.
#[tauri::command]
fn workflow(shared: State<Shared>) -> WorkflowDto {
    shared.raw(|app| {
        let here = app.here();
        let state = app.project_state(&here);
        let limits = sightline_core::limits::in_force(&here).unwrap_or_default();
        WorkflowDto {
            where_: here.to_string_lossy().into_owned(),
            checks: state.checks,
            invariants: state.invariants,
            trusted: state.trusted,
            constitution: state.constitution,
            ceilings: limits.describe(),
            has_ceilings: limits.any(),
            running: app.running_sessions(),
            can_refuse: state.can_refuse(),
            can_verify: state.can_verify(),
        }
    })
}

/// Write this project the two files that make supervised work mean anything.
#[tauri::command]
fn set_up_project(shared: State<Shared>) -> Result<String, String> {
    shared.raw(|app| {
        let here = app.here();
        app.set_up_project(&here)
    })
}

/// Try to break what must never stop being true here.
#[tauri::command]
fn run_invariants(shared: State<Shared>) -> Result<String, String> {
    shared.raw(|app| {
        let here = app.here();
        app.run_invariants(&here)
    })
}

/// What a fleet on this machine may do.
#[tauri::command]
fn set_ceilings(
    shared: State<Shared>,
    sessions: Option<usize>,
    spend: Option<f64>,
) -> Result<String, String> {
    shared.raw(|app| app.set_ceilings(sessions, spend))
}

/// Reconcile this fork onto a newer upstream release.
#[tauri::command]
fn reconcile(shared: State<Shared>, version: String) -> Result<String, String> {
    bootstrap::ensure_backend()?;
    shared.raw(|app| {
        let here = app.here();
        let (name, worktree) = app.reconcile(&here, version.trim(), None, None)?;
        Ok(format!("{name} is reconciling in {}", worktree.display()))
    })
}

/// Hand work to a chief, in the folder the Hub is pointed at.
#[tauri::command]
fn start_chief(shared: State<Shared>, intent: String) -> Result<String, String> {
    bootstrap::ensure_backend()?;
    shared.raw(|app| {
        let here = app.here();
        let id = app.start_chief(&here, &intent, None)?;
        Ok(app
            .owned_of(&id)
            .map(|o| o.name.clone())
            .unwrap_or_else(|| id.clone()))
    })
}

#[tauri::command]
fn task_state(shared: State<Shared>, task: String, state: String) -> Result<(), String> {
    let wanted = match state.as_str() {
        "assigned" => work::State::Assigned,
        "working" => work::State::Working,
        "claimed" => work::State::Claimed,
        "verified" => work::State::Verified,
        "abandoned" => work::State::Abandoned,
        other => return Err(format!("no such state: {other}")),
    };
    shared.raw(|app| {
        let out = app.work.set_state(&task, wanted);
        app.work.flush();
        out
    })
}

#[tauri::command]
fn lineage(shared: State<Shared>, child: String, parent: String) {
    shared.raw(|app| app.record_lineage(&child, &parent));
}

/// How many consumers are attached to the socket — including, once it is
/// running, a foreman.
#[tauri::command]
fn consumers(shared: State<Shared>) -> Option<usize> {
    shared.raw(|app| app.consumers())
}

fn main() {
    // Whatever holds sessions is started before the window is drawn, so the
    // first thing anyone clicks does not have to wait for it.
    let _ = bootstrap::ensure_backend();
    // The same key, for the same reason: a session opened from the window has
    // to have a way back to it.
    let way_back = control::hold_way_back();
    let mut app = App::new(
        core_app::default_root(),
        core_app::default_sessions_dir(),
        Duration::from_secs(24 * 3600),
        false,
    );
    // The window is a consumer of the stream like any other. What it loses if
    // this fails is the live feed, not the app, so it is reported and carried
    // on from rather than fatal.
    let subscription = match app.with_stream() {
        // Either way the window gets a live feed. Owning the stream means it is
        // also the one journalling it and offering it on the socket.
        Ok(_) => Some(app.bus.subscribe()),
        Err(e) => {
            eprintln!("the event stream is not available: {e}");
            None
        }
    };
    tauri::Builder::default()
        .manage(Shared::new(app))
        .setup(move |handle| {
            // The engine, kept current whether or not anyone is asking. Slower
            // than the catch-up threshold so every wake does some work, and
            // faster than the pane probe's own throttle so that throttle is
            // what decides the rate rather than this.
            let ticker = handle.handle().clone();
            std::thread::Builder::new()
                .name("sightline-engine".into())
                .spawn(move || {
                    use tauri::Manager;
                    loop {
                        std::thread::sleep(ENGINE_TICK);
                        // A panic in one tick — an unexpected shape from a file
                        // Sightline does not write, some future change — must not
                        // wedge the engine for the rest of the session. Catch it,
                        // and take the next tick. The lock is poison-tolerant
                        // already (raw() recovers an into_inner), so the state is
                        // still usable afterwards.
                        let ticker = std::panic::AssertUnwindSafe(&ticker);
                        let _ = std::panic::catch_unwind(|| {
                            ticker.state::<Shared>().catch_up();
                        });
                    }
                })?;

            let Some(subscription) = subscription else {
                return Ok(());
            };
            // Pushed rather than polled: the window learns that something
            // happened at the moment it happens, and stops asking otherwise.
            let emitter = handle.handle().clone();
            std::thread::Builder::new()
                .name("sightline-window-feed".into())
                .spawn(move || {
                    use tauri::Emitter;
                    while let Some(ev) = subscription.recv() {
                        if emitter.emit("sightline://event", &ev).is_err() {
                            break;
                        }
                    }
                })?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            readiness,
            sessions,
            feed,
            screen,
            send,
            attach_image,
            attach_file,
            mission,
            diff,
            revert,
            mark_reviewed,
            reviewed,
            clipboard_image,
            answer,
            interrupt,
            start,
            reopen,
            past,
            rename,
            reorder,
            stop,
            remove,
            remove_ended,
            restore_removed,
            removed_count,
            files,
            plan,
            agents,
            errors,
            window,
            tree,
            search,
            queued,
            stream,
            tasks,
            assign,
            backdrop,
            set_backdrop,
            set_backdrop_image,
            note,
            brief,
            workflow,
            set_up_project,
            run_invariants,
            set_ceilings,
            start_chief,
            reconcile,
            constitution,
            save_constitution,
            task_state,
            lineage,
            consumers,
            open_file,
            file_diff,
            session_cwd,
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
