//! nyfe scope — watch every Claude Code session on this machine, live.

mod app;
mod control;
mod git;
mod notify;
mod event;
mod pricing;
mod registry;
mod session;
mod tail;
mod ui;

use anyhow::Result;
use app::{App, Focus, Prompt, View};
use crossterm::event::{self as cevent, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use session::Status;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const USAGE: &str = "\
nyfe scope — live view of what Claude Code is doing

usage: scope [options]
       scope new [path] [--model M] [--effort E] [--permission-mode P] [--prompt T]
                               start a Claude Code session in tmux and exit
       scope send <who> <text> type a line into a running session and submit it
       scope waiting            list sessions blocked on a prompt
       scope approve <who> [n]  answer a blocked session (default option 1)

options:
  --since <dur>   include sessions touched within this window (default 24h)
                  accepts 90m, 12h, 7d, or plain seconds
  --live          only sessions with a running claude process
  --cost          show API-equivalent cost (default: subscription view)
  --view <name>   start on feed, files, or stats
  --plain         no colour (also honours NO_COLOR)
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
    num.trim().parse::<u64>().ok().map(|n| Duration::from_secs(n * mult))
}

fn main() -> Result<()> {
    let mut since = Duration::from_secs(24 * 3_600);
    let mut only_live = false;
    let mut once = false;
    let mut show_cost = false;
    let mut view = View::Feed;
    let mut plain = std::env::var_os("NO_COLOR").is_some();
    let mut root = app::default_root();

    let args: Vec<String> = std::env::args().skip(1).collect();

    if matches!(args.first().map(String::as_str), Some("waiting") | Some("approve")) {
        let mut app = App::new(
            app::default_root(),
            app::default_sessions_dir(),
            Duration::from_secs(7 * 86_400),
            true,
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
        let who = args.get(1).ok_or_else(|| anyhow::anyhow!("usage: scope approve <who> [n]"))?;
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
        let who = args.get(1).ok_or_else(|| anyhow::anyhow!("usage: scope send <who> <text>"))?;
        let text = args[2..].join(" ");
        if text.is_empty() {
            anyhow::bail!("usage: scope send <who> <text>");
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
                let name = app.steer.get(&s.id).map(|p| p.session.clone()).unwrap_or_default();
                s.id.starts_with(who.as_str())
                    || s.label().eq_ignore_ascii_case(who)
                    || name == *who
            })
            .map(|s| s.id.clone())
            .ok_or_else(|| anyhow::anyhow!("no live session matching {who}"))?;
        let pane = app
            .pane_of(&hit)
            .ok_or_else(|| anyhow::anyhow!("{who} is not running in tmux, so it cannot be typed into"))?
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
        let name = control::new_session_with(
            std::path::Path::new(&path),
            opt("--model").as_deref(),
            opt("--effort").as_deref(),
            opt("--permission-mode").as_deref(),
            opt("--prompt").as_deref(),
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        println!("started tmux session {name} — attach with: tmux attach -t {name}");
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
                println!("nyfe scope {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--live" => only_live = true,
            "--cost" => show_cost = true,
            "--plain" => plain = true,
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
                    args.get(i).ok_or_else(|| anyhow::anyhow!("--root wants a path"))?,
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

    let mut term = ratatui::init();
    let result = run(&mut term, &mut app);
    ratatui::restore();
    result
}

fn print_once(app: &App) {
    let money = if app.show_cost { "EST$" } else { "REQS" };
    println!(
        "{:<12} {:<26} {:<26} {:>7} {:>7} {:>9} {:>6}",
        "STATUS", "SESSION", "WHERE", "CTX", "OUT", money, "LAST"
    );
    for s in &app.sessions {
        let status = match s.status() {
            Status::Running(t) => format!("run:{t}"),
            Status::Working => "working".into(),
            Status::Waiting => "waiting".into(),
            Status::Ended => "ended".into(),
        };
        println!(
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
    println!(
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

fn run(term: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let tick = Duration::from_millis(250);
    let mut last_tick = Instant::now();
    loop {
        term.draw(|f| ui::draw(f, app))?;

        let timeout = tick.saturating_sub(last_tick.elapsed());
        if cevent::poll(timeout)? {
            if let Event::Key(key) = cevent::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c'))
                {
                    return Ok(());
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
                    if ctrl && key.code == KeyCode::Char(']') {
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
                    match key.code {
                        KeyCode::Esc => app.input = None,
                        KeyCode::Enter => app.submit_input(),
                        KeyCode::Backspace => {
                            input.buf.pop();
                        }
                        KeyCode::Char(c) => input.buf.push(c),
                        _ => {}
                    }
                    continue;
                }
                if app.help {
                    app.help = false;
                    continue;
                }
                let move_right = |app: &mut App, d: isize| match app.view {
                    View::Files => app.move_files(d),
                    View::Feed => app.move_feed(d),
                    View::Plan | View::Stats | View::Mirror => {}
                    // the simple list views share one cursor
                    _ => {
                        let next = (app.list_sel as isize + d).max(0);
                        app.list_sel = next as usize;
                    }
                };
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Tab => {
                        app.focus = match app.focus {
                            Focus::Sessions => Focus::Feed,
                            Focus::Feed => Focus::Sessions,
                        }
                    }
                    KeyCode::Char('j') | KeyCode::Down => match app.focus {
                        Focus::Sessions => app.select_session(1),
                        Focus::Feed => move_right(app, 1),
                    },
                    KeyCode::Char('k') | KeyCode::Up => match app.focus {
                        Focus::Sessions => app.select_session(-1),
                        Focus::Feed => move_right(app, -1),
                    },
                    KeyCode::Char('J') => {
                        app.focus = Focus::Feed;
                        move_right(app, 1);
                    }
                    KeyCode::Char('K') => {
                        app.focus = Focus::Feed;
                        move_right(app, -1);
                    }
                    KeyCode::PageDown => move_right(app, 20),
                    KeyCode::PageUp => move_right(app, -20),
                    KeyCode::Char(c @ '1'..='9') => {
                        let i = c as usize - '1' as usize;
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
                        app.focus = Focus::Feed;
                        app.follow = false;
                        app.feed_sel = 0;
                        app.feed_top = 0;
                    }
                    KeyCode::Char('G') | KeyCode::End => app.follow = true,
                    KeyCode::Enter | KeyCode::Char('v') => {
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
                    KeyCode::Char('s') => app.open_input(Prompt::Send),
                    KeyCode::Char('b') => {
                        if app.steer.is_empty() {
                            app.say("no sessions are running in tmux");
                        } else {
                            app.open_input(Prompt::Broadcast);
                        }
                    }
                    KeyCode::Char('n') => app.open_input(Prompt::NewSession),
                    KeyCode::Char('i') => app.interrupt(),
                    KeyCode::Char('a') => app.attach(),
                    KeyCode::Char('A') => app.adopt(),
                    KeyCode::Char('y') => app.answer(1),
                    KeyCode::Char('d') => app.answer(0),
                    KeyCode::Char('p') => app.next_blocked(),
                    KeyCode::Char('Q') => app.open_input(Prompt::Queue),
                    KeyCode::Char('/') => app.open_input(Prompt::Search),
                    KeyCode::Char(']') => app.cycle_hit(1),
                    KeyCode::Char('[') => app.cycle_hit(-1),
                    KeyCode::Char('m') => app.toggle_passthrough(),
                    KeyCode::Char('L') => app.launch_fleet(),
                    KeyCode::Char('N') => {
                        app.notify_on = !app.notify_on;
                        let on = app.notify_on;
                        app.say(if on { "notifications on" } else { "notifications off" });
                    }
                    KeyCode::Char('?') => app.help = true,
                    _ => {}
                }
            }
        }

        if let Some(session) = app.attach_to.take() {
            ratatui::restore();
            let outcome = control::attach(&session);
            *term = ratatui::init();
            term.clear()?;
            if let Err(e) = outcome {
                app.say(e);
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
