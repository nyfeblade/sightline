//! Ironsight — watch every Claude Code session on this machine, live.

use ironsight_core::{app, bootstrap, bus, checks, control, gateway, git, owned, session, work};

mod ui;

use anyhow::Result;
use app::{App, Prompt, View};
use crossterm::event::{
    self as cevent, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::DefaultTerminal;
use session::Status;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const USAGE: &str = "\
Ironsight — live view of what Claude Code is doing

usage: Ironsight [options]
       ironsight new [path] [--agent A] [--name N] [--model M] [--effort E]
                 [--permission-mode P] [--prompt T] [--worktree BRANCH]
                 [--task WHAT] [--parent WHO]
                               start a session and exit; --agent picks which
                               agent to run (claude, codex, gemini, aider, or
                               any command), default claude
       ironsight send <who> <text> type a line into a running session and submit it
       ironsight adopt <who>        (re)open a conversation in tmux so it can be steered
       ironsight prune              close Ironsight sessions whose process has exited
       ironsight doctor             check everything Ironsight needs is installed
       ironsight run [--model M] <prompt>
                               run a session Ironsight owns: structured JSON, no
                               terminal, no scraping. Streams what it does as it
                               happens and exits when the turn is done
       ironsight serve              hold sessions in a process of Ironsight's own,
                                so they outlive every window. Started for you
                                when it is needed; run it yourself to watch it
       ironsight attach <who>       hand this terminal to a session Ironsight holds
                                — the way out when the window is the problem
       ironsight stop [who|--all]   stop one session, or everything Ironsight started
       ironsight waiting            list sessions blocked on a prompt
       ironsight approve <who> [n]  answer a blocked session (default option 1)
       ironsight events [--since N] [--json]
                               follow everything happening on this machine;
                               attaches to a running Ironsight if there is one,
                               and watches the machine itself if there is not
       ironsight tasks              what each session was asked to do
       ironsight assign <who> <text>
                               give a session an assignment
       ironsight note <task> <text> append what was learned to a task
       ironsight refute <task> <command>
                               name something that would show this work is
                               wrong. The command must fail; if it succeeds the
                               claim is refused. Without one, work can be
                               checked but never verified
       ironsight claim <who>        say a session's work is finished; the checks decide
       ironsight check <who>        run this project's checks now and report
       ironsight trust [path]       approve a project's checks, having read them.
                               Nothing runs from a .ironsight/checks.toml until
                               you have, and it asks again if the file changes
       ironsight foreman [--every N]
                               watch for claimed work and refuse what does not
                               pass its checks. Never writes code, never
                               restarts anything, never guesses

options:
  --since <dur>   include sessions touched within this window (default 24h)
                  accepts 90m, 12h, 7d, or plain seconds
  --live          only sessions with a running claude process
  --cost          show API-equivalent cost (default: subscription view)
  --view <name>   start on feed, files, or stats
  --plain         no colour (also honours NO_COLOR)
  --no-mouse      do not capture the mouse (restores terminal text selection)
  --once          print a one-shot table instead of the live view
  --root <path>   transcript root (default ~/.claude/projects)
  -h, --help      this text
  -V, --version   version

keys: j/k select · J/K feed · enter detail · f filter · l live · ? help · q quit";

fn parse_since(s: &str) -> Option<Duration> {
    let (num, mult) = match s.chars().last()? {
        'd' => (&s[..s.len() - 1], 86_400),
        'h' => (&s[..s.len() - 1], 3_600),
        'm' => (&s[..s.len() - 1], 60),
        's' => (&s[..s.len() - 1], 1),
        _ => (s, 1),
    };
    num.trim()
        .parse::<u64>()
        .ok()
        .map(|n| Duration::from_secs(n * mult))
}

fn report(o: &checks::Outcome) {
    let mark = match &o.state {
        checks::State::Passed => "ok  ".to_string(),
        checks::State::Failed { .. } => "FAIL".to_string(),
        checks::State::Unknown { .. } => "??  ".to_string(),
    };
    let why = match &o.state {
        checks::State::Passed => String::new(),
        checks::State::Failed { first } => format!(" · {first}"),
        checks::State::Unknown { why } => format!(" · {why}"),
    };
    println!("{mark} {:<10} {:>6}ms{why}", o.name, o.ms);
}

/// Run a session's project checks, and — when asked — record what they decided.
///
/// The recording is the whole point. A task reaches `Verified` here and nowhere
/// else, and a claim that fails goes back to `Working` with the first failure
/// attached, which is the message the agent that claimed it will read.
fn verify(app: &mut App, id: &str, record: bool) -> Result<Vec<checks::Outcome>, String> {
    let session = app
        .sessions
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("no session {id}"))?;
    let cwd = session.cwd.clone();
    if cwd.is_empty() {
        return Err("that session has no directory to run anything in".into());
    }
    let cwd = std::path::PathBuf::from(&cwd);
    let (root, suite) = checks::Suite::find(&cwd)?.ok_or_else(|| {
        format!(
            "{} has no {} — a project has to say what finished means",
            cwd.display(),
            checks::FILE
        )
    })?;
    let mut env = std::collections::HashMap::new();
    if let Some(tree) = git::status(&cwd) {
        env.insert("BRANCH".to_string(), tree.branch);
    }
    // Nothing runs until these exact commands have been approved. A checks
    // file arrives with a repository, and cloning something should not run
    // whatever its author felt like running.
    if !checks::trusted(&root, &suite) {
        return Err(checks::untrusted_hint(&root, &suite));
    }
    let outcomes = suite.run(&root, &env);
    if !record {
        return Ok(outcomes);
    }

    let Some(task) = app.work.task_for(id).map(|t| t.id.clone()) else {
        return Ok(outcomes);
    };
    let short = &id[..id.len().min(8)];
    if checks::Suite::verified(&outcomes) {
        let names: Vec<&str> = outcomes.iter().map(|o| o.name.as_str()).collect();
        for o in &outcomes {
            let ev = bus::Event::new(
                id,
                "foreman",
                bus::Kind::ChecksPassed {
                    suite: o.name.clone(),
                    ms: o.ms,
                },
            );
            app.publish(ev);
        }
        // The checks passing is a floor, never a finish.
        //
        // A suite can only say that the failures it is able to express did not
        // happen. Writing "verified" on the strength of that manufactures
        // confidence in work nobody has tried to break, which is worse than
        // saying nothing — so this stops at `Checked`, and what carries a task
        // past it is an attempt to show it is wrong that failed.
        let _ = app.work.set_state(&task, work::State::Checked);
        let _ = app
            .work
            .note(&task, &format!("checks passed: {}", names.join(", ")));
        let refutations = app
            .work
            .get(&task)
            .map(|t| t.refutes.clone())
            .unwrap_or_default();
        if refutations.is_empty() {
            println!(
                "{task} checked · {short} · {} passed. Not verified: nothing says what \
                 wrong would look like (ironsight refute {task} <command>)",
                names.join(", ")
            );
            app.work.flush();
            return Ok(outcomes);
        }
        let proven: Vec<String> = app
            .work
            .get(&task)
            .map(|t| t.proven.clone())
            .unwrap_or_default();
        let mut stood = 0;
        let mut unproven: Vec<String> = Vec::new();
        for command in &refutations {
            let (verdict, ms) = checks::refute(command, &root, &env);
            match verdict {
                checks::Verdict::Stands => {
                    // Standing is only evidence if this refutation has ever
                    // been seen to catch anything. One that cannot fire stands
                    // for ever and proves nothing.
                    if proven.iter().any(|p| p == command) {
                        stood += 1;
                        println!("ok   refutation {ms:>6}ms · did not fire · {command}");
                    } else {
                        unproven.push(command.clone());
                        println!(
                            "??   refutation {ms:>6}ms · did not fire, and never has · {command}"
                        );
                    }
                }
                checks::Verdict::Refuted { how } => {
                    // It caught something. That is bad news for the claim and
                    // good news for the refutation: it is now a demonstrated
                    // instrument, and standing later will mean something.
                    app.work.proved(&task, command);
                    let why = format!("refuted: {how} succeeded, and it was written to fail");
                    let _ = app.work.set_state(&task, work::State::Working);
                    let _ = app.work.note(&task, &why);
                    println!("{task} refused · {short} · {why}");
                    let ev = bus::Event::new(
                        id,
                        "foreman",
                        bus::Kind::ChecksFailed {
                            suite: "refutation".into(),
                            first: how,
                        },
                    );
                    app.publish(ev);
                    app.work.flush();
                    return Ok(outcomes);
                }
                checks::Verdict::Unrunnable { why } => {
                    // Neither evidence for nor against. It stays checked, and
                    // says why it got no further.
                    let note = format!("could not run a refutation · {command} · {why}");
                    let _ = app.work.note(&task, &note);
                    println!("??   refutation {:>6}ms · {note}", ms);
                }
            }
        }
        if stood == refutations.len() {
            let _ = app.work.set_state(&task, work::State::Verified);
            let _ = app.work.note(
                &task,
                &format!("verified: {stood} demonstrated refutation(s) tried, none fired"),
            );
            println!("{task} verified · {short} · {stood} attempt(s) to break it failed");
        } else if !unproven.is_empty() {
            let note = format!(
                "not verified: {} refutation(s) have never caught anything, so their \
                 standing is not evidence — {}",
                unproven.len(),
                unproven.join(", ")
            );
            let _ = app.work.note(&task, &note);
            println!("{task} checked · {short} · {note}");
        } else {
            println!(
                "{task} checked · {short} · not verified: {} of {} refutations could not be run",
                refutations.len() - stood,
                refutations.len()
            );
        }
    } else {
        let refusal = checks::Suite::refusal(&outcomes).unwrap_or_else(|| "not verified".into());
        // Back to working, not blocked: there is nothing to wait for, only
        // something to fix.
        let _ = app.work.set_state(&task, work::State::Working);
        let _ = app.work.note(&task, &refusal);
        println!("{task} refused · {short} · {refusal}");
        for o in &outcomes {
            if let checks::State::Failed { first } = &o.state {
                let ev = bus::Event::new(
                    id,
                    "foreman",
                    bus::Kind::ChecksFailed {
                        suite: o.name.clone(),
                        first: first.clone(),
                    },
                );
                app.publish(ev);
            }
        }
    }
    app.work.flush();
    Ok(outcomes)
}

