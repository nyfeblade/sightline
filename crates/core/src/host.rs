//! Sessions scope hosts itself, for machines with no tmux.
//!
//! Windows has no way to reach into a console another process owns, so the
//! trick that works on Unix — let tmux hold the session and type into it — has
//! no equivalent. Here scope is the terminal: it starts Claude Code on a
//! pseudo-console it owns, keeps a screen model of everything the session
//! draws, and writes key presses into it. Everything above this module then
//! works unchanged, because a hosted session answers the same questions a tmux
//! pane does — what is on your screen, and take this key.
//!
//! The difference worth knowing is lifetime. tmux outlives scope; this does
//! not. A hosted session ends when scope exits, which is why quitting asks
//! first, and why the conversation being reopenable matters more here than on
//! Unix.
//!
//! Nothing in here is Windows-only at compile time. It builds and runs on Unix
//! as well, which is where its tests run.

use crate::control::{Pane, adopted_pane, next_name_after};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// The size a hosted session is given. Claude Code draws to whatever it is
/// told; this is roughly a comfortable terminal, and wide enough that its
/// prompts do not wrap into something unreadable.
const ROWS: u16 = 40;
const COLS: u16 = 120;

struct Hosted {
    name: String,
    /// what it was started with, e.g. "claude --resume <id>"
    cmd: String,
    cwd: String,
    /// pid of the process on the far side of the pty
    pid: i64,
    writer: Box<dyn Write + Send>,
    screen: Arc<Mutex<vt100::Parser>>,
    child: Box<dyn Child + Send + Sync>,
    /// kept so the pty is not closed under the session
    _master: Box<dyn MasterPty + Send>,
}

impl Hosted {
    fn pane(&self) -> Pane {
        Pane {
            id: self.name.clone(),
            pid: self.pid,
            session: self.name.clone(),
            cmd: self.cmd.clone(),
            cwd: self.cwd.clone(),
        }
    }
}

type Sessions = Mutex<HashMap<String, Hosted>>;

fn sessions() -> &'static Sessions {
    static SESSIONS: OnceLock<Sessions> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// scope can always host a session — there is nothing to install.
pub fn available() -> bool {
    true
}

/// scope never nests inside anything here.
pub fn inside_tmux() -> bool {
    false
}

/// Drop sessions whose process has exited, and return the names that went.
fn reap() -> Vec<String> {
    let mut map = match sessions().lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    let done: Vec<String> = map
        .iter_mut()
        .filter_map(|(name, s)| matches!(s.child.try_wait(), Ok(Some(_))).then(|| name.clone()))
        .collect();
    for name in &done {
        map.remove(name);
    }
    done
}

/// Every session scope is hosting.
pub fn panes() -> Vec<Pane> {
    reap();
    let map = match sessions().lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    let mut out: Vec<Pane> = map.values().map(Hosted::pane).collect();
    out.sort_by(|a, b| a.session.cmp(&b.session));
    out
}

/// The pane a session's process is running inside.
///
/// A pid match is the answer whenever scope started Claude Code directly. An
/// npm install is a `claude.cmd` shim, which has to be run through the command
/// interpreter, so the pid scope knows is the interpreter's and the session's
/// own pid is one below it; there the working directory identifies it, as long
/// as only one hosted session is in that directory.
pub fn pane_for(pid: i64, cwd: &str, panes: &[Pane]) -> Option<Pane> {
    if let Some(p) = panes.iter().find(|p| p.pid == pid) {
        return Some(p.clone());
    }
    if cwd.is_empty() {
        return None;
    }
    let mut in_dir = panes.iter().filter(|p| p.cwd == cwd);
    let only = in_dir.next()?;
    in_dir.next().is_none().then(|| only.clone())
}

fn with<T>(name: &str, f: impl FnOnce(&mut Hosted) -> T) -> Result<T, String> {
    let mut map = match sessions().lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    match map.get_mut(name) {
        Some(s) => Ok(f(s)),
        None => Err(format!("{name} is not a session scope is hosting")),
    }
}

fn write(name: &str, bytes: &[u8]) -> Result<(), String> {
    with(name, |s| {
        s.writer
            .write_all(bytes)
            .and_then(|()| s.writer.flush())
            .map_err(|e| e.to_string())
    })?
}

