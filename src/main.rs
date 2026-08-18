//! nyfe scope — watch every Claude Code session on this machine, live.

mod app;
mod event;
mod pricing;
mod registry;
mod session;
mod tail;
mod ui;

use anyhow::Result;
use app::{App, Focus, View};
use crossterm::event::{self as cevent, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use session::Status;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const USAGE: &str = "\
nyfe scope — live view of what Claude Code is doing

usage: scope [options]

options:
  --since <dur>   include sessions touched within this window (default 24h)
                  accepts 90m, 12h, 7d, or plain seconds
  --live          only sessions with a running claude process
  --cost          show API-equivalent cost (default: subscription view)
  --view <name>   start on feed, files, or stats
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
    let mut root = app::default_root();

    let args: Vec<String> = std::env::args().skip(1).collect();
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

    if !root.exists() {
        anyhow::bail!("no transcripts at {} — is Claude Code installed here?", root.display());
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
                if app.help {
                    app.help = false;
                    continue;
                }
                let move_right = |app: &mut App, d: isize| match app.view {
                    View::Files => app.move_files(d),
                    _ => app.move_feed(d),
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
                    KeyCode::Char('1') => app.view = View::Feed,
                    KeyCode::Char('2') => app.view = View::Files,
                    KeyCode::Char('3') => app.view = View::Stats,
                    KeyCode::Char('w') => app.view = app.view.next(),
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
                    KeyCode::Char('?') => app.help = true,
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick {
            app.refresh();
            if app.last_discover.elapsed() >= Duration::from_secs(3) {
                app.discover();
            }
            last_tick = Instant::now();
        }
    }
}