/// Hand this terminal to a session, until the way-back key is pressed.
///
/// The screen is redrawn from the session's own, and every key goes straight to
/// it. This is a poll rather than a stream — the daemon answers questions, it
/// does not push — which costs a frame of latency and buys a protocol nobody
/// has to debug at the moment a fleet is wedged.
fn attach_to(pane: &str, name: &str) -> Result<()> {
    use crossterm::{cursor, execute, style, terminal};
    use std::io::Write;

    let mut out = std::io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;
    restore_terminal_however_this_ends();

    let result = (|| -> Result<()> {
        let mut last: Vec<Vec<ironsight_core::screen::Run>> = Vec::new();
        loop {
            let (cols, rows) = terminal::size()?;
            if let Some(frame) = control::frame(pane, cols, rows.saturating_sub(1)) {
                // Only the lines that changed, or attaching to a busy session
                // flickers the whole screen thirty times a second.
                for (y, line) in frame.lines.iter().enumerate() {
                    if last.get(y) == Some(line) {
                        continue;
                    }
                    execute!(
                        out,
                        cursor::MoveTo(0, y as u16),
                        terminal::Clear(terminal::ClearType::UntilNewLine)
                    )?;
                    for run in line {
                        let mut style = style::ContentStyle::new();
                        if let Some(c) = run.fg.as_deref().and_then(colour) {
                            style.foreground_color = Some(c);
                        }
                        if let Some(c) = run.bg.as_deref().and_then(colour) {
                            style.background_color = Some(c);
                        }
                        if run.bold {
                            style.attributes.set(style::Attribute::Bold);
                        }
                        if run.underline {
                            style.attributes.set(style::Attribute::Underlined);
                        }
                        if run.inverse {
                            style.attributes.set(style::Attribute::Reverse);
                        }
                        execute!(out, style::PrintStyledContent(style.apply(&run.text)))?;
                    }
                }
                last = frame.lines.clone();
                let (cy, cx) = frame.cursor;
                execute!(
                    out,
                    cursor::MoveTo(0, rows.saturating_sub(1)),
                    terminal::Clear(terminal::ClearType::UntilNewLine),
                    style::Print(format!("── {name} · ctrl+] to let go ")),
                    cursor::MoveTo(cx, cy),
                )?;
                out.flush()?;
            }

            if cevent::poll(Duration::from_millis(40))? {
                if let Event::Key(key) = cevent::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    // Every shape ctrl+] takes on a terminal without the kitty
                    // keyboard protocol, plus F12 — the same universal escape
                    // the live view uses. Watching only for Char(']') made
                    // attach a one-way door on macOS Terminal and most SSH.
                    if leaves_passthrough(key.code, ctrl) {
                        return Ok(());
                    }
                    let _ = control::forward_key(pane, key.code, ctrl);
                }
            }
        }
    })();

    execute!(out, cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    result
}