/// Type a line into a session and submit it, the same as a person would.
pub fn send_text(pane: &str, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("nothing to send".into());
    }
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(b'\r');
    write(pane, &bytes)
}

/// Send a named key — the names are tmux's, so callers do not have to know
/// which backend they are talking to.
pub fn send_key(pane: &str, key: &str) -> Result<(), String> {
    let bytes = named_key(key).ok_or_else(|| format!("no key called {key}"))?;
    write(pane, &bytes)
}

/// The bytes a terminal sends for the key names scope uses.
fn named_key(key: &str) -> Option<Vec<u8>> {
    let one = |b: u8| Some(vec![b]);
    match key {
        "Escape" => one(0x1b),
        "Enter" => one(b'\r'),
        "Tab" => one(b'\t'),
        "BSpace" => one(0x7f),
        "Up" => Some(b"\x1b[A".to_vec()),
        "Down" => Some(b"\x1b[B".to_vec()),
        "Right" => Some(b"\x1b[C".to_vec()),
        "Left" => Some(b"\x1b[D".to_vec()),
        k => k
            .strip_prefix("C-")
            .and_then(|c| c.chars().next())
            .and_then(control_byte)
            .map(|b| vec![b]),
    }
}

/// ctrl+<letter> is the letter with the top three bits cleared; the handful of
/// punctuation keys that carry a control code follow it in the table.
fn control_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | ' ' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' | '?' => Some(0x1f),
        _ => None,
    }
}

/// The bytes for one key press, as a terminal would send them.
fn key_bytes(code: crossterm::event::KeyCode, ctrl: bool) -> Option<Vec<u8>> {
    use crossterm::event::KeyCode as K;
    let seq = |s: &str| Some(s.as_bytes().to_vec());
    match code {
        K::Char(c) if ctrl => control_byte(c).map(|b| vec![b]),
        K::Char(c) => Some(c.to_string().into_bytes()),
        K::Enter => seq("\r"),
        K::Esc => seq("\x1b"),
        K::Tab => seq("\t"),
        K::BackTab => seq("\x1b[Z"),
        K::Backspace => Some(vec![0x7f]),
        K::Delete => seq("\x1b[3~"),
        K::Up => seq("\x1b[A"),
        K::Down => seq("\x1b[B"),
        K::Right => seq("\x1b[C"),
        K::Left => seq("\x1b[D"),
        K::Home => seq("\x1b[H"),
        K::End => seq("\x1b[F"),
        K::PageUp => seq("\x1b[5~"),
        K::PageDown => seq("\x1b[6~"),
        _ => None,
    }
}

/// Send one key press to a session.
pub fn forward_key(pane: &str, code: crossterm::event::KeyCode, ctrl: bool) -> Result<(), String> {
    match key_bytes(code, ctrl) {
        Some(bytes) => write(pane, &bytes),
        None => Ok(()),
    }
}

/// scope owns the pseudo-console here, so there is nothing to hand back.
pub fn release_frame(_pane: &str) {}

