//! Ironsight holding its own sessions.
//!
//! On Unix, sessions have lived in tmux because tmux outlives the program that
//! started them: close Ironsight and your agents keep working. That is the only
//! reason for the dependency, and it is a good one — losing a fleet because a
//! window was closed would be indefensible.
//!
//! This is the other way of having that property. A small headless process owns
//! the pseudo-terminals and nothing else; the windows and the terminal view
//! become clients of it. Closing a client closes a client. The sessions belong
//! to something that is still there.
//!
//! What it is deliberately not: a place for logic. The daemon owns file
//! descriptors and answers questions about them. Everything about what a
//! session *means* — status, cost, transcripts, tasks — stays in the front end,
//! reading the same files it always read. A daemon that starts making judgements
//! is a daemon that has to be restarted to change one.
//!
//! The protocol is one JSON object per line, request in and reply out, on a Unix
//! socket beside the event socket. It is deliberately dull. Anything clever here
//! would be a thing to debug at the moment a fleet is wedged.

use crate::control::Pane;
use crate::screen::Frame;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Bumped when the wire changes in a way an older client would misread. A
/// client that sees a version it does not know refuses rather than guessing.
///
/// 2 added owned sessions: ones the daemon holds by pipe rather than by
/// pseudo-terminal. A version-1 daemon answers `Own` with "could not read that
/// request", which is why the mismatch is caught at `Hello` instead.
///
/// 3 added what a session is allowed and forbidden to do. Bumped for a field,
/// which is not the usual rule — the usual rule is that fields may be added —
/// because this one decides what an agent may run. An old daemon reading a spec
/// it half understands would start a session with the wrong permissions and say
/// nothing, so any change to `owned::Spec` bumps this.
pub const WIRE: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "camelCase")]
pub enum Request {
    /// Is anyone there, and speaking which version.
    Hello,
    Panes,
    Start {
        cwd: String,
        argv: Vec<String>,
        opening: Vec<String>,
    },
    /// Everything typed goes through here: text, a named key, a forwarded key
    /// press. The client turns all three into bytes, so the table that says
    /// what a key is exists once.
    Write {
        pane: String,
        bytes: Vec<u8>,
    },
    Screen {
        pane: String,
        cols: u16,
        rows: u16,
    },
    Capture {
        pane: String,
    },
    Kill {
        pane: String,
    },
    Prune,
    StopAll,
    Adopt {
        cwd: String,
        session: String,
    },
    EndProcess {
        pid: i64,
    },
    Count,
    // ── sessions held by pipe rather than by pseudo-terminal ──────────────
    /// Start one, with the message it is to begin on.
    Own {
        cwd: String,
        /// Everything settled at the start and fixed for the life of the
        /// session: model, permission mode, denied tools, opening message.
        spec: crate::owned::Spec,
    },
    /// Every owned session this daemon holds.
    OwnedAll,
    /// Say something to one, by Ironsight's name for it or by its transcript id.
    Say {
        who: String,
        text: String,
    },
    /// End one, and forget it.
    OwnedStop {
        who: String,
    },
    /// Forget the ones that have exited.
    OwnedReap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "is", rename_all = "camelCase")]
pub enum Reply {
    Hello { wire: u32, pid: u32 },
    Panes { panes: Vec<Pane> },
    Name { name: String },
    Names { names: Vec<String> },
    Screen { frame: Option<Frame> },
    Text { text: Option<String> },
    Count { n: usize },
    Yes { it: bool },
    Owned { it: crate::owned::Owned },
    OwnedAll { all: Vec<crate::owned::Owned> },
    Done,
    Failed { why: String },
}

impl Reply {
    #[cfg(unix)]
    fn of<T>(r: Result<T, String>, ok: impl FnOnce(T) -> Reply) -> Reply {
        match r {
            Ok(v) => ok(v),
            Err(why) => Reply::Failed { why },
        }
    }
}

