//! Ironsight — watch every Claude Code session on this machine, live.

use ironsight_core::{app, bootstrap, control, git, session};

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
                               start a session and exit; --agent picks which
                               agent to run (claude, codex, gemini, aider, or
                               any command), default claude
       ironsight send <who> <text> type a line into a running session and submit it
       ironsight adopt <who>        (re)open a conversation in tmux so it can be steered
       ironsight prune              close Ironsight sessions whose process has exited
       ironsight doctor             check everything Ironsight needs is installed
       ironsight stop [who|--all]   stop one session, or everything Ironsight started
       ironsight waiting            list sessions blocked on a prompt
       ironsight approve <who> [n]  answer a blocked session (default option 1)

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
        if !control::OUTLIVES_SCOPE && ONE_SHOT.contains(&cmd.as_str()) {
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
        let name = app.start_session(&spec).map_err(|e| anyhow::anyhow!(e))?;
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

    // One key that always means "back to scope", held for as long as Ironsight is
    // here to come back to.
    let way_back = control::hold_way_back();
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