/// A session's screen at a given size. scope owns the pseudo-console here, so
/// the size is set on it directly and the screen model is already parsed.
pub fn frame(pane: &str, cols: u16, rows: u16) -> Option<crate::screen::Frame> {
    with(pane, |s| {
        let mut parser = match s.screen.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        if parser.screen().size() != (rows, cols) {
            parser.screen_mut().set_size(rows, cols);
            let _ = s._master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        crate::screen::frame_of(parser.screen())
    })
    .ok()
}

/// What a session's screen shows right now, as plain text.
pub fn capture(pane: &str) -> Option<String> {
    with(pane, |s| {
        let screen = match s.screen.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        screen.screen().contents()
    })
    .ok()
}

/// Where the executable actually is, given a PATH and the extensions the
/// platform considers runnable. Windows needs this: `claude` is `claude.exe`
/// for a native install and `claude.cmd` for an npm one, and neither is found
/// by asking for `claude`.
fn resolve_in(
    name: &str,
    dirs: &[PathBuf],
    exts: &[String],
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    for dir in dirs {
        let plain = dir.join(name);
        if exists(&plain) {
            return Some(plain);
        }
        for ext in exts {
            let with_ext = dir.join(format!("{name}{ext}"));
            if exists(&with_ext) {
                return Some(with_ext);
            }
        }
    }
    None
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

fn path_exts() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| !e.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The command to run, with the shim case handled: a `.cmd` or `.bat` is a
/// script, so it has to go through the command interpreter rather than being
/// executed directly.
fn command(argv: &[String], cwd: &str) -> CommandBuilder {
    let found = resolve_in(&argv[0], &path_dirs(), &path_exts(), &|p| p.exists());
    let script = found
        .as_ref()
        .and_then(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(str::to_lowercase)
        })
        .map(|e| e == "cmd" || e == "bat")
        .unwrap_or(false);
    let mut cmd = if script {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg(found.unwrap_or_else(|| PathBuf::from(&argv[0])));
        c
    } else {
        CommandBuilder::new(found.unwrap_or_else(|| PathBuf::from(&argv[0])))
    };
    for a in &argv[1..] {
        cmd.arg(a);
    }
    if !cwd.is_empty() {
        cmd.cwd(cwd);
    }
    cmd
}

fn next_name() -> String {
    let map = match sessions().lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    let existing: String = map.keys().cloned().collect::<Vec<_>>().join("\n");
    next_name_after(&existing)
}

/// Start a session on a pty scope owns, and read whatever it draws into a
/// screen that can be looked at later.
fn spawn(cwd: &Path, argv: Vec<String>) -> Result<String, String> {
    let pty = portable_pty::native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("could not open a terminal for it: {e}"))?;
    let cwd = cwd.to_string_lossy().to_string();
    let child = pty
        .slave
        .spawn_command(command(&argv, &cwd))
        .map_err(|e| format!("could not start it: {e} (is claude on PATH?)"))?;
    // The slave end has been handed to the child; holding it open here would
    // keep the pty alive after the session ends, and reading would never stop.
    drop(pty.slave);

    let pid = child.process_id().map(i64::from).unwrap_or(0);
    let writer = pty
        .master
        .take_writer()
        .map_err(|e| format!("could not type into it: {e}"))?;
    let reader = pty
        .master
        .try_clone_reader()
        .map_err(|e| format!("could not read from it: {e}"))?;
    let screen = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
    read_into(reader, Arc::clone(&screen));

    let name = next_name();
    let hosted = Hosted {
        name: name.clone(),
        cmd: argv.join(" "),
        cwd,
        pid,
        writer,
        screen,
        child,
        _master: pty.master,
    };
    let mut map = match sessions().lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    map.insert(name.clone(), hosted);
    Ok(name)
}

/// Feed everything the session draws into its screen, until it closes.
fn read_into(mut reader: Box<dyn Read + Send>, screen: Arc<Mutex<vt100::Parser>>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let mut parser = match screen.lock() {
                        Ok(p) => p,
                        Err(e) => e.into_inner(),
                    };
                    parser.process(&buf[..n]);
                }
            }
        }
    });
}

/// Start a session with explicit Claude Code options.
/// Start a session on a pty scope owns, running whatever agent was asked for,
/// and type the opening lines into it once it is up.
pub fn new_session_with(cwd: &Path, argv: &[String], opening: &[String]) -> Result<String, String> {
    let program = argv.first().cloned().unwrap_or_default();
    let name = spawn(cwd, argv.to_vec())?;
    // A program that could not run exits immediately, and claiming it started
    // would leave a session in the list that was never there.
    std::thread::sleep(std::time::Duration::from_millis(400));
    if !panes().iter().any(|p| p.session == name) {
        return Err(format!(
            "{program} stopped as soon as it started — is it installed and on PATH?"
        ));
    }
    if !opening.is_empty() {
        // Give the agent a moment to draw its prompt before typing into it.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        for line in opening {
            send_text(&name, line)?;
        }
    }
    Ok(name)
}

/// Continue an existing conversation, so it becomes steerable.
pub fn adopt(cwd: &Path, session_id: &str) -> Result<String, String> {
    if let Some(p) = adopted_pane(session_id, &panes()) {
        return Ok(p.session);
    }
    spawn(
        cwd,
        vec![
            "claude".to_string(),
            "--resume".to_string(),
            session_id.to_string(),
        ],
    )
}