/// Where the daemon listens. Beside the event socket, because they are two
/// halves of the same conversation: one carries what happened, this one carries
/// what to do.
pub fn default_path() -> PathBuf {
    crate::app::data_dir().join("control.sock")
}

// ── the daemon ─────────────────────────────────────────────────────────────

/// Answer one request, against the sessions this process owns.
#[cfg(unix)]
fn answer(request: Request) -> Reply {
    use crate::host;
    match request {
        Request::Hello => Reply::Hello {
            wire: WIRE,
            pid: std::process::id(),
        },
        Request::Panes => Reply::Panes {
            panes: host::panes(),
        },
        Request::Start { cwd, argv, opening } => Reply::of(
            host::new_session_with(Path::new(&cwd), &argv, &opening),
            |name| Reply::Name { name },
        ),
        Request::Write { pane, bytes } => {
            Reply::of(host::write_bytes(&pane, &bytes), |_| Reply::Done)
        }
        Request::Screen { pane, cols, rows } => Reply::Screen {
            frame: host::frame(&pane, cols, rows),
        },
        Request::Capture { pane } => Reply::Text {
            text: host::capture(&pane),
        },
        Request::Kill { pane } => Reply::of(host::kill_session(&pane), |_| Reply::Done),
        Request::Prune => Reply::Names {
            names: host::prune(),
        },
        Request::StopAll => Reply::Names {
            names: host::stop_all(),
        },
        Request::Adopt { cwd, session } => {
            Reply::of(host::adopt(Path::new(&cwd), &session), |name| Reply::Name {
                name,
            })
        }
        Request::EndProcess { pid } => Reply::Yes {
            it: host::end_process(pid),
        },
        Request::Count => Reply::Count {
            n: host::hosted_count(),
        },
        Request::Own { cwd, spec } => Reply::of(
            crate::owned::start(
                &crate::control::claude_program(),
                Path::new(&cwd),
                &spec,
                // Long enough for the agent's first line, which is what binds
                // the session to its transcript. A caller waiting on a socket
                // would rather wait a moment than be handed a session no view
                // can find.
                std::time::Duration::from_secs(20),
            ),
            |it| Reply::Owned { it },
        ),
        Request::OwnedAll => Reply::OwnedAll {
            all: crate::owned::list(),
        },
        Request::Say { who, text } => Reply::of(crate::owned::say(&who, &text), |_| Reply::Done),
        Request::OwnedStop { who } => Reply::of(crate::owned::stop(&who), |_| Reply::Done),
        Request::OwnedReap => Reply::Names {
            names: crate::owned::reap(),
        },
    }
}

/// Listen, and keep listening. Never returns while the socket is held.
#[cfg(unix)]
pub fn serve(path: PathBuf) -> std::io::Result<()> {
    use std::os::unix::net::{UnixListener, UnixStream};

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A socket left by a process that did not exit cleanly would otherwise make
    // every future run fail to bind. One nobody answers is safe to remove.
    if path.exists() && UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // A client that stalls mid-request must not stall the others, and a
        // client that dies must not take the daemon with it.
        std::thread::Builder::new()
            .name("ironsight-client".into())
            .spawn(move || {
                let reader = BufReader::new(match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => return,
                });
                let mut out = stream;
                for line in reader.lines().map_while(Result::ok) {
                    let reply = match serde_json::from_str::<Request>(&line) {
                        Ok(request) => answer(request),
                        Err(e) => Reply::Failed {
                            why: format!("could not read that request: {e}"),
                        },
                    };
                    let mut text = serde_json::to_string(&reply).unwrap_or_default();
                    text.push('\n');
                    if out.write_all(text.as_bytes()).is_err() {
                        return;
                    }
                    let _ = out.flush();
                }
            })?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn serve(_path: PathBuf) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the daemon needs a Unix domain socket",
    ))
}

// ── being a client of it ───────────────────────────────────────────────────