/// `#rrggbb`, which is how a screen run carries a colour.
fn colour(css: &str) -> Option<crossterm::style::Color> {
    let hex = css.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let n = |a: usize, b: usize| u8::from_str_radix(&hex[a..b], 16).ok();
    Some(crossterm::style::Color::Rgb {
        r: n(0, 2)?,
        g: n(2, 4)?,
        b: n(4, 6)?,
    })
}

/// Tokens, in the shortest form that is still honest.
fn tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.0}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}

/// A session id prefix, the name you gave it, or its tmux session name.
/// Every subcommand that takes a "who" accepts all three, because which one a
/// person has to hand depends on where they are looking.
fn resolve(app: &App, who: &str) -> Option<String> {
    app.sessions
        .iter()
        .find(|s| {
            let pane = app
                .steer
                .get(&s.id)
                .map(|p| p.session.clone())
                .unwrap_or_default();
            s.id.starts_with(who) || s.label().eq_ignore_ascii_case(who) || pane == who
        })
        .map(|s| s.id.clone())
}

fn main() -> Result<()> {
    let mut since = Duration::from_secs(24 * 3_600);
    let mut only_live = false;
    let mut once = false;
    let mut show_cost = false;
    let mut view = View::Feed;
    let mut plain = std::env::var_os("NO_COLOR").is_some();
    let mut mouse = true;
    let mut root = app::default_root();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // Where Ironsight holds sessions itself, a subcommand would start or steer
    // something and then exit, taking it with it. Say so rather than doing it.
    const ONE_SHOT: [&str; 7] = [
        "new", "send", "adopt", "approve", "waiting", "stop", "prune",
    ];
    if let Some(cmd) = args.first() {
        if !control::outlives_ironsight() && ONE_SHOT.contains(&cmd.as_str()) {
            anyhow::bail!(
                "scope holds sessions itself on this platform, so they end when it exits.\n\
                 `Ironsight {cmd}` would do that immediately — run Ironsight and use it from there."
            );
        }
    }

    if args.first().map(String::as_str) == Some("stop") {
        let who = args.get(1).map(String::as_str).unwrap_or("--all");
        if who == "--all" {
            let closed = control::stop_all();
            if closed.is_empty() {
                println!("nothing of Ironsight's was running");
            } else {
                println!("stopped {}", closed.join(", "));
            }
            return Ok(());
        }
        let panes = control::panes();
        let target = panes
            .iter()
            .find(|p| p.session == who)
            .map(|p| p.session.clone())
            .ok_or_else(|| anyhow::anyhow!("no session called {who} — try ironsight stop --all"))?;
        control::kill_session(&target).map_err(|e| anyhow::anyhow!(e))?;
        println!("stopped {target}");
        return Ok(());
    }

    // `doctor` answers the question an app launched from a dock cannot ask a
    // person: is everything it needs actually here.
    if args.first().map(String::as_str) == Some("doctor") {
        let checks = bootstrap::assess(&bootstrap::probe(&app::default_root()));
        for c in &checks {
            let mark = if c.ok { "ok  " } else { "MISS" };
            println!("{mark} {:<14} {}", c.name, c.detail);
            if let Some(fix) = &c.fix {
                println!("     {:<14} {fix}", "");
            }
        }
        if bootstrap::ready(&checks) {
            println!("\nready");
            return Ok(());
        }
        anyhow::bail!("something required is missing");
    }

    // The daemon. Nothing but the sessions and a socket: everything about what
    // a session *means* stays in the front ends, which read the same files they
    // always read.
    // A session Ironsight owns, spoken to over the protocol rather than a
    // terminal. One-shot: send the prompt, stream what happens, exit when the
    // turn finishes. This is the seam the foreman and chief will drive.
    if args.first().map(String::as_str) == Some("run") {
        // `--model M` is a leading flag; everything from the first non-flag word
        // on is the prompt, verbatim. Scanning the whole argv for `--model`
        // would steal a word out of a prompt that merely mentions it — "explain
        // the --model flag" would lose "--model flag" silently.
        let mut model: Option<String> = None;
        let mut it = args[1..].iter().peekable();
        while let Some(a) = it.peek() {
            match a.as_str() {
                "--model" => {
                    it.next();
                    model = it.next().cloned();
                }
                // An explicit end-of-flags marker, for a prompt that really does
                // begin with a dash.
                "--" => {
                    it.next();
                    break;
                }
                _ => break,
            }
        }
        let prompt = it.cloned().collect::<Vec<_>>().join(" ");
        if prompt.trim().is_empty() {
            anyhow::bail!("usage: ironsight run [--model M] <prompt>");
        }

        let program = control::claude_program();
        let cwd = std::env::current_dir()?;
        let session_id = format!("owned-{}", std::process::id());
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_reader = done.clone();

        let mut owned = owned::OwnedSession::start_with(
            &program,
            &cwd,
            model.as_deref(),
            &session_id,
            "claude",
            // A one-shot command: let claude's diagnostics reach the terminal,
            // so a startup or auth failure is visible rather than a blank line.
            owned::Stderr::Inherit,
            move |ev| {
                println!("{}", ev.human());
                use std::io::Write;
                let _ = std::io::stdout().flush();
                // The turn is over when the session goes back to waiting.
                if matches!(ev.kind, bus::Kind::SessionWaiting) {
                    done_reader.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
        )
        .map_err(|e| anyhow::anyhow!("could not start an owned session: {e}"))?;

        owned.send(&prompt)?;
        owned.close_input();

        // Wait for the turn, bounded so a hung agent does not hang the command.
        let deadline = Instant::now() + Duration::from_secs(600);
        while Instant::now() < deadline {
            if done.load(std::sync::atomic::Ordering::Relaxed) || !owned.alive() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        owned.stop();
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("serve") {
        let path = ironsight_core::daemon::default_path();
        if ironsight_core::daemon::running() {
            anyhow::bail!("one is already listening on {}", path.display());
        }
        println!("holding sessions · {}", path.display());
        ironsight_core::daemon::serve(path)?;
        return Ok(());
    }

    // The way back in when the window is the problem.
    //
    // tmux gave this for free — `tmux attach` and you are in the session. Held
    // by Ironsight there has to be something that does the same, and it has to
    // exist before anyone relies on the daemon rather than after.
    if args.first().map(String::as_str) == Some("attach") {
        let who = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: ironsight attach <who>"))?;
        let pane = control::panes()
            .into_iter()
            .find(|p| p.session == *who || p.id == *who)
            .ok_or_else(|| anyhow::anyhow!("no session called {who}"))?;
        return attach_to(&pane.id, &pane.session);
    }

    if args.first().map(String::as_str) == Some("prune") {
        let closed = control::prune();
        if closed.is_empty() {
            println!("nothing to tidy up — everything Ironsight started is still running");
        } else {
            println!("closed {}", closed.join(", "));
        }
        return Ok(());
    }

    if matches!(
        args.first().map(String::as_str),
        Some("waiting") | Some("approve") | Some("adopt")
    ) {
        // Reopening a conversation works whether or not it is still running, so
        // adopt looks at everything; the other two only care about live ones.
        let only_live = args[0] != "adopt";
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            only_live,
        );
        app.rescan_panes();
        app.probe();
        if args[0] == "waiting" {
            if app.approvals.is_empty() {
                println!("nothing is waiting");
                return Ok(());
            }
            for s in &app.sessions {
                if let Some(a) = app.approvals.get(&s.id) {
                    println!("{:<24} {}", s.label(), a.question);
                    for o in &a.options {
                        println!("{:<24}   {o}", "");
                    }
                }
            }
            return Ok(());
        }
        if args[0] == "adopt" {
            let who = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: ironsight adopt <who>"))?;
            let idx = app
                .sessions
                .iter()
                .position(|s| s.id.starts_with(who.as_str()) || s.label().eq_ignore_ascii_case(who))
                .ok_or_else(|| anyhow::anyhow!("no session matching {who}"))?;
            app.sel = idx;
            app.adopt();
            println!("{}", app.note);
            return Ok(());
        }
        let who = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: ironsight approve <who> [n]"))?;
        let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
        let idx = app
            .sessions
            .iter()
            .position(|s| s.id.starts_with(who.as_str()) || s.label().eq_ignore_ascii_case(who))
            .ok_or_else(|| anyhow::anyhow!("no live session matching {who}"))?;
        app.sel = idx;
        app.answer(n);
        println!("{}", app.note);
        return Ok(());
    }

    // The stream, for anything that is not Ironsight.
    //
    // If an Ironsight is running its socket is the source, so several consumers
    // see one consistent stream. If none is, this becomes the watcher itself —
    // which is the layer being useful with nothing above it, rather than a
    // window onto something else that must already be open.
    if args.first().map(String::as_str) == Some("events") {
        let json = args.iter().any(|a| a == "--json");
        let since: Option<u64> = args
            .iter()
            .position(|a| a == "--since")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse().ok());
        let dir = app::data_dir();
        let show = |ev: &bus::Event| {
            println!("{}", if json { ev.line() } else { ev.human() });
            // Piped into something that has stopped reading, there is nothing
            // useful left to do, and a broken pipe is not an error worth a
            // stack trace.
            use std::io::Write;
            std::io::stdout().flush().ok();
        };

        let mut last = since.unwrap_or(0);
        if let Some(from) = since {
            let replayed = bus::replay_from(&dir.join("events.jsonl"), from);
            // Say so before the events, not after: a consumer restarting needs
            // to know it has a hole before it treats what follows as complete.
            if replayed.missed > 0 {
                eprintln!(
                    "(missed {} event(s) before this point — they rotated out of the journal)",
                    replayed.missed
                );
            }
            for ev in replayed.events {
                last = last.max(ev.seq);
                show(&ev);
            }
        }

        let sock = dir.join("events.sock");
        if sock.exists() {
            match gateway::connect(&sock) {
                Ok(live) => {
                    // Connected first, so nothing published from here on is
                    // missed. Now top the journal up from where the replay above
                    // stopped: that catches anything the publisher wrote between
                    // that replay and this connect — the gap `--since` exists to
                    // close. The live stream is then deduped against it by seq,
                    // so an event in both is shown once.
                    for ev in bus::replay_from(&dir.join("events.jsonl"), last).events {
                        if ev.seq > last {
                            last = ev.seq;
                            show(&ev);
                        }
                    }
                    for ev in live {
                        if ev.seq > last {
                            last = ev.seq;
                            show(&ev);
                        }
                    }
                    return Ok(());
                }
                // The Ironsight that owned it exited between the check and the
                // connect; fall through and watch the machine directly.
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
                Err(e) => return Err(e.into()),
            }
        }

        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(24 * 3_600),
            false,
        );
        if !app.with_stream().map_err(|e| anyhow::anyhow!(e))? {
            // Another Ironsight took the socket between the check above and the
            // bind. Read from it rather than watching the machine twice.
            gateway::follow(&sock, |ev| show(&ev))?;
            return Ok(());
        }
        let sub = app.bus.subscribe();
        app.rescan_panes();
        let mut lost = 0;
        loop {
            app.refresh();
            app.probe();
            for ev in sub.drain() {
                let _ = last;
                show(&ev);
            }
            if sub.lost() > lost {
                eprintln!("(missed {} events — reading too slowly)", sub.lost() - lost);
                lost = sub.lost();
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    if args.first().map(String::as_str) == Some("tasks") {
        // The sessions come too, because cost attributed to a piece of work
        // rather than to a process is the point of having a tree at all — a
        // supervisor's line shows what its workers spent as well as its own.
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            false,
        );
        app.with_state();
        if app.work.tasks().is_empty() {
            println!("nothing has been assigned — try: ironsight assign <who> <what>");
            return Ok(());
        }
        let rolled = app.rolled_up();
        let store = &app.work;
        // Ordered by the shape of the work, so a supervisor's workers sit under
        // it rather than beside it.
        let sessions: Vec<String> = store.tasks().iter().map(|t| t.session.clone()).collect();
        for (session, depth) in store.ordered(&sessions) {
            let Some(task) = store
                .task_for(&session)
                .or_else(|| store.tasks().iter().rev().find(|t| t.session == session))
            else {
                continue;
            };
            let pad = "  ".repeat(depth);
            let short = session.chars().take(8).collect::<String>();
            let cost = rolled.get(&session).copied().unwrap_or_default();
            let spend = if cost.output > 0 {
                format!("  [{} out · ${:.2}]", tokens(cost.output), cost.estimate)
            } else {
                String::new()
            };
            println!(
                "{pad}{:<4} {:<10} {short}  {}{spend}",
                task.id,
                task.state.label(),
                task.assignment
            );
            for note in &task.notes {
                println!("{pad}       · {}", note.text);
            }
        }
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("assign") {
        let who = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: ironsight assign <who> <what>"))?;
        let what = args[2..].join(" ");
        if what.is_empty() {
            anyhow::bail!("usage: ironsight assign <who> <what>");
        }
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            false,
        );
        // Only the store: an Ironsight may well be running, and this command has
        // no business taking the stream from it.
        app.with_state();
        app.rescan_panes();
        let id = resolve(&app, who).ok_or_else(|| anyhow::anyhow!("no session matching {who}"))?;
        let task = app.assign(&id, &what);
        println!("{task} assigned to {}", &id[..id.len().min(8)]);
        return Ok(());
    }

    // Verification, and the thing that does it on your behalf.
    //
    // These are one piece of work in two shapes: `check` answers now, `foreman`
    // keeps answering. Neither can accept anything on an agent's say-so, which
    // is the entire reason they exist.
    if matches!(
        args.first().map(String::as_str),
        Some("check") | Some("claim") | Some("foreman")
    ) {
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            false,
        );
        // A foreman is a supervisor, so it publishes what it decides when it
        // can. When another Ironsight already holds the stream it still does
        // the work — its verdicts go to the store either way — and says so
        // rather than pretending its events went anywhere.
        let publishing = if args[0] == "foreman" {
            app.with_stream().map_err(|e| anyhow::anyhow!(e))?
        } else {
            app.with_state();
            false
        };
        app.rescan_panes();

        if args[0] == "claim" {
            let who = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: ironsight claim <who>"))?;
            let id =
                resolve(&app, who).ok_or_else(|| anyhow::anyhow!("no session matching {who}"))?;
            let task = app
                .work
                .task_for(&id)
                .map(|t| t.id.clone())
                .ok_or_else(|| anyhow::anyhow!("{who} has not been assigned anything"))?;
            app.work
                .set_state(&task, work::State::Claimed)
                .map_err(|e| anyhow::anyhow!(e))?;
            app.work.flush();
            println!("{task} claimed — it is not done until the checks say so");
            return Ok(());
        }

        if args[0] == "check" {
            let who = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: ironsight check <who>"))?;
            let id =
                resolve(&app, who).ok_or_else(|| anyhow::anyhow!("no session matching {who}"))?;
            let outcomes = verify(&mut app, &id, false);
            match outcomes {
                Err(why) => anyhow::bail!(why),
                Ok(outcomes) => {
                    for o in &outcomes {
                        report(&o);
                    }
                    if checks::Suite::verified(&outcomes) {
                        println!("verified");
                    } else {
                        anyhow::bail!(
                            checks::Suite::refusal(&outcomes)
                                .unwrap_or_else(|| "not verified".into())
                        );
                    }
                }
            }
            return Ok(());
        }

        // The foreman.
        let every = args
            .iter()
            .position(|a| a == "--every")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| parse_since(s))
            .unwrap_or(Duration::from_secs(10));
        println!(
            "foreman watching · checking claimed work every {}s · {}",
            every.as_secs(),
            if publishing {
                "publishing to the stream"
            } else {
                "another Ironsight holds the stream, so verdicts go to the store only"
            }
        );
        loop {
            app.refresh();
            app.probe();
            app.work.reload_if_stale();
            let claimed: Vec<String> = app
                .work
                .tasks()
                .iter()
                .filter(|t| t.state == work::State::Claimed)
                .map(|t| t.session.clone())
                .collect();
            for id in claimed {
                match verify(&mut app, &id, true) {
                    Ok(outcomes) => {
                        for o in &outcomes {
                            report(&o);
                        }
                    }
                    // Nowhere to run the checks is not a verdict. Saying so and
                    // leaving the task alone is the only honest answer.
                    Err(why) => println!("cannot judge {}: {why}", &id[..id.len().min(8)]),
                }
            }
            std::thread::sleep(every);
        }
    }

    // Approving a project's checks, having read them.
    if args.first().map(String::as_str) == Some("trust") {
        let where_ = args.get(1).cloned().unwrap_or_else(|| ".".to_string());
        let dir = std::path::PathBuf::from(app::expand(&where_));
        let (root, suite) = checks::Suite::find(&dir)
            .map_err(|e| anyhow::anyhow!(e))?
            .ok_or_else(|| anyhow::anyhow!("{} has no {}", dir.display(), checks::FILE))?;
        println!("{} would run, in {}:", suite.checks.len(), root.display());
        for c in &suite.checks {
            println!("  {:<10} {}", c.name, c.run);
        }
        checks::trust(&root, &suite).map_err(|e| anyhow::anyhow!(e))?;
        println!("approved. If the file changes, it will ask again.");
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("refute") {
        let id = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: ironsight refute <task> <command>"))?;
        let command = args[2..].join(" ");
        if command.is_empty() {
            anyhow::bail!("usage: ironsight refute <task> <command that must fail>");
        }
        let mut store = work::Store::load(work::path_in(&app::data_dir()));
        store
            .refute_with(id, &command)
            .map_err(|e| anyhow::anyhow!(e))?;
        store.save()?;
        println!("{id} will be refused if this succeeds: {command}");
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("note") {
        let id = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: ironsight note <task> <text>"))?;
        let text = args[2..].join(" ");
        if text.is_empty() {
            anyhow::bail!("usage: ironsight note <task> <text>");
        }
        let mut store = work::Store::load(work::path_in(&app::data_dir()));
        store.note(id, &text).map_err(|e| anyhow::anyhow!(e))?;
        store.save()?;
        println!("noted on {id}");
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("send") {
        let who = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: ironsight send <who> <text>"))?;
        let text = args[2..].join(" ");
        if text.is_empty() {
            anyhow::bail!("usage: ironsight send <who> <text>");
        }
        // A tmux session or pane name works even before the session has written
        // a transcript — which is the case while it is still asking whether the
        // folder is trusted.
        let panes = control::panes();
        if let Some(p) = panes.iter().find(|p| p.session == *who || p.id == *who) {
            control::send_text(&p.id, &text).map_err(|e| anyhow::anyhow!(e))?;
            println!("sent to {} ({})", p.session, p.id);
            return Ok(());
        }

        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            true,
        );
        app.rescan_panes();
        let hit = app
            .sessions
            .iter()
            .find(|s| {
                let name = app
                    .steer
                    .get(&s.id)
                    .map(|p| p.session.clone())
                    .unwrap_or_default();
                s.id.starts_with(who.as_str())
                    || s.label().eq_ignore_ascii_case(who)
                    || name == *who
            })
            .map(|s| s.id.clone())
            .ok_or_else(|| anyhow::anyhow!("no live session matching {who}"))?;
        let pane = app
            .pane_of(&hit)
            .ok_or_else(|| {
                anyhow::anyhow!("{who} cannot be typed into — Ironsight has no terminal for it")
            })?
            .clone();
        control::send_text(&pane.id, &text).map_err(|e| anyhow::anyhow!(e))?;
        println!("sent to {} ({})", pane.session, pane.id);
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("new") {
        let path = args
            .get(1)
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .unwrap_or_else(|| ".".into());
        let opt = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        // --worktree runs the session in its own branch and checkout.
        let path = match opt("--worktree") {
            Some(branch) => {
                let repo = git::repo_root(std::path::Path::new(&path))
                    .ok_or_else(|| anyhow::anyhow!("{path} is not inside a git repository"))?;
                let dir = git::create_worktree(&repo, &branch).map_err(|e| anyhow::anyhow!(e))?;
                println!("worktree {} on branch {branch}", dir.display());
                dir.to_string_lossy().into_owned()
            }
            None => path,
        };
        let spec = app::NewSpec {
            path,
            agent: opt("--agent"),
            name: opt("--name"),
            model: opt("--model"),
            effort: opt("--effort"),
            mode: opt("--permission-mode"),
            prompt: opt("--prompt"),
        };
        // Starting a session is the same act however it is asked for, so the
        // command line goes through the engine rather than round it.
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(3600),
            true,
        );
        let assignment = opt("--task");
        let parent = opt("--parent");
        let name = app.start_session(&spec).map_err(|e| anyhow::anyhow!(e))?;
        // Lineage and assignment are recorded against the session Ironsight has
        // just started, which it knows by the pane it is running in until the
        // transcript catches up.
        if assignment.is_some() || parent.is_some() {
            app.with_state();
            app.rescan_panes();
            let id = app
                .sessions
                .iter()
                .find(|s| {
                    app.steer
                        .get(&s.id)
                        .map(|p| p.session == name)
                        .unwrap_or(false)
                })
                .map(|s| s.id.clone())
                .unwrap_or_else(|| format!("pane:{name}"));
            if let Some(parent) = parent {
                if let Some(pid) = resolve(&app, &parent) {
                    app.record_lineage(&id, &pid);
                    println!("started by {}", &pid[..pid.len().min(8)]);
                }
            }
            if let Some(what) = assignment {
                let task = app.assign(&id, &what);
                println!("{task} assigned");
            }
        }
        println!("started {name} — {}", control::attach_hint(&name));
        return Ok(());
    }

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("Ironsight {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--live" => only_live = true,
            "--cost" => show_cost = true,
            "--plain" => plain = true,
            "--no-mouse" => mouse = false,
            "--view" => {
                i += 1;
                view = match args.get(i).map(String::as_str) {
                    Some("feed") => View::Feed,
                    Some("files") => View::Files,
                    Some("stats") => View::Stats,
                    _ => anyhow::bail!("--view wants feed, files, or stats"),
                };
            }
            "--once" => once = true,
            "--since" => {
                i += 1;
                since = args
                    .get(i)
                    .and_then(|s| parse_since(s))
                    .ok_or_else(|| anyhow::anyhow!("--since wants a duration like 6h"))?;
            }
            "--root" => {
                i += 1;
                root = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("--root wants a path"))?,
                );
            }
            other => anyhow::bail!("unknown option {other}\n\n{USAGE}"),
        }
        i += 1;
    }

    ui::init_palette(plain);

    if !root.exists() {
        anyhow::bail!(
            "no transcripts at {}\nset CLAUDE_CONFIG_DIR, or point at them with --root <path>",
            root.display()
        );
    }

    let mut app = App::new(root, app::default_sessions_dir(), since, only_live);
    app.show_cost = show_cost;
    app.view = view;

    if once {
        print_once(&app);
        return Ok(());
    }

    // The live view publishes. A one-shot table does not: it would bind the
    // socket, print, and take the stream away again before anything could read
    // it, and it would fight the Ironsight you already have open.
    match app.with_stream() {
        Ok(true) => {}
        Ok(false) => app.say("another Ironsight is publishing the stream — this one is watching"),
        Err(e) => app.say(format!("the event stream is not available: {e}")),
    }

    // One key that always means "back to scope", held for as long as Ironsight is
    // here to come back to.
    let way_back = control::hold_way_back();
    // Before the terminal is put into a state that has to be undone.
    restore_terminal_however_this_ends();
    let mut term = ratatui::init();
    if mouse {
        let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    }
    let result = run(&mut term, &mut app);
    if mouse {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();
    control::drop_way_back(way_back);
    result
}

/// Everything a terminal must be told to undo, as one string of bytes.
///
/// Mouse reporting first, because that is the one that makes a terminal
/// unusable rather than merely untidy: with it left on, every movement of the
/// mouse is typed at whatever is reading, and there is no way to type over it.
/// Then the caret back on, and out of the alternate screen.
const RESTORE: &[u8] =
    b"\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l";

/// Put the terminal back, from anywhere, including places where almost nothing
/// is allowed to happen.
///
/// A signal handler may do very little safely — no allocation, no locks — so
/// this is one `write` of a constant, which is on the short list of things that
/// are. It is deliberately not `ratatui::restore()`, which allocates.
#[cfg(unix)]
extern "C" fn restore_on_signal(sig: i32) {
    unsafe {
        libc::write(1, RESTORE.as_ptr().cast(), RESTORE.len());
        libc::write(2, RESTORE.as_ptr().cast(), RESTORE.len());
        // Having tidied up, die the way we were asked to, so whoever sent the
        // signal sees the exit status they expected.
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Make sure the terminal is handed back however this process ends.
///
/// The ordinary path already does this. These are the other ones: a `pkill`, a
/// `systemctl stop`, a closed terminal, a panic. Without them the last thing
/// Ironsight does is leave mouse reporting on, and every mouse movement is then
/// typed into whatever shell comes next — which cannot be typed over, and looks
/// like a broken terminal rather than like a program that failed to clean up.
fn restore_terminal_however_this_ends() {
    #[cfg(unix)]
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
            libc::signal(sig, restore_on_signal as *const () as libc::sighandler_t);
        }
    }
    let existing = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(RESTORE);
        let _ = out.flush();
        ratatui::restore();
        existing(info);
    }));
}