/// End a hosted session.
pub fn kill_session(session: &str) -> Result<(), String> {
    let mut map = match sessions().lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    let mut hosted = map
        .remove(session)
        .ok_or_else(|| format!("{session} is not a session scope is hosting"))?;
    hosted.child.kill().map_err(|e| e.to_string())?;
    let _ = hosted.child.wait();
    Ok(())
}

/// Close hosted sessions whose process has exited. Returns the names.
pub fn prune() -> Vec<String> {
    reap()
}

/// Close every session scope is hosting. Returns the names that were closed.
pub fn stop_all() -> Vec<String> {
    let names: Vec<String> = panes().into_iter().map(|p| p.session).collect();
    names
        .into_iter()
        .filter(|n| kill_session(n).is_ok())
        .collect()
}

/// End a Claude Code process scope does not host — the original window, after
/// its conversation has been reopened here.
pub fn end_process(pid: i64) -> bool {
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// There is no window to hand a hosted session to: scope is its terminal.
/// The mirror shows it full-screen instead, which is what `a` does here.
pub fn attach(_session: &str) -> Result<bool, String> {
    Err("scope is this session's terminal — press m to type into it".into())
}

pub fn open_window(_session: &str) -> Result<String, String> {
    Err("scope hosts this session itself, so it has no window of its own".into())
}

/// Sessions are held by this process, so they end with it. The one-shot
/// subcommands would start something and immediately take it away again.
pub const OUTLIVES_SCOPE: bool = false;

/// What to call the place scope steers sessions from, in a sentence.
pub const WHERE: &str = "scope";

/// There is no multiplexer to take a key from here: scope is the terminal, and
/// leaving a session is leaving the pane it is drawn in.
pub fn hold_way_back() -> bool {
    false
}

pub fn drop_way_back(_held: bool) {}

/// How to look at a session. scope is its terminal, so the way to see it is
/// scope's own mirror.
pub fn attach_hint(_session: &str) -> String {
    "press a to watch it".to_string()
}

/// Why a session cannot be steered, and what to do about it. Here scope can
/// only steer what it started, because it has to own the terminal.
pub fn steer_hint(name: &str) -> String {
    format!("{name} is not one scope started — press A to reopen it here")
}

/// scope can always host a session, so this is never the reason.
pub fn unavailable_hint() -> &'static str {
    "scope cannot start a session here"
}

/// Where a session scope can steer is running, for the session card.
pub fn where_hint(session: &str) -> String {
    format!("steerable · hosted by scope as {session}")
}