/// Ask the daemon something.
///
/// A connection per request. It is a Unix socket on the same machine, so the
/// cost is tens of microseconds, and in exchange there is no connection state
/// to go stale, no reconnect logic, and no shared mutable client.
#[cfg(unix)]
pub fn ask(request: &Request) -> Result<Reply, String> {
    use std::os::unix::net::UnixStream;

    let path = default_path();
    let stream = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = stream.try_clone().map_err(|e| e.to_string())?;
    let mut text = serde_json::to_string(request).map_err(|e| e.to_string())?;
    text.push('\n');
    out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.trim().is_empty() {
        return Err("the daemon closed without answering".into());
    }
    serde_json::from_str(&line).map_err(|e| format!("could not read that reply: {e}"))
}

#[cfg(not(unix))]
pub fn ask(_request: &Request) -> Result<Reply, String> {
    Err("no daemon on this platform".into())
}

/// Whether a daemon is listening, and speaking a version we understand.
pub fn running() -> bool {
    matches!(ask(&Request::Hello), Ok(Reply::Hello { wire, .. }) if wire == WIRE)
}

/// Start one if there is not one already.
///
/// It is put in a session of its own, so that closing the terminal that
/// happened to start it does not send it a hangup and take every session with
/// it — which would defeat the entire point of having it.
#[cfg(unix)]
pub fn ensure_running() -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    if running() {
        return Ok(());
    }
    // One that is listening but speaking a version we do not know cannot be
    // talked to, and starting another would only fail to bind the socket it
    // holds. Say which it is, because "the daemon did not start listening" sends
    // someone looking for a crash that never happened.
    if let Ok(Reply::Hello { wire, pid }) = ask(&Request::Hello) {
        return Err(format!(
            "a daemon (pid {pid}) is already holding sessions and speaks wire {wire}, not {WIRE} \
             — it is running an older Ironsight. Stop it, or leave this to it."
        ));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    command
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            // Its own session and process group: no controlling terminal, so no
            // hangup when ours goes away.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().map_err(|e| e.to_string())?;

    // It has to be listening before the first question, and it is starting from
    // cold, so wait — briefly, and for the thing itself rather than a guess at
    // how long it takes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if running() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Err("the daemon did not start listening".into())
}

#[cfg(not(unix))]
pub fn ensure_running() -> Result<(), String> {
    Err("no daemon on this platform".into())
}

/// Ask, and expect it to have worked.
fn done(request: &Request) -> Result<(), String> {
    match ask(request)? {
        Reply::Done => Ok(()),
        Reply::Failed { why } => Err(why),
        other => Err(format!("unexpected answer: {other:?}")),
    }
}

pub fn panes() -> Vec<Pane> {
    match ask(&Request::Panes) {
        Ok(Reply::Panes { panes }) => panes,
        _ => Vec::new(),
    }
}

pub fn start(cwd: &Path, argv: &[String], opening: &[String]) -> Result<String, String> {
    match ask(&Request::Start {
        cwd: cwd.to_string_lossy().into_owned(),
        argv: argv.to_vec(),
        opening: opening.to_vec(),
    })? {
        Reply::Name { name } => Ok(name),
        Reply::Failed { why } => Err(why),
        other => Err(format!("unexpected answer: {other:?}")),
    }
}

pub fn write_bytes(pane: &str, bytes: &[u8]) -> Result<(), String> {
    done(&Request::Write {
        pane: pane.to_string(),
        bytes: bytes.to_vec(),
    })
}

pub fn frame(pane: &str, cols: u16, rows: u16) -> Option<Frame> {
    match ask(&Request::Screen {
        pane: pane.to_string(),
        cols,
        rows,
    }) {
        Ok(Reply::Screen { frame }) => frame,
        _ => None,
    }
}

pub fn capture(pane: &str) -> Option<String> {
    match ask(&Request::Capture {
        pane: pane.to_string(),
    }) {
        Ok(Reply::Text { text }) => text,
        _ => None,
    }
}

