//! Sightline — the commands, and the session table.
//!
//! The interface is the window (`crates/gui`). What is here is everything that
//! is useful from a shell or a script: starting and steering sessions, tasks,
//! checks, briefs, ceilings, invariants, glue — and a one-shot table of what is
//! running, which is what a bare `sightline` prints.

use sightline_core::{
    app, bootstrap, brief, bus, checks, control, gateway, git, ladder, owned, session, work,
};

use anyhow::Result;
use app::App;
use crossterm::event::{self as cevent, Event, KeyCode, KeyEventKind, KeyModifiers};
use session::Status;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const USAGE: &str = "\
Sightline — commands for the Claude Code sessions on this machine

usage: sightline [options]     print what every session is doing, and exit
       sightline new [path] [--agent A] [--name N] [--model M] [--effort E]
                 [--permission-mode P] [--prompt T] [--worktree BRANCH]
                 [--task WHAT] [--parent WHO] [--owned]
                               start a session and exit; --agent picks which
                               agent to run (claude, codex, gemini, aider, or
                               any command), default claude. --owned starts one
                               Sightline holds itself, spoken to over structured
                               JSON with no terminal in the way
       sightline glue <version> [--remote NAME] [--dry-run]
                               reconcile this fork onto a newer upstream release:
                               teaches your agent upstream's architecture, seams
                               and invariants, then has it write the adapters in a
                               worktree of its own. --install just teaches it
       sightline owned              list the sessions Sightline is holding itself
       sightline key                read one keypress and say what arrived, for
                               working out why the way back does nothing
       sightline hidden [--ended] [--clear]
                               rows taken off the session list; --ended takes
                               every finished one off, --clear puts them all back
       sightline send <who> <text> send a line to a running session, whether it is
                               in a terminal or held by Sightline
       sightline adopt <who>        (re)open a conversation in tmux so it can be steered
       sightline prune              close Sightline sessions whose process has exited
       sightline doctor             check everything Sightline needs is installed
       sightline run [--model M] [--permission-mode P] <prompt>
                               run a session Sightline owns: structured JSON, no
                               terminal, no scraping. Streams what it does as it
                               happens and exits when the turn is done
       sightline serve              hold sessions in a process of Sightline's own,
                                so they outlive every window. Started for you
                                when it is needed; run it yourself to watch it
       sightline attach <who>       hand this terminal to a session Sightline holds
                                — the way out when the window is the problem
       sightline stop [who|--all]   stop one session, or everything Sightline started
       sightline waiting            list sessions blocked on a prompt
       sightline approve <who> [n]  answer a blocked session (default option 1)
       sightline events [--since N] [--json]
                               follow everything happening on this machine;
                               attaches to a running Sightline if there is one,
                               and watches the machine itself if there is not
       sightline tasks [--json]     what each session was asked to do
       sightline assign <who> <text>
                               give a session an assignment
       sightline note <task> <text> append what was learned to a task
       sightline brief <who> [--task <what>]
                               render a session's brief from the project's
                               constitution: the constraints that bear on the
                               task, what done means, and when to escalate
       sightline refute <task> <command>
                               name something that would show this work is
                               wrong. The command must fail; if it succeeds the
                               claim is refused. Without one, work can be
                               checked but never verified
       sightline claim <who>        say a session's work is finished; the checks decide
       sightline check <who>        run this project's checks now and report
       sightline invariants         try to break what must never stop being true
                               here. A quiet run is the good one
       sightline trust [path]       approve a project's checks, having read them.
                               Nothing runs from a .sightline/checks.toml until
                               you have, and it asks again if the file changes
       sightline foreman [--every N]
                               watch for claimed work and refuse what does not
                               pass its checks. Never writes code, never
                               restarts anything, never guesses

options:
  --since <dur>   include sessions touched within this window (default 24h)
                  accepts 90m, 12h, 7d, or plain seconds
  --live          only sessions with a running claude process
  --cost          show API-equivalent cost (default: subscription view)
  --root <path>   transcript root (default ~/.claude/projects)
  -h, --help      this text
  -V, --version   version