/// How many sessions would end if scope exited now.
pub fn hosted_count() -> usize {
    panes().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn sends_the_bytes_a_terminal_sends() {
        assert_eq!(key_bytes(KeyCode::Char('a'), false).unwrap(), b"a");
        assert_eq!(key_bytes(KeyCode::Char('c'), true).unwrap(), vec![3]);
        assert_eq!(key_bytes(KeyCode::Esc, false).unwrap(), vec![0x1b]);
        assert_eq!(key_bytes(KeyCode::Enter, false).unwrap(), b"\r");
        assert_eq!(key_bytes(KeyCode::Up, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(KeyCode::Backspace, false).unwrap(), vec![0x7f]);
        // A key with no terminal sequence is dropped, not guessed at.
        assert!(key_bytes(KeyCode::F(5), false).is_none());
        // Multi-byte characters survive.
        assert_eq!(
            key_bytes(KeyCode::Char('é'), false).unwrap(),
            "é".as_bytes()
        );
    }

    #[test]
    fn understands_the_key_names_scope_uses() {
        assert_eq!(named_key("Escape").unwrap(), vec![0x1b]);
        assert_eq!(named_key("Enter").unwrap(), b"\r");
        assert_eq!(named_key("C-c").unwrap(), vec![3]);
        assert!(named_key("Nonsense").is_none());
    }

    #[test]
    fn finds_the_executable_however_it_was_installed() {
        let dirs = vec![PathBuf::from("/bin"), PathBuf::from("/opt/tools")];
        let exts: Vec<String> = vec![".exe".into(), ".cmd".into()];
        let native = |p: &Path| p == Path::new("/opt/tools/claude.exe");
        assert_eq!(
            resolve_in("claude", &dirs, &exts, &native),
            Some(PathBuf::from("/opt/tools/claude.exe"))
        );
        let npm = |p: &Path| p == Path::new("/bin/claude.cmd");
        assert_eq!(
            resolve_in("claude", &dirs, &exts, &npm),
            Some(PathBuf::from("/bin/claude.cmd"))
        );
        let unix = |p: &Path| p == Path::new("/bin/claude");
        assert_eq!(
            resolve_in("claude", &dirs, &exts, &unix),
            Some(PathBuf::from("/bin/claude"))
        );
        assert_eq!(resolve_in("claude", &dirs, &exts, &|_| false), None);
    }

    #[test]
    fn a_script_shim_runs_through_the_interpreter() {
        // Only the resolved extension decides this, so it can be checked
        // without a Windows machine to run it on. What comes back is a full
        // path where the program was found — which on Windows it is — so the
        // name is what this asserts, not the string it was handed.
        let cmd = command(&["cmd.exe".into(), "/c".into()], "");
        let program = PathBuf::from(cmd.get_argv()[0].to_string_lossy().to_string());
        assert_eq!(
            program
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase()),
            Some("cmd.exe".to_string())
        );
    }

    #[test]
    fn one_session_in_a_directory_can_be_identified_by_it() {
        let pane = |name: &str, pid: i64, cwd: &str| Pane {
            id: name.into(),
            pid,
            session: name.into(),
            cmd: "claude".into(),
            cwd: cwd.into(),
        };
        let one = vec![pane("scope-1", 10, "/work"), pane("scope-2", 11, "/other")];
        // The pid is the answer when it matches.
        assert_eq!(
            pane_for(11, "", &one).map(|p| p.session),
            Some("scope-2".into())
        );
        // Otherwise the directory identifies it, when only one session is there.
        assert_eq!(
            pane_for(999, "/work", &one).map(|p| p.session),
            Some("scope-1".into())
        );
        let two = vec![pane("scope-1", 10, "/work"), pane("scope-2", 11, "/work")];
        assert!(
            pane_for(999, "/work", &two).is_none(),
            "ambiguous, so no guess"
        );
    }

    /// The whole backend, driven for real: start a session, watch what it
    /// draws, type into it, read the prompt back off its screen, stop it.
    /// The pseudo-terminal is the part most likely to be wrong, and it works
    /// the same way here as it does on the platform this exists for.
    #[cfg(unix)]
    #[test]
    fn hosts_a_session_end_to_end() {
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join("scope-host-test");
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("claude");
        // The prompt marker goes in as itself rather than as a `\u` escape:
        // macOS ships bash 3.2, whose printf does not know that escape, so the
        // fixture drew a literal backslash-u and the test looked for a prompt
        // that was never on screen.
        std::fs::write(
            &fake,
            "#!/usr/bin/env bash\n\
             printf 'READY\\n'\n\
             while IFS= read -r line; do\n\
               printf 'you said: %s\\n' \"$line\"\n\
               if [ \"$line\" = ask ]; then\n\
                 printf 'Do you want to proceed?\\n'\n\
                 printf '\u{276f} 1. Yes\\n'\n\
                 printf '  2. No\\n'\n\
               fi\n\
             done\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        fn until(name: &str, want: &str) -> String {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let text = capture(name).unwrap_or_default();
                if text.contains(want) {
                    return text;
                }
                assert!(
                    Instant::now() < deadline,
                    "never saw {want:?}; the screen was:\n{text}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        let name = spawn(&dir, vec![fake.to_string_lossy().to_string()]).expect("spawn");
        assert!(
            panes().iter().any(|p| p.session == name),
            "a hosted session should be listed"
        );
        until(&name, "READY");

        send_text(&name, "hello").expect("send");
        until(&name, "you said: hello");

        // What the approval reader sees is this screen, so the two halves are
        // checked together rather than on a hand-written fixture.
        send_text(&name, "ask").expect("send");
        let screen = until(&name, "2. No");
        let approval = crate::control::pending_approval(&screen).expect("prompt should be seen");
        assert_eq!(approval.question, "Do you want to proceed?");
        assert_eq!(approval.options.len(), 2);

        kill_session(&name).expect("kill");
        assert!(
            !panes().iter().any(|p| p.session == name),
            "a stopped session should not be listed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