pub fn kill_session(pane: &str) -> Result<(), String> {
    done(&Request::Kill {
        pane: pane.to_string(),
    })
}

fn names(request: &Request) -> Vec<String> {
    match ask(request) {
        Ok(Reply::Names { names }) => names,
        _ => Vec::new(),
    }
}

pub fn prune() -> Vec<String> {
    names(&Request::Prune)
}

pub fn stop_all() -> Vec<String> {
    names(&Request::StopAll)
}

pub fn adopt(cwd: &Path, session: &str) -> Result<String, String> {
    match ask(&Request::Adopt {
        cwd: cwd.to_string_lossy().into_owned(),
        session: session.to_string(),
    })? {
        Reply::Name { name } => Ok(name),
        Reply::Failed { why } => Err(why),
        other => Err(format!("unexpected answer: {other:?}")),
    }
}

pub fn end_process(pid: i64) -> bool {
    matches!(
        ask(&Request::EndProcess { pid }),
        Ok(Reply::Yes { it: true })
    )
}

pub fn hosted_count() -> usize {
    match ask(&Request::Count) {
        Ok(Reply::Count { n }) => n,
        _ => 0,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ironsight-daemon-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("control.sock")
    }

    #[test]
    fn a_request_and_its_reply_survive_the_wire() {
        let request = Request::Write {
            pane: "%3".into(),
            bytes: vec![0x1b, b'[', b'A'],
        };
        let line = serde_json::to_string(&request).unwrap();
        let back: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(back, Request::Write { bytes, .. } if bytes == vec![0x1b, b'[', b'A']));

        let reply = Reply::Failed {
            why: "no such session".into(),
        };
        let back: Reply = serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert!(matches!(back, Reply::Failed { why } if why == "no such session"));
    }

    #[test]
    fn answers_over_a_real_socket_and_survives_a_client_that_leaves() {
        let path = scratch("talks");
        let listening = path.clone();
        std::thread::spawn(move || {
            let _ = serve(listening);
        });

        // Wait for it to be listening rather than guessing how long that takes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Talk to this one rather than whatever is on the machine.
        let speak = |req: &Request| -> Reply {
            use std::os::unix::net::UnixStream;
            let stream = UnixStream::connect(&path).expect("the daemon is listening");
            let mut out = stream.try_clone().unwrap();
            let mut text = serde_json::to_string(req).unwrap();
            text.push('\n');
            out.write_all(text.as_bytes()).unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        };

        match speak(&Request::Hello) {
            Reply::Hello { wire, pid } => {
                assert_eq!(wire, WIRE);
                assert_eq!(pid, std::process::id(), "it is this process answering");
            }
            other => panic!("expected a greeting, got {other:?}"),
        }

        // A client that asks for something impossible is told, not dropped.
        match speak(&Request::Kill {
            pane: "not-a-session".into(),
        }) {
            Reply::Failed { .. } | Reply::Done => {}
            other => panic!("expected an answer, got {other:?}"),
        }

        // And the daemon is still there afterwards.
        assert!(matches!(speak(&Request::Count), Reply::Count { .. }));
    }

    #[test]
    fn refuses_a_wire_it_does_not_understand() {
        // What an older client would send after the format moved on.
        let bad = "{\"do\":\"somethingFromTheFuture\"}";
        assert!(serde_json::from_str::<Request>(bad).is_err());
    }
}

/// The daemon, dressed as a session backend.
///
/// Every function `tmux` and `host` offer, offered again, so that whether
/// sessions live in tmux, in this process, or in a daemon is a decision made
/// once at startup rather than a fork in every caller.
///
/// Anything that is pure translation — what bytes a key is, what a hint says —
/// is borrowed from `host` rather than re-implemented. Only the things that
/// need to reach the pseudo-terminals go over the wire.
pub mod backend {
    use super::*;
    use crate::host;