The window is the interface: `sightline-gui`, or the desktop entry. This binary
is the commands, plus the table above for a shell or a script.";

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
/// Run this project's checks against a session's work, and say what they showed.
///
/// The verdict itself is `ladder::adjudicate`, in core, because the worker that
/// did the work reaches it too — through the `claim` tool — and two
/// implementations of "finished" is two definitions of finished. What is here is
/// the part that genuinely belongs to a front end: printing, and publishing the
/// events core deliberately leaves to whoever holds the journal.
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

    if !record {
        // Asking what the checks say, without it counting as a claim.
        let (root, suite) = checks::Suite::find(&cwd)?.ok_or_else(|| {
            format!(
                "{} has no {} — a project has to say what finished means",
                cwd.display(),
                checks::FILE
            )
        })?;
        if !checks::trusted(&root, &suite) {
            return Err(checks::untrusted_hint(&root, &suite));
        }
        let mut env = std::collections::HashMap::new();
        if let Some(tree) = git::status(&cwd) {
            env.insert("BRANCH".to_string(), tree.branch);
        }
        return Ok(suite.run(&root, &env));
    }

    let mut store = std::mem::take(&mut app.work);
    let outcome = ladder::adjudicate(&mut store, id, &cwd);
    app.work = store;
    let report = outcome?;

    let short = &id[..id.len().min(8)];
    for line in &report.tried {
        println!("     refutation {line}");
    }
    let task = app
        .work
        .task_for(id)
        .map(|t| t.id.clone())
        .unwrap_or_else(|| short.to_string());
    println!("{task} · {short} · {}", report.reached.say());
    // Core reaches the verdict; only a front end journals it, because only one
    // process may hold the publisher lock.
    for ev in report.events {
        app.publish(ev);
    }
    Ok(report.outcomes)
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
        let mut last: Vec<Vec<sightline_core::screen::Run>> = Vec::new();
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
    let mut show_cost = false;
    let mut root = app::default_root();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // Where Sightline holds sessions itself, a subcommand would start or steer
    // something and then exit, taking it with it. Say so rather than doing it.
    const ONE_SHOT: [&str; 7] = [
        "new", "send", "adopt", "approve", "waiting", "stop", "prune",
    ];
    if let Some(cmd) = args.first() {
        if !control::outlives_sightline() && ONE_SHOT.contains(&cmd.as_str()) {
            anyhow::bail!(
                "scope holds sessions itself on this platform, so they end when it exits.\n\
                 `Sightline {cmd}` would do that immediately — run Sightline and use it from there."
            );
        }
    }

    if args.first().map(String::as_str) == Some("stop") {
        let who = args.get(1).map(String::as_str).unwrap_or("--all");
        if who == "--all" {
            let closed = control::stop_all();
            if closed.is_empty() {
                println!("nothing of Sightline's was running");
            } else {
                println!("stopped {}", closed.join(", "));
            }
            return Ok(());
        }
        // A session Sightline holds by pipe is stopped by name too — there is
        // no terminal to kill, so the terminal backend would never find it.
        if control::owned_all()
            .iter()
            .any(|o| o.name == who || o.session_id == who)
        {
            control::owned_stop(who).map_err(|e| anyhow::anyhow!(e))?;
            println!("stopped {who}");
            return Ok(());
        }
        let panes = control::panes();
        let target = panes
            .iter()
            .find(|p| p.session == who)
            .map(|p| p.session.clone())
            .ok_or_else(|| anyhow::anyhow!("no session called {who} — try sightline stop --all"))?;
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
        // Not a check — nothing is missing either way — but it is the question
        // people actually ask, and the answer has been guessable rather than
        // askable. `F12 → back to Sightline` is printed on a session's status
        // line; whether it is true depends on tmux, and tmux is where to look.
        println!("ok   {:<14} {}", "way back", control::way_back_state());
        if bootstrap::ready(&checks) {
            println!("\nready");
            return Ok(());
        }
        anyhow::bail!("something required is missing");
    }

    // The daemon. Nothing but the sessions and a socket: everything about what
    // a session *means* stays in the front ends, which read the same files they
    // always read.
    // A session Sightline owns, spoken to over the protocol rather than a
    // terminal. One-shot: send the prompt, stream what happens, exit when the
    // turn finishes. This is the seam the foreman and chief will drive.
    if args.first().map(String::as_str) == Some("run") {
        // `--model M` is a leading flag; everything from the first non-flag word
        // on is the prompt, verbatim. Scanning the whole argv for `--model`
        // would steal a word out of a prompt that merely mentions it — "explain
        // the --model flag" would lose "--model flag" silently.
        let mut model: Option<String> = None;
        let mut mode: Option<String> = None;
        let mut it = args[1..].iter().peekable();
        while let Some(a) = it.peek() {
            match a.as_str() {
                "--model" => {
                    it.next();
                    model = it.next().cloned();
                }
                // Nothing can be asked of a session in this mode: a tool the
                // settings do not allow is refused outright. So what it is
                // allowed to do is settled here or not at all.
                "--permission-mode" => {
                    it.next();
                    mode = it.next().cloned();
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
            anyhow::bail!("usage: sightline run [--model M] [--permission-mode P] <prompt>");
        }

        let program = control::claude_program();
        let cwd = std::env::current_dir()?;
        let session_id = format!("owned-{}", std::process::id());
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_reader = done.clone();

        let owned = owned::OwnedSession::start_with(
            &program,
            &cwd,
            &owned::Spec::default()
                .with_model(model.as_deref())
                .with_mode(mode.as_deref()),
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
        let path = sightline_core::daemon::default_path();
        if sightline_core::daemon::running() {
            anyhow::bail!("one is already listening on {}", path.display());
        }
        println!("holding sessions · {}", path.display());
        sightline_core::daemon::serve(path)?;
        return Ok(());
    }

    // The way back in when the window is the problem.
    //
    // tmux gave this for free — `tmux attach` and you are in the session. Held
    // by Sightline there has to be something that does the same, and it has to
    // exist before anyone relies on the daemon rather than after.
    if args.first().map(String::as_str) == Some("attach") {
        let who = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: sightline attach <who>"))?;
        let pane = control::panes()
            .into_iter()
            .find(|p| p.session == *who || p.id == *who)
            .ok_or_else(|| anyhow::anyhow!("no session called {who}"))?;
        return attach_to(&pane.id, &pane.session);
    }

    // A session that supervises the others: Sightline on its path, a brief, and a
    // ceiling it cannot raise. Not a new runtime — that is the whole point.
    if args.first().map(String::as_str) == Some("chief") {
        let opt = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let path = args
            .get(1)
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .unwrap_or_else(|| ".".into());
        let cwd = std::path::PathBuf::from(app::expand(&path));
        if !cwd.is_dir() {
            anyhow::bail!("{} is not a folder", cwd.display());
        }

        // Everything after the flags is what you want done, in your words. A
        // chief with no intent is a supervisor with nothing to supervise.
        let intent = match opt("--intent") {
            Some(text) => text,
            None => {
                let mut rest: Vec<String> = Vec::new();
                let mut it = args[1..].iter().peekable();
                if it.peek().map(|a| !a.starts_with("--")).unwrap_or(false) {
                    it.next();
                }
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "--model" | "--intent" => {
                            it.next();
                        }
                        "--" => {
                            rest.extend(it.by_ref().cloned());
                            break;
                        }
                        other if other.starts_with("--") => {}
                        other => {
                            rest.push(other.to_string());
                            rest.extend(it.by_ref().cloned());
                            break;
                        }
                    }
                }
                rest.join(" ")
            }
        };
        if intent.trim().is_empty() {
            anyhow::bail!(
                "usage: sightline chief [path] <what you want done>\n                 a chief needs to be told what is wanted, in your words"
            );
        }

        // Ceilings are not optional here, and this is the one place that says
        // so. Ordinary use does not need them; granting something else the
        // power to start sessions is exactly the case they exist for, and a
        // supervisor that could start a hundred workers because nobody had got
        // round to setting a number is not a design, it is a hope.
        let limits = sightline_core::limits::in_force(&cwd).map_err(|e| anyhow::anyhow!(e))?;
        if !limits.any() {
            anyhow::bail!(
                "a chief starts sessions on your behalf, so it does not start without a \
                 ceiling.\nSet one first:\n\n    sightline limits --sessions 6 --spend 20\n"
            );
        }

        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            true,
        );
        app.with_state();
        let constitution = brief::Constitution::find(&cwd).map(|(_, c)| c);
        let packet = sightline_core::chief::brief(
            &intent,
            &cwd.to_string_lossy(),
            constitution.as_ref(),
            &limits,
            &app.work,
        );

        if args.iter().any(|a| a == "--dry-run") {
            // What it would be told, without paying to tell it. The brief is
            // the whole of the chief, so being able to read it before starting
            // one is worth a flag.
            println!("{packet}");
            return Ok(());
        }

        let it = control::own(
            &cwd,
            &sightline_core::chief::spec(opt("--model").as_deref(), &packet, &cwd),
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        let id = if it.session_id.is_empty() {
            it.name.clone()
        } else {
            it.session_id.clone()
        };
        // The chief's own work is work, and is tracked like anyone else's.
        let task = app.assign(&id, &format!("supervise: {intent}"));
        println!("{} · {task} · {}", it.name, limits.describe());
        println!("talk to it with `sightline send {} <text>`", it.name);
        return Ok(());
    }

    // What must never stop being true here, tried rather than recited.
    if args.first().map(String::as_str) == Some("invariants") {
        let here = std::env::current_dir()?;
        let Some((root, suite)) = checks::Suite::find(&here).map_err(|e| anyhow::anyhow!(e))?
        else {
            anyhow::bail!("no {} anywhere above {}", checks::FILE, here.display());
        };
        if suite.invariants.is_empty() {
            println!("{} names no invariants.", checks::FILE);
            println!();
            println!("An invariant is the other direction from a check: a command that must");
            println!("FAIL, written to succeed only when a guarantee has stopped being true.");
            println!("A passing suite survives a change that quietly broke something");
            println!("load-bearing; a command looking for the breakage does not.");
            return Ok(());
        }
        // Same gate as the checks, and for the same reason: these are shell
        // that arrived with someone else's code.
        if !checks::trusted(&root, &suite) {
            anyhow::bail!(checks::untrusted_hint(&root, &suite));
        }
        let env = std::collections::HashMap::new();
        let held = suite.hold(&root, &env);
        let mut fired = 0;
        let mut unrunnable = 0;
        for h in &held {
            match &h.verdict {
                checks::Verdict::Stands => {
                    println!("ok    {:>6}ms  {}", h.ms, h.name);
                }
                checks::Verdict::Refuted { how } => {
                    fired += 1;
                    println!("BROKE {:>6}ms  {}", h.ms, h.name);
                    println!("               {}", h.must);
                    println!("               it fired: {how}");
                }
                checks::Verdict::Unrunnable { why } => {
                    unrunnable += 1;
                    println!("??    {:>6}ms  {} — could not be run · {why}", h.ms, h.name);
                }
            }
        }
        println!();
        if fired > 0 {
            anyhow::bail!(
                "{fired} of {} invariant(s) fired. Each one is a guarantee that has                  stopped being true.",
                held.len()
            );
        }
        // Said rather than folded into the good news: an invariant nobody can
        // test is an instrument that vouches for everything.
        if unrunnable > 0 {
            println!(
                "{} of {} could not be run, and have shown nothing either way.",
                unrunnable,
                held.len()
            );
        }
        println!("{} invariant(s) tried, none fired.", held.len());
        return Ok(());
    }

    // Reconcile a fork onto a newer upstream release, by teaching the fork's own
    // agent rather than by merging text.
    // `--glue <version>` as well as `glue <version>`: it reads as a flag on the
    // program more than as a subcommand, and being wrong about which one it was
    // should not cost anyone a round trip through the usage text.
    let args: Vec<String> = if args.first().map(String::as_str) == Some("--glue") {
        std::iter::once("glue".to_string())
            .chain(args[1..].iter().cloned())
            .collect()
    } else {
        args
    };

    if args.first().map(String::as_str) == Some("glue") {
        use sightline_core::glue;
        let opt = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let here = std::env::current_dir()?;
        let root = git::repo_root(&here).unwrap_or_else(|| here.clone());
        if git::repo_root(&here).is_none() {
            anyhow::bail!(
                "{} is not inside a git repository, and reconciling a fork needs one",
                here.display()
            );
        }

        // Teaching the fork is worth doing on its own: after this the fork's
        // own agent can be asked to reconcile without going through Sightline
        // at all, which is the point of shipping an ability rather than a tool.
        if args.iter().any(|a| a == "--install") {
            let path = glue::install(&root).map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "installed the {} ability at {}",
                glue::ABILITY_NAME,
                path.display()
            );
            println!("your agent can now be asked to reconcile this fork directly.");
            return Ok(());
        }

        let version = args
            .get(1)
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{}",
                    [
                        "usage: sightline glue <version> [--remote NAME] [--dry-run]",
                        "       sightline glue --install",
                    ]
                    .join("\n")
                )
            })?;

        // Which remote is upstream, said out loud rather than assumed: a fork
        // usually has both `upstream` and `origin` and they mean different
        // things.
        let remote = opt("--remote").or_else(|| glue::upstream_remote(&root));
        let Some(remote) = remote else {
            anyhow::bail!(
                "{}",
                [
                    "this repository has no remotes, so there is no upstream to reconcile onto.",
                    "Add one:  git remote add upstream <url>",
                ]
                .join("\n")
            );
        };
        println!("upstream is {remote}");

        // Resolve the version against what is actually here. Fetching is the
        // caller's business — this does not reach the network on its own.
        let candidates = [
            version.clone(),
            format!("{remote}/{version}"),
            format!("refs/tags/{version}"),
        ];
        let Some(upstream_ref) = candidates.iter().find(|r| glue::known(&root, r)).cloned() else {
            anyhow::bail!(
                "{}",
                [
                    format!("nothing here is called {version}. Fetch it first:"),
                    format!("    git fetch {remote} --tags"),
                    "then try again.".to_string(),
                ]
                .join("\n")
            );
        };

        // Reading the packet before paying to send it. Everything up to here is
        // the same work `reconcile` does, so this stops short of it rather than
        // duplicating what comes after.
        if args.iter().any(|a| a == "--dry-run") {
            let divergence =
                glue::divergence(&root, &upstream_ref).map_err(|e| anyhow::anyhow!(e))?;
            let checks_file = root.join(checks::FILE);
            let checks = checks_file.is_file().then_some(checks::FILE);
            println!(
                "\n{}",
                glue::brief(
                    &version,
                    &upstream_ref,
                    &root.to_string_lossy(),
                    &root
                        .join("..")
                        .join(format!("glue-{version}"))
                        .to_string_lossy(),
                    &divergence,
                    checks,
                    glue::dirty(&root),
                )
            );
            return Ok(());
        }

        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            true,
        );
        // The same engine function the Hub calls. It used to be written out
        // again here, which is exactly the duplication the one-engine rule is
        // for.
        let (name, worktree) = app
            .reconcile(&root, &version, Some(&remote), opt("--model").as_deref())
            .map_err(|e| anyhow::anyhow!(e))?;
        println!("{name} · {}", worktree.display());
        println!("watch it with `sightline`, talk to it with `sightline send {name} <text>`");
        println!("it works only in that worktree and will not merge — that is yours to do.");
        return Ok(());
    }

    // Does the key even arrive.
    //
    // The way back out of a full-screen session is a single key, and when it
    // does not work there are three different places it can be going wrong: the
    // desktop can take it, the terminal can take it, or tmux can be holding a
    // binding for something else. Nothing about the first two is visible from
    // inside Sightline — so rather than guess, read one keypress and say what
    // actually arrived.
    if args.first().map(String::as_str) == Some("key") {
        use crossterm::event::{self, Event, KeyCode, KeyEventKind};
        // A keypress needs somewhere for one to come from. Piped or redirected,
        // raw mode fails with an OS error that says nothing useful.
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            anyhow::bail!(
                "there is no terminal on stdin, so there is no key to read — \
                 run `sightline key` directly in a terminal"
            );
        }
        let want = control::way_back_key();
        println!("press {want} — or any key to see what it is. esc gives up.");
        println!("(if nothing happens at all, something above Sightline is taking it)");
        crossterm::terminal::enable_raw_mode()?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut saw: Option<String> = None;
        while Instant::now() < deadline {
            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            if let Event::Key(k) = event::read()? {
                // Terminals that speak the kitty protocol report a release as
                // well, and reporting both would read as two presses.
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if k.code == KeyCode::Esc {
                    break;
                }
                saw = Some(match k.code {
                    KeyCode::F(n) => format!("F{n}"),
                    KeyCode::Char(c) if k.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        format!("ctrl+{c}")
                    }
                    KeyCode::Char(c) => format!("{c}"),
                    other => format!("{other:?}"),
                });
                break;
            }
        }
        crossterm::terminal::disable_raw_mode()?;
        println!();
        match saw {
            Some(key) if key == want => {
                println!("{key} arrives here, so your terminal is not taking it.");
                println!("If it does nothing inside a session, the binding is the problem:");
                println!("  sightline doctor        says whether tmux is holding it");
            }
            Some(key) => {
                println!("that arrived as {key}, not {want}.");
                println!("Use it instead:  SIGHTLINE_WAY_BACK={key} sightline");
                println!("Put that in your shell profile to keep it.");
            }
            None => {
                println!("nothing arrived in twenty seconds.");
                println!("Whatever you pressed is being taken above Sightline — by the");
                println!("desktop, or by the terminal itself. Pick a key that gets");
                println!("through and name it:  SIGHTLINE_WAY_BACK=F9 sightline");
            }
        }
        return Ok(());
    }

    // Rows taken off the list, and the way back. A `hidden.json` nobody can
    // read from a terminal would make removing a row a thing you could not
    // undo without knowing where Sightline keeps its state.
    if args.first().map(String::as_str) == Some("hidden") {
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(30 * 86_400),
            false,
        );
        if args.iter().any(|a| a == "--ended") {
            // The clutter, cleared without opening the interface. Only finished
            // sessions: anything still running keeps its row, because a hidden
            // session that is working is an agent spending money out of sight.
            app.discover();
            app.refresh();
            let gone = app.hide_ended();
            println!(
                "{}",
                match gone {
                    0 => "nothing on the list has finished".to_string(),
                    n => format!(
                        "took {n} finished session(s) off the list — \
                         put them back with: sightline hidden --clear"
                    ),
                }
            );
            return Ok(());
        }
        if args.iter().any(|a| a == "--clear") {
            let back = app.unhide_all();
            println!(
                "{}",
                match back {
                    0 => "nothing was hidden".to_string(),
                    n => format!("put {n} session(s) back on the list"),
                }
            );
            return Ok(());
        }
        let n = app.hidden_count();
        if n == 0 {
            println!("nothing is hidden — every conversation Sightline knows about is listed");
        } else {
            println!("{n} session(s) taken off the list. They are not deleted: the");
            println!("transcripts are where they always were, and `sightline` R still");
            println!("finds them. Put them all back with: sightline hidden --clear");
        }
        return Ok(());
    }

    // The ceilings on what a fleet may do, and the one command that sets them.
    //
    // They are written here rather than edited by hand because the file lives
    // outside every worktree on purpose, and a person who has to go looking for
    // it will not set one.
    // The light behind the glass. Here as well as in the window because both
    // front ends are meant to reach the same engine, and because setting it
    // from a shell is how somebody scripts a machine's appearance.
    // The shape of one supervised project. Here as well as in the window
    // because both front ends read the same engine, and because a diagram is
    // worth having in a terminal too — as an indented tree, which is what a
    // terminal's version of a flow chart is.
    if args.first().map(String::as_str) == Some("mission") {
        let who = args.get(1).cloned().unwrap_or_default();
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(30 * 86_400),
            false,
        );
        // Both, and in this order. `discover` walks transcripts and is what
        // makes a name resolvable; `with_state` loads the work store, which is
        // where the assignments are. Without the second the chart was built
        // against an empty store and reported a chief with nothing assigned —
        // which is a real state, and so it looked like an answer rather than a
        // mistake.
        app.discover();
        app.with_state();
        let id = app
            .sessions
            .iter()
            .find(|s| s.title == who || s.id == who || s.id.starts_with(&who))
            .map(|s| s.id.clone())
            .unwrap_or(who.clone());
        let chart = app.mission(&id);
        if chart.nodes.is_empty() {
            anyhow::bail!("{who} has no work of its own on record");
        }
        if args.iter().any(|a| a == "--json") {
            println!("{}", serde_json::to_string_pretty(&chart)?);
            return Ok(());
        }
        // The first line only. `intent` is the whole paragraph a person
        // handed over, which in a terminal is a wall in front of the tree it
        // is supposed to introduce.
        println!("{}\n", chart.intent.lines().next().unwrap_or("").trim());
        for node in &chart.nodes {
            let indent = "  ".repeat(node.depth);
            let what = node.assignment.replace("supervise:", "").trim().to_string();
            let what: String = what.chars().take(64).collect();
            // A subagent is marked, because it is not the same kind of thing
            // as a worker: not counted against the ceiling, not confined by a
            // policy of its own, and gone when its parent's turn ends.
            let name: String = if node.inner {
                format!("· {}", node.name)
            } else if node.name.is_empty() {
                node.session.chars().take(14).collect()
            } else {
                node.name.clone()
            };
            println!("{indent}{name:<18} {:<10} {what}", node.state);
        }
        println!("\n{} finished · {} open", chart.done, chart.open);
        return Ok(());
    }

    // Which agents are on this machine, and what each one still needs.
    //
    // The question somebody has on their first day and nowhere to ask it: they
    // have cloned this, and the README tells them what is possible rather than
    // what is missing on their own machine.
    if args.first().map(String::as_str) == Some("connections") {
        let deep = !args.iter().any(|a| a == "--quick");
        let all = sightline_core::agent::connections(deep);
        if args.iter().any(|a| a == "--json") {
            println!("{}", serde_json::to_string_pretty(&all)?);
            return Ok(());
        }
        for c in &all {
            let state = if !c.installed {
                "not installed".to_string()
            } else {
                match c.signed_in {
                    Some(true) => format!("ready · {}", c.version),
                    Some(false) => "installed, not signed in".into(),
                    None => format!("installed · {}", c.version),
                }
            };
            println!(
                "{:<8} {:<26} {}",
                c.id,
                state,
                if c.governed {
                    "governed — the kernels apply"
                } else {
                    "not governed — watched and driven only"
                }
            );
            if !c.installed && !c.install_hint.is_empty() {
                println!("         install: {}", c.install_hint);
            } else if c.signed_in == Some(false) && !c.signin_hint.is_empty() {
                println!("         sign in: {}", c.signin_hint);
            }
        }
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("backdrop") {
        use sightline_core::backdrop::{self, Choice};
        match args.get(1).map(String::as_str) {
            None => {
                println!(
                    "{}",
                    match backdrop::load() {
                        Choice::Bloom => "bloom".to_string(),
                        Choice::None => "none".to_string(),
                        Choice::Image(p) => p.display().to_string(),
                    }
                );
            }
            Some(choice) => {
                let choice = match choice {
                    "bloom" => Choice::Bloom,
                    "none" => Choice::None,
                    path => Choice::Image(std::path::PathBuf::from(app::expand(path))),
                };
                backdrop::save(&choice).map_err(|e| anyhow::anyhow!(e))?;
                println!("backdrop set — the window picks it up when it next opens");
            }
        }
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("limits") {
        let opt = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let asked = [opt("--sessions"), opt("--spend"), opt("--window")];
        if asked.iter().any(Option::is_some) || args.iter().any(|a| a == "--none") {
            let path = sightline_core::limits::machine_path();
            let mut limits = sightline_core::limits::read(&path)
                .map_err(|e| anyhow::anyhow!(e))?
                .unwrap_or_default();
            if args.iter().any(|a| a == "--none") {
                limits = Default::default();
            }
            if let Some(v) = &asked[0] {
                limits.sessions =
                    Some(v.parse().map_err(|_| {
                        anyhow::anyhow!("--sessions wants a whole number, not {v}")
                    })?);
            }
            if let Some(v) = &asked[1] {
                limits.spend = Some(
                    v.trim_start_matches('$')
                        .parse()
                        .map_err(|_| anyhow::anyhow!("--spend wants an amount, not {v}"))?,
                );
            }
            if let Some(v) = &asked[2] {
                limits.window = Some(
                    v.trim_end_matches('h')
                        .parse()
                        .map_err(|_| anyhow::anyhow!("--window wants hours, not {v}"))?,
                );
            }
            let mut text = [
                "# What a fleet on this machine may do.",
                "#",
                "# Here rather than in a repository on purpose: a ceiling a supervised",
                "# agent can edit is not a ceiling. A project may lower these in",
                "# .sightline/limits.toml, and may never raise them.",
                "",
                "",
            ]
            .join("\n");
            if let Some(n) = limits.sessions {
                text.push_str(&format!("sessions = {n}\n"));
            }
            if let Some(d) = limits.spend {
                text.push_str(&format!("spend    = {d}\n"));
            }
            if let Some(h) = limits.window {
                text.push_str(&format!("window   = {h}\n"));
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, text)?;
            println!("{} · {}", path.display(), limits.describe());
            return Ok(());
        }

        let cwd = std::env::current_dir()?;
        let machine = sightline_core::limits::read(&sightline_core::limits::machine_path())
            .map_err(|e| anyhow::anyhow!(e))?;
        let project = match sightline_core::limits::project_path(&cwd) {
            Some(path) => (
                Some(path.clone()),
                sightline_core::limits::read(&path).map_err(|e| anyhow::anyhow!(e))?,
            ),
            None => (None, None),
        };
        let in_force = sightline_core::limits::effective(machine, project.1);
        println!(
            "machine  {} · {}",
            sightline_core::limits::machine_path().display(),
            machine.map(|m| m.describe()).unwrap_or("none set".into())
        );
        if let (Some(path), Some(p)) = (&project.0, project.1) {
            println!("project  {} · {}", path.display(), p.describe());
        }
        println!("in force {}", in_force.describe());
        if !in_force.any() {
            println!("\nset one with: sightline limits --sessions 8 --spend 25");
            return Ok(());
        }
        // What the ceilings are actually being measured against, because a
        // ceiling you cannot see the other side of is not much use — and
        // picking a number without knowing the current one is guessing.
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            true,
        );
        app.discover();
        app.refresh();
        let running = app.running_sessions();
        println!(
            "running  {running} session(s) of Sightline's own — your own sessions do not count"
        );
        let journal = app::data_dir().join("events.jsonl");
        let hours = in_force.window_hours();
        let spent = sightline_core::limits::spent_since(&journal, hours);
        println!("spent    ${spent:.2} in the last {hours}h, by the event journal");
        if in_force.spend.is_some() && !journal.exists() {
            // Said plainly rather than left to be discovered: spend is counted
            // from what was written down, and nothing writes it down unless an
            // Sightline is running. A spend ceiling on a machine that only ever
            // runs the commands is a ceiling nothing is measured against.
            println!("\nnothing has been journalled yet ({}).", journal.display());
            println!("spend is counted from that file, and it is written while an Sightline");
            println!("window or terminal view is running — a spend ceiling measures nothing");
            println!("until one has been.");
        }
        return Ok(());
    }

    // What Sightline is holding by pipe rather than by terminal: the handle, the
    // conversation it is in, and whether it is mid-turn.
    if args.first().map(String::as_str) == Some("owned") {
        let all = control::owned_all();
        if all.is_empty() {
            println!("Sightline is holding no sessions of its own");
            return Ok(());
        }
        for o in all {
            let state = match (o.alive, o.busy) {
                (false, _) => "ended".to_string(),
                (true, true) if !o.tool.is_empty() => format!("working · {}", o.tool),
                (true, true) => "working".to_string(),
                (true, false) => "waiting".to_string(),
            };
            let conversation = if o.session_id.is_empty() {
                "(not yet named)".to_string()
            } else {
                o.session_id.clone()
            };
            // The permission mode is worth a column: it is fixed for the life
            // of the session and it decides every tool call, so a session
            // getting nothing done is usually this column's fault.
            let mode = if o.mode.is_empty() {
                "default".to_string()
            } else {
                o.mode.clone()
            };
            println!(
                "{:<10} {:<10} {:<14} {conversation}  {}",
                o.name, state, mode, o.cwd
            );
        }
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("prune") {
        let mut closed = control::prune();
        // The ones Sightline holds are tidied by the same word. Only the dead:
        // nothing running is touched, which is what a person means by prune.
        closed.extend(control::owned_reap());
        if closed.is_empty() {
            println!("nothing to tidy up — everything Sightline started is still running");
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
                .ok_or_else(|| anyhow::anyhow!("usage: sightline adopt <who>"))?;
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
            .ok_or_else(|| anyhow::anyhow!("usage: sightline approve <who> [n]"))?;
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

    // The stream, for anything that is not Sightline.
    //
    // If an Sightline is running its socket is the source, so several consumers
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
                // The Sightline that owned it exited between the check and the
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
            // Another Sightline took the socket between the check above and the
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
        let json = args.iter().any(|a| a == "--json");
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
            if json {
                println!("[]");
            } else {
                println!("nothing has been assigned — try: sightline assign <who> <what>");
            }
            return Ok(());
        }
        let rolled = app.rolled_up();
        let store = &app.work;
        // Ordered by the shape of the work, so a supervisor's workers sit under
        // it rather than beside it.
        let sessions: Vec<String> = store.tasks().iter().map(|t| t.session.clone()).collect();

        // One object per task, for a chief or any tool that acts on fleet state
        // rather than reads it. The same tree, the same rolled-up cost, as data.
        if json {
            let items: Vec<serde_json::Value> = store
                .ordered(&sessions)
                .into_iter()
                .filter_map(|(session, depth)| {
                    let task = store
                        .task_for(&session)
                        .or_else(|| store.tasks().iter().rev().find(|t| t.session == session))?;
                    let cost = rolled.get(&session).copied().unwrap_or_default();
                    Some(serde_json::json!({
                        "id": task.id,
                        "session": session,
                        "depth": depth,
                        "parent": task.parent,
                        "state": task.state.label(),
                        "assignment": task.assignment,
                        "constraints": task.constraints,
                        "success": task.success,
                        "escalate_if": task.escalate_if,
                        "notes": task.notes.iter().map(|n| &n.text).collect::<Vec<_>>(),
                        "rolled_output": cost.output,
                        "rolled_cost": cost.estimate,
                    }))
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&items)?);
            return Ok(());
        }

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
            .ok_or_else(|| anyhow::anyhow!("usage: sightline assign <who> <what>"))?;
        let what = args[2..].join(" ");
        if what.is_empty() {
            anyhow::bail!("usage: sightline assign <who> <what>");
        }
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            false,
        );
        // Only the store: an Sightline may well be running, and this command has
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
        // can. When another Sightline already holds the stream it still does
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
                .ok_or_else(|| anyhow::anyhow!("usage: sightline claim <who>"))?;
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
                .ok_or_else(|| anyhow::anyhow!("usage: sightline check <who>"))?;
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
                "another Sightline holds the stream, so verdicts go to the store only"
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
        let total = suite.checks.len() + suite.invariants.len();
        println!("{total} would run, in {}:", root.display());
        for c in &suite.checks {
            println!("  check      {:<38} {}", c.name, c.run);
        }
        // Shown separately, because they run the other way round and someone
        // approving them should know that: these are commands expected to fail.
        for i in &suite.invariants {
            println!("  invariant  {:<38} {}", i.name, i.refute);
        }
        checks::trust(&root, &suite).map_err(|e| anyhow::anyhow!(e))?;
        println!("approved. If the file changes, it will ask again.");
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("refute") {
        let id = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: sightline refute <task> <command>"))?;
        let command = args[2..].join(" ");
        if command.is_empty() {
            anyhow::bail!("usage: sightline refute <task> <command that must fail>");
        }
        let mut store = work::Store::load(work::path_in(&app::data_dir()));
        store
            .refute_with(id, &command)
            .map_err(|e| anyhow::anyhow!(e))?;
        store.save()?;
        println!("{id} will be refused if this succeeds: {command}");
        return Ok(());
    }

    // Render the brief for a session's task: the constraints that bear on it,
    // what success looks like, and when to escalate — assembled from the
    // project's constitution and the task's own record, nothing else.
    if args.first().map(String::as_str) == Some("brief") {
        let who = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: sightline brief <who> [--task <what>]"))?;
        let adhoc = args
            .iter()
            .position(|a| a == "--task")
            .and_then(|i| args.get(i + 1..))
            .map(|rest| {
                rest.iter()
                    .take_while(|a| !a.starts_with("--"))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|s| !s.is_empty());

        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            false,
        );
        app.with_state();
        app.rescan_panes();

        // A real session is resolved for its cwd and its assigned task. But a
        // `--task` given against a path (or `.`) is a preview from that
        // directory's constitution, so a brief can be read before any session
        // exists — which is how a person checks what a worker would be told.
        let session = resolve(&app, who);
        let (cwd, task) = match (&session, &adhoc) {
            (Some(id), _) => {
                let cwd = app
                    .sessions
                    .iter()
                    .find(|s| s.id == *id)
                    .map(|s| s.cwd.clone())
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| ".".into());
                let task = match &adhoc {
                    Some(what) => work::Task::new("adhoc".into(), id.clone(), what.clone()),
                    None => app.work.task_for(id).cloned().ok_or_else(|| {
                        anyhow::anyhow!("{who} has no assignment — give one with --task")
                    })?,
                };
                (cwd, task)
            }
            (None, Some(what)) => {
                // `who` was a path to preview from, not a session.
                let cwd = app::expand(who);
                (
                    cwd,
                    work::Task::new("preview".into(), who.clone(), what.clone()),
                )
            }
            (None, None) => anyhow::bail!("no session matching {who} — give --task to preview"),
        };
        let constitution = brief::Constitution::find(std::path::Path::new(&cwd)).map(|(_, c)| c);
        print!("{}", brief::render(constitution.as_ref(), &task));
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("note") {
        let id = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: sightline note <task> <text>"))?;
        let text = args[2..].join(" ");
        if text.is_empty() {
            anyhow::bail!("usage: sightline note <task> <text>");
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
            .ok_or_else(|| anyhow::anyhow!("usage: sightline send <who> <text>"))?;
        let text = args[2..].join(" ");
        if text.is_empty() {
            anyhow::bail!("usage: sightline send <who> <text>");
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
        app.rescan_owned();
        let hit = app
            .sessions
            .iter()
            .find(|s| {
                let name = app
                    .steer
                    .get(&s.id)
                    .map(|p| p.session.clone())
                    .unwrap_or_default();
                let held = app
                    .owned_of(&s.id)
                    .map(|o| o.name.clone())
                    .unwrap_or_default();
                s.id.starts_with(who.as_str())
                    || s.label().eq_ignore_ascii_case(who)
                    || name == *who
                    || held == *who
            })
            .map(|s| s.id.clone())
            .ok_or_else(|| anyhow::anyhow!("no live session matching {who}"))?;
        // One place decides what sending means, so a message typed into a
        // terminal and one written down a pipe cannot come to mean different
        // things.
        app.send_to(&hit, &text).map_err(|e| anyhow::anyhow!(e))?;
        println!("{}", app.note);
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
        // Whether there is room for another session at all, asked before a
        // worktree is cut for it. A ceiling that refuses after the checkout
        // exists leaves a branch and a directory behind for a session that
        // never started — which is exactly what a supervisor then finds and has
        // to work out.
        {
            let where_ = std::path::PathBuf::from(app::expand(&path));
            let probe = App::new(
                app::default_root(),
                app::default_sessions_dir(),
                Duration::from_secs(3600),
                true,
            );
            if let Some(refused) = probe.ceiling_refusal(&where_) {
                anyhow::bail!(refused);
            }
        }

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
            owned: args.iter().any(|a| a == "--owned"),
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

        // A session Sightline holds itself, spoken to down a pipe rather than
        // typed into a terminal. Started here rather than under a command of
        // its own because it is a session: the same folder, model, task,
        // lineage and brief apply, and only the way in is different.
        if spec.owned {
            if spec.agent.as_deref().is_some_and(|a| a != "claude") {
                anyhow::bail!("--owned needs Claude Code: it is the agent that speaks stream-json");
            }
            app.with_state();
            // The opening message, worked out before the session exists,
            // because an owned agent says nothing until it is spoken to and its
            // first message is the one that names the conversation.
            let cwd = std::path::PathBuf::from(app::expand(&spec.path));
            let opening = match (&assignment, &spec.prompt) {
                (Some(what), _) => {
                    let constitution = brief::Constitution::find(&cwd).map(|(_, c)| c);
                    let task = sightline_core::work::Task::new(
                        "pending".into(),
                        String::new(),
                        what.clone(),
                    );
                    Some(brief::render(constitution.as_ref(), &task))
                }
                (None, Some(p)) => Some(p.clone()),
                (None, None) => None,
            };
            let id = app
                .start_owned(&spec, opening.as_deref())
                .map_err(|e| anyhow::anyhow!(e))?;
            if let Some(parent) = parent {
                if let Some(pid) = resolve(&app, &parent) {
                    app.record_lineage(&id, &pid);
                    println!("started by {}", &pid[..pid.len().min(8)]);
                }
            }
            if let Some(what) = assignment {
                let task_id = app.assign(&id, &what);
                println!("{task_id} assigned");
                if opening.is_some() {
                    println!("briefed from {}", brief::FILE);
                }
            }
            let held = app
                .owned_of(&id)
                .map(|o| o.name.clone())
                .unwrap_or_else(|| id.clone());
            println!(
                "started {held} — held by Sightline, talk to it with `sightline send {held} <text>`"
            );
            return Ok(());
        }

        let name = app.start_session(&spec).map_err(|e| anyhow::anyhow!(e))?;
        // Lineage and assignment are recorded against the session Sightline has
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
                let task_id = app.assign(&id, &what);
                println!("{task_id} assigned");
                // Brief the session from the project's constitution: the
                // constraints that bear on this task and what done means here,
                // delivered as its opening message rather than left implicit.
                let cwd = std::path::PathBuf::from(&spec.path);
                let constitution = brief::Constitution::find(&cwd).map(|(_, c)| c);
                if let Some(task) = app.work.get(&task_id) {
                    let packet = brief::render(constitution.as_ref(), task);
                    match app.send_to(&id, &packet) {
                        Ok(()) => println!("briefed from {}", brief::FILE),
                        // The session may not be steerable yet; the brief still
                        // stands as the task record, so this is not fatal.
                        Err(_) => {}
                    }
                }
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
                println!("Sightline {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--live" => only_live = true,
            "--cost" => show_cost = true,
            // The table is all there is now, so this only ever meant "yes".
            "--once" => {}
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

    if !root.exists() {
        anyhow::bail!(
            "no transcripts at {}\nset CLAUDE_CONFIG_DIR, or point at them with --root <path>",
            root.display()
        );
    }

    let mut app = App::new(root, app::default_sessions_dir(), since, only_live);
    app.show_cost = show_cost;
    print_once(&app);
    Ok(())
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
/// Sightline does is leave mouse reporting on, and every mouse movement is then
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
        existing(info);
    }));
}

/// Token counts the way a person reads them, and how long ago something last
/// happened. Both used to live in the terminal view; the table is what is left
/// of it.
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_age(secs: i64) -> String {
    if secs == i64::MAX {
        return "-".into();
    }
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// The one-shot table.
///
/// Writes rather than prints: `sightline | head` closes the pipe halfway,
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
            fmt_tokens(s.totals.ctx),
            fmt_tokens(s.totals.output),
            if app.show_cost {
                format!("${:.2}", s.totals.cost)
            } else {
                s.totals.requests.to_string()
            },
            fmt_age(s.age_secs()),
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
        fmt_tokens(tokens)
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
/// ctrl+5 — so Sightline watched for a key press that could never arrive and
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