/// The one-shot table.
///
/// Writes rather than prints: `Ironsight --once | head` closes the pipe halfway,
/// and a tool that panics when someone pipes it into `head` is a tool that
/// looks broken.
fn print_once(app: &App) {
    use std::io::Write;
    let out = std::io::stdout();
    let mut out = out.lock();
    macro_rules! line {
        ($($arg:tt)*) => {
            if writeln!(out, $($arg)*).is_err() {
                return;
            }
        };
    }

    let money = if app.show_cost { "EST$" } else { "REQS" };
    line!(
        "{:<12} {:<26} {:<26} {:>7} {:>7} {:>9} {:>6}",
        "STATUS",
        "SESSION",
        "WHERE",
        "CTX",
        "OUT",
        money,
        "LAST"
    );
    for s in &app.sessions {
        let status = match s.status() {
            Status::Running(t) => format!("run:{t}"),
            Status::Working => "working".into(),
            Status::Waiting => "waiting".into(),
            Status::Ended => "ended".into(),
        };
        line!(
            "{:<12} {:<26} {:<26} {:>7} {:>7} {:>9} {:>6}",
            truncate(&status, 12),
            truncate(&s.label(), 26),
            truncate(&s.where_(), 26),
            ui::fmt_tokens(s.totals.ctx),
            ui::fmt_tokens(s.totals.output),
            if app.show_cost {
                format!("${:.2}", s.totals.cost)
            } else {
                s.totals.requests.to_string()
            },
            ui::fmt_age(s.age_secs()),
        );
    }
    let (tokens, cost, working) = app.totals();
    let tail = if app.show_cost {
        format!(" · ~${cost:.2} if run on the API")
    } else {
        String::new()
    };
    line!(
        "\n{} sessions · {working} working · {} output tokens{tail}",
        app.sessions.len(),
        ui::fmt_tokens(tokens)
    );
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).chain(['…']).collect()
    }
}