    pub const OUTLIVES_SCOPE: bool = true;
    pub const WHERE: &str = "Ironsight";

    pub fn outlives_ironsight() -> bool {
        OUTLIVES_SCOPE
    }

    pub fn where_name() -> &'static str {
        WHERE
    }

    /// Whether sessions can be held this way at all: a daemon is running, or
    /// one could be started.
    pub fn available() -> bool {
        super::running() || super::ensure_running().is_ok()
    }

    pub fn panes() -> Vec<Pane> {
        super::panes()
    }

    pub fn pane_for(pid: i64, cwd: &str, panes: &[Pane]) -> Option<Pane> {
        host::pane_for(pid, cwd, panes)
    }

    pub fn send_text(pane: &str, text: &str) -> Result<(), String> {
        if text.is_empty() {
            return Err("nothing to send".into());
        }
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\r');
        super::write_bytes(pane, &bytes)
    }

    pub fn send_key(pane: &str, key: &str) -> Result<(), String> {
        let bytes = host::named_key(key).ok_or_else(|| format!("no key called {key}"))?;
        super::write_bytes(pane, &bytes)
    }

    pub fn forward_key(
        pane: &str,
        code: crossterm::event::KeyCode,
        ctrl: bool,
    ) -> Result<(), String> {
        match host::key_bytes(code, ctrl) {
            Some(bytes) => super::write_bytes(pane, &bytes),
            None => Ok(()),
        }
    }

    pub fn frame(pane: &str, cols: u16, rows: u16) -> Option<crate::screen::Frame> {
        super::frame(pane, cols, rows)
    }

    pub fn capture(pane: &str) -> Option<String> {
        super::capture(pane)
    }

    /// The daemon owns the pseudo-terminal, so there is no size to hand back.
    pub fn release_frame(_pane: &str) {}

    pub fn new_session_with(
        cwd: &std::path::Path,
        argv: &[String],
        opening: &[String],
    ) -> Result<String, String> {
        super::ensure_running()?;
        super::start(cwd, argv, opening)
    }

    pub fn adopt(cwd: &std::path::Path, session_id: &str) -> Result<String, String> {
        super::ensure_running()?;
        super::adopt(cwd, session_id)
    }

    pub fn kill_session(session: &str) -> Result<(), String> {
        super::kill_session(session)
    }

    pub fn prune() -> Vec<String> {
        super::prune()
    }

    pub fn stop_all() -> Vec<String> {
        super::stop_all()
    }

    pub fn end_process(pid: i64) -> bool {
        super::end_process(pid)
    }

    pub fn hosted_count() -> usize {
        super::hosted_count()
    }

    /// There is no multiplexer to be inside.
    pub fn inside_tmux() -> bool {
        false
    }

    /// tmux binds a key so a person can get back out of a session. Here the
    /// way back is closing the window Ironsight put the session in, so there is
    /// nothing to hold.
    pub fn hold_way_back() -> bool {
        false
    }

    pub fn drop_way_back(_held: bool) {}

    /// Handing a terminal over to a session is `ironsight attach`, which is a
    /// command rather than something the engine does to the caller.
    pub fn attach(_session: &str) -> Result<bool, String> {
        Err("run `ironsight attach <session>` from a terminal".into())
    }

    pub fn open_window(session: &str) -> Result<String, String> {
        crate::control::open_terminal_with(&format!("ironsight attach {session}"))
    }

    pub fn attach_hint(session: &str) -> String {
        format!("attach with: ironsight attach {session}")
    }

    pub fn steer_hint(name: &str) -> String {
        format!("{name} is held by Ironsight and can be typed into from here")
    }

    pub fn unavailable_hint() -> &'static str {
        "Ironsight could not start the process that holds sessions"
    }

    pub fn where_hint(session: &str) -> String {
        format!("held by Ironsight · ironsight attach {session}")
    }
}