/// The way out of passthrough, in every shape a terminal reports it.
///
/// ctrl+] is the classic escape, but a terminal without the kitty keyboard
/// protocol sends the raw control byte 0x1D, which crossterm reports as
/// ctrl+5 — so Ironsight watched for a key press that could never arrive and
/// passthrough became a one-way door. F12 is there as a way out that no
/// keyboard layout can withhold.
fn leaves_passthrough(code: KeyCode, ctrl: bool) -> bool {
    match code {
        KeyCode::F(12) => true,
        // The 0x1D byte itself, if a terminal ever passes it through unnamed.
        KeyCode::Char('\u{1d}') => true,
        // ']' under the kitty protocol, ctrl+5 from the 0x1D byte elsewhere.
        KeyCode::Char(']') | KeyCode::Char('5') => ctrl,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_way_a_terminal_reports_ctrl_bracket_gets_out() {
        // What a terminal without the kitty keyboard protocol actually sends:
        // the 0x1D byte, which crossterm calls ctrl+5. Watching only for ']'
        // left passthrough with no exit at all.
        assert!(leaves_passthrough(KeyCode::Char('5'), true));
        assert!(leaves_passthrough(KeyCode::Char(']'), true));
        assert!(leaves_passthrough(KeyCode::Char('\u{1d}'), false));
        assert!(leaves_passthrough(KeyCode::F(12), false));
    }

    #[test]
    fn ordinary_keys_still_reach_the_session() {
        assert!(!leaves_passthrough(KeyCode::Char('5'), false));
        assert!(!leaves_passthrough(KeyCode::Char(']'), false));
        assert!(!leaves_passthrough(KeyCode::Esc, false));
        assert!(!leaves_passthrough(KeyCode::Char('q'), false));
        assert!(!leaves_passthrough(KeyCode::Char('c'), true));
    }
}

fn run(term: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let tick = Duration::from_millis(250);
    let mut last_tick = Instant::now();
    loop {
        term.draw(|f| ui::draw(f, app))?;

        let timeout = tick.saturating_sub(last_tick.elapsed());
        if cevent::poll(timeout)? {
            let event = cevent::read()?;
            // Clicks and the wheel, so the thing on screen can just be pointed at.
            if let Event::Mouse(m) = event {
                let (col, row) = (m.column, m.row);
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(i) = app.regions.menu_at(col, row) {
                            let keys: Vec<char> = app.actions().iter().map(|a| a.key).collect();
                            if let Some(k) = keys.get(i) {
                                app.menu_sel = i;
                                app.run_action(*k);
                            }
                        } else if app.menu {
                            app.menu = false;
                        } else if let Some(i) = app.regions.session_at(col, row) {
                            if i < app.sessions.len() {
                                app.sel = i;
                            }
                        } else if let Some(i) = app.regions.right_at(col, row) {
                            // A second click on the same row opens it.
                            if app.point_right(i) {
                                app.popup = true;
                                app.popup_scroll = 0;
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if app.regions.over_list(col, row) {
                            app.select_session(1);
                        } else {
                            app.move_right(3);
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if app.regions.over_list(col, row) {
                            app.select_session(-1);
                        } else {
                            app.move_right(-3);
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c'))
                {
                    return Ok(());
                }
                // The conversation browser takes the keyboard while it is
                // open: typing filters it, so nothing else can claim letters.
                if app.past_open {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Esc => app.past_open = false,
                        KeyCode::Enter => app.resume_past(),
                        KeyCode::Down => app.move_past(1),
                        KeyCode::Up => app.move_past(-1),
                        KeyCode::Char('n') if ctrl => app.move_past(1),
                        KeyCode::Char('p') if ctrl => app.move_past(-1),
                        KeyCode::PageDown => app.move_past(10),
                        KeyCode::PageUp => app.move_past(-10),
                        KeyCode::Char('u') if ctrl => app.filter_past(String::clear),
                        KeyCode::Backspace => app.filter_past(|f| {
                            f.pop();
                        }),
                        KeyCode::Char(c) if !ctrl => app.filter_past(|f| f.push(c)),
                        _ => {}
                    }
                    continue;
                }
                if app.popup {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('v') => {
                            app.popup = false
                        }
                        KeyCode::Down | KeyCode::Char('j') => app.popup_scroll += 1,
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.popup_scroll = app.popup_scroll.saturating_sub(1)
                        }
                        KeyCode::PageDown => app.popup_scroll += 20,
                        KeyCode::PageUp => app.popup_scroll = app.popup_scroll.saturating_sub(20),
                        KeyCode::Home => app.popup_scroll = 0,
                        _ => {}
                    }
                    continue;
                }
                if app.passthrough {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    if leaves_passthrough(key.code, ctrl) {
                        app.toggle_passthrough();
                        continue;
                    }
                    app.forward_key(key.code, ctrl);
                    continue;
                }
                // ctrl+<digit> answers a prompt with that option
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let KeyCode::Char(c @ '1'..='9') = key.code {
                        app.answer(c as usize - '0' as usize);
                        continue;
                    }
                }
                if let Some(input) = app.input.as_mut() {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Esc => app.input = None,
                        KeyCode::Enter => app.submit_input(),
                        KeyCode::Backspace => input.backspace(),
                        KeyCode::Delete => input.delete(),
                        KeyCode::Left => input.left(),
                        KeyCode::Right => input.right(),
                        KeyCode::Home => input.home(),
                        KeyCode::End => input.end(),
                        KeyCode::Char('u') if ctrl => input.clear(),
                        KeyCode::Char('w') if ctrl => input.delete_word(),
                        KeyCode::Char('a') if ctrl => input.home(),
                        KeyCode::Char('e') if ctrl => input.end(),
                        KeyCode::Char(c) => input.insert(c),
                        _ => {}
                    }
                    continue;
                }
                if app.menu {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => app.menu = false,
                        KeyCode::Down | KeyCode::Char('j') => app.menu_sel += 1,
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.menu_sel = app.menu_sel.saturating_sub(1)
                        }
                        KeyCode::Enter => {
                            let key = app.actions().get(app.menu_sel).map(|a| a.key);
                            if let Some(k) = key {
                                app.run_action(k);
                            }
                        }
                        KeyCode::Char(c) => app.run_action(c),
                        _ => {}
                    }
                    continue;
                }
                if app.help {
                    app.help = false;
                    continue;
                }
                match key.code {
                    // Esc dismisses; it does not quit. An accidental Esc should
                    // never take the monitor down with it.
                    KeyCode::Char('q') => {
                        if app.may_quit() {
                            return Ok(());
                        }
                    }
                    KeyCode::Esc => {
                        if app.passthrough {
                            app.toggle_passthrough();
                        } else if !app.search.is_empty() {
                            app.search.clear();
                            app.hits.clear();
                            app.say("search cleared");
                        }
                    }
                    KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.move_session(1)
                    }
                    KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.move_session(-1)
                    }
                    KeyCode::Char('j') | KeyCode::Down => app.select_session(1),
                    KeyCode::Char('k') | KeyCode::Up => app.select_session(-1),
                    KeyCode::Tab => app.select_session(1),
                    KeyCode::BackTab => app.select_session(-1),
                    KeyCode::Char('J') => app.move_right(1),
                    KeyCode::Char('K') => app.move_right(-1),
                    KeyCode::PageDown => app.move_right(20),
                    KeyCode::PageUp => app.move_right(-20),
                    KeyCode::Char(c @ '0'..='9') => {
                        // 1-9 pick the first nine panes, 0 the tenth.
                        let i = if c == '0' {
                            9
                        } else {
                            c as usize - '1' as usize
                        };
                        if let Some(v) = app::VIEWS.get(i) {
                            app.view = *v;
                            app.list_sel = 0;
                            app.list_top_right = 0;
                        }
                    }
                    KeyCode::Char('w') => {
                        app.view = app.view.next();
                        app.list_sel = 0;
                        app.list_top_right = 0;
                    }
                    KeyCode::Char('$') => app.show_cost = !app.show_cost,
                    KeyCode::Char('g') | KeyCode::Home => {
                        app.follow = false;
                        app.feed_sel = 0;
                        app.feed_top = 0;
                    }
                    KeyCode::Char('G') | KeyCode::End => app.follow = true,
                    KeyCode::Enter | KeyCode::Char('.') => {
                        app.menu = true;
                        app.menu_sel = 0;
                    }
                    KeyCode::Char('v') | KeyCode::Char('o') => {
                        let has = match app.view {
                            View::Files => !app.file_keys().is_empty(),
                            _ => app.feed_len() > 0,
                        };
                        if has {
                            app.popup = true;
                            app.popup_scroll = 0;
                        }
                    }
                    KeyCode::Char('f') => {
                        app.filter = app.filter.next();
                        app.feed_top = 0;
                        app.follow = true;
                    }
                    KeyCode::Char('l') => {
                        app.only_live = !app.only_live;
                        app.discover();
                        app.refresh();
                    }
                    KeyCode::Char('r') => {
                        app.discover();
                        app.refresh();
                    }
                    KeyCode::Char('s') => app.run_action('s'),
                    KeyCode::Char('b') => {
                        if app.steer.is_empty() {
                            app.say("nothing Ironsight can steer is running");
                        } else {
                            app.open_input(Prompt::Broadcast);
                        }
                    }
                    KeyCode::Char('n') => app.run_action('n'),
                    KeyCode::Char('i') => app.run_action('i'),
                    KeyCode::Char('a') => app.run_action('a'),
                    KeyCode::Char('A') => app.run_action('A'),
                    KeyCode::Char('R') => app.run_action('R'),
                    KeyCode::F(2) => app.run_action('N'),
                    KeyCode::Char('x') => app.run_action('x'),
                    KeyCode::Char('y') => app.answer(1),
                    KeyCode::Char('d') => app.answer(0),

                    KeyCode::Char('p') => app.next_blocked(),
                    KeyCode::Char('Q') => app.run_action('Q'),
                    KeyCode::Char('/') => app.open_input(Prompt::Search),
                    KeyCode::Char(']') => app.cycle_hit(1),
                    KeyCode::Char('[') => app.cycle_hit(-1),
                    KeyCode::Char('m') => app.run_action('m'),
                    KeyCode::Char('L') => app.launch_fleet(),
                    KeyCode::Char('W') => app.run_action('W'),
                    KeyCode::Char('M') => app.run_action('M'),
                    KeyCode::Char('X') => app.run_action('X'),
                    KeyCode::Char('N') => {
                        app.notify_on = !app.notify_on;
                        let on = app.notify_on;
                        app.say(if on {
                            "notifications on"
                        } else {
                            "notifications off"
                        });
                    }
                    KeyCode::Char('?') => app.help = true,
                    _ => {}
                }
            }
        }

        if let Some(session) = app.attach_to.take() {
            if control::inside_tmux() {
                // Ironsight stays where it is; only the tmux client moves.
                match control::attach(&session) {
                    Ok(_) => app.say(format!("switched to {session} — F12 comes back")),
                    Err(e) => app.say(e),
                }
            } else {
                ratatui::restore();
                let outcome = control::attach(&session);
                *term = ratatui::init();
                term.clear()?;
                if let Err(e) = outcome {
                    app.say(e);
                }
            }
            app.discover();
            app.refresh();
            continue;
        }

        if last_tick.elapsed() >= tick {
            app.refresh();
            app.probe();
            if app.last_discover.elapsed() >= Duration::from_secs(3) {
                app.discover();
            }
            last_tick = Instant::now();
        }
    }
}
