//! Rendering. Nyfe palette: midnight ground, gold accent, everything else muted.

use crate::app::{App, Focus, View};
use crate::event::{Ev, Kind};
use crate::session::{Session, Status};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

pub const MIDNIGHT: Color = Color::Rgb(0x0B, 0x12, 0x20);
pub const GOLD: Color = Color::Rgb(0xC0, 0x85, 0x42);
const TEXT: Color = Color::Rgb(0xD6, 0xDC, 0xE8);
const BODY: Color = Color::Rgb(0xC2, 0xCA, 0xD9);
const MUTED: Color = Color::Rgb(0x64, 0x70, 0x84);
const DIM: Color = Color::Rgb(0x8A, 0x94, 0xA6);
const OK: Color = Color::Rgb(0x74, 0xA8, 0x7C);
const BAD: Color = Color::Rgb(0xC4, 0x5D, 0x4E);
const PANEL: Color = Color::Rgb(0x1A, 0x24, 0x36);

fn muted() -> Style {
    Style::new().fg(MUTED)
}

pub fn fmt_tokens(n: u64) -> String {
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

pub fn fmt_age(secs: i64) -> String {
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

fn fmt_ms(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

fn clip_to(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else if w <= 1 {
        String::new()
    } else {
        let mut out: String = s.chars().take(w - 1).collect();
        out.push('…');
        out
    }
}

/// Keep the tail of a long path — the end is the informative part.
fn clip_left(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        s.to_string()
    } else if w <= 1 {
        String::new()
    } else {
        let mut out = String::from("…");
        out.extend(s.chars().skip(n - (w - 1)));
        out
    }
}

/// Left spans, then right spans pushed to the far edge.
fn row(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let lw: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let rw: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let gap = width.saturating_sub(lw + rw);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

fn field(key: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<9}"), muted()),
        Span::styled(value, Style::new().fg(BODY)),
    ])
}

fn spark(vals: &[u64]) -> String {
    const CHARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = vals.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return " ".repeat(vals.len());
    }
    vals.iter()
        .map(|v| {
            let i = ((*v as f64 / max as f64) * 8.0).round() as usize;
            CHARS[i.min(8)]
        })
        .collect()
}

fn status_mark(s: &Session) -> (String, Color) {
    match s.status() {
        Status::Running(tool) => (format!("● {tool}"), GOLD),
        Status::Working => ("● working".into(), GOLD),
        Status::Waiting => ("○ waiting".into(), DIM),
        Status::Ended => ("· ended".into(), MUTED),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(MIDNIGHT)), area);
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
            .areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Length(42), Constraint::Min(20)]).areas(body);
    let [list, card] = Layout::vertical([Constraint::Min(6), Constraint::Length(9)]).areas(left);

    draw_header(f, app, header);
    draw_sessions(f, app, list);
    draw_card(f, app, card);
    match app.view {
        View::Feed => draw_feed(f, app, right),
        View::Files => draw_files(f, app, right),
        View::Stats => draw_stats(f, app, right),
    }
    draw_footer(f, app, footer);

    if app.popup {
        draw_popup(f, app, area);
    }
    if app.help {
        draw_help(f, area);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let (tokens, cost, working) = app.totals();
    let unpriced = app.sessions.iter().any(|s| s.totals.unpriced > 0);
    let left = vec![
        Span::styled("▌", Style::new().fg(GOLD)),
        Span::styled(" nyfe scope ", Style::new().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled(format!("· {} sessions · {working} working", app.sessions.len()), muted()),
    ];
    let mut right = vec![Span::styled(format!("{} out ", fmt_tokens(tokens)), Style::new().fg(DIM))];
    if app.show_cost {
        right.push(Span::styled(
            format!("~${cost:.2} if API{} ", if unpriced { "*" } else { "" }),
            Style::new().fg(GOLD),
        ));
    } else {
        right.push(Span::styled("subscription ", Style::new().fg(GOLD)));
    }
    f.render_widget(Paragraph::new(row(left, right, area.width as usize)), area);
}

fn draw_sessions(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sessions;
    let block = Block::bordered()
        .border_style(Style::new().fg(if focused { GOLD } else { PANEL }))
        .title(Span::styled(" sessions ", Style::new().fg(if focused { GOLD } else { DIM })));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = (inner.height as usize) / 2;
    if rows == 0 {
        return;
    }
    if app.sel < app.list_top {
        app.list_top = app.sel;
    } else if app.sel >= app.list_top + rows {
        app.list_top = app.sel + 1 - rows;
    }

    let w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (i, s) in app.sessions.iter().enumerate().skip(app.list_top).take(rows) {
        let selected = i == app.sel;
        let (mark, color) = status_mark(s);
        let dot = mark.chars().next().unwrap_or('·').to_string();
        let word: String = mark.chars().skip(2).collect();
        let age = fmt_age(s.age_secs());
        let label_style = if selected {
            Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(TEXT)
        };
        let label_w = w.saturating_sub(age.chars().count() + 5);
        lines.push(row(
            vec![
                Span::styled(if selected { "▌" } else { " " }, Style::new().fg(GOLD)),
                Span::styled(format!("{dot} "), Style::new().fg(color)),
                Span::styled(clip_to(&s.label(), label_w), label_style),
            ],
            vec![Span::styled(format!("{age} "), muted())],
            w,
        ));
        let right = format!("ctx {} ", fmt_tokens(s.totals.ctx));
        let room = w.saturating_sub(word.chars().count() + right.chars().count() + 6);
        lines.push(row(
            vec![
                Span::raw("   "),
                Span::styled(word, Style::new().fg(color)),
                Span::styled(format!(" · {}", clip_left(&s.where_(), room)), muted()),
            ],
            vec![Span::styled(right, muted())],
            w,
        ));
    }
    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(" no sessions in window", muted())));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_card(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_style(Style::new().fg(PANEL))
        .title(Span::styled(" session ", Style::new().fg(DIM)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(s) = app.current() else { return };

    let mut tools: Vec<(&String, &usize)> = s.tools.iter().collect();
    tools.sort_by(|a, b| b.1.cmp(a.1));
    let top: Vec<String> = tools.iter().take(3).map(|(n, c)| format!("{n} {c}")).collect();
    let t = &s.totals;
    let (added, removed) = s.lines_changed();
    let started = s
        .started
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "—".into());
    let client = match &s.live {
        Some(l) => format!("{} · pid {}", l.kind, l.pid),
        None => "closed".to_string(),
    };
    let money = if app.show_cost {
        format!("~${:.2} if API", t.cost)
    } else {
        format!("{} requests", t.requests)
    };
    let lines = vec![
        field("model", if s.model.is_empty() { "—".into() } else { s.model.clone() }),
        field("client", client),
        field("started", format!("{started} · {} turns", s.turns)),
        field("tools", if top.is_empty() { "—".into() } else { top.join(" · ") }),
        field("files", format!("{} touched · +{added}/-{removed}", s.files.len())),
        field("tokens", format!("out {} · ctx {}", fmt_tokens(t.output), fmt_tokens(t.ctx))),
        field("usage", money),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn pane_legend(name: &str) -> &'static str {
    match name {
        "files" => " files · E edit  W write  R read ",
        "feed" => " feed ",
        _ => " stats ",
    }
}

fn pane(app: &App, name: &str) -> (Block<'static>, bool) {
    let focused = app.focus == Focus::Feed;
    let title = match app.current() {
        Some(s) => format!(" {} · {} ", s.label(), if s.model.is_empty() { "—" } else { &s.model }),
        None => format!(" {name} "),
    };
    let block = Block::bordered()
        .border_style(Style::new().fg(if focused { GOLD } else { PANEL }))
        .title(Span::styled(title, Style::new().fg(if focused { GOLD } else { DIM })))
        .title_bottom(Span::styled(pane_legend(name), Style::new().fg(GOLD)));
    (block, focused)
}

fn kind_tag(ev: &Ev) -> (String, Color) {
    match ev.kind {
        Kind::Prompt => ("▸ you".into(), GOLD),
        Kind::Text => ("◆ claude".into(), TEXT),
        Kind::Thinking => ("· think".into(), MUTED),
        Kind::Tool => (format!("→ {}", ev.tool.clone().unwrap_or_default()), GOLD),
        Kind::Result => (
            format!("← {}", ev.tool.clone().unwrap_or_default()),
            if ev.ok { OK } else { BAD },
        ),
        Kind::System => ("⚙ sys".into(), if ev.ok { DIM } else { BAD }),
    }
}

fn draw_feed(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, focused) = pane(app, "feed");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let filtered = app.feed_indices();
    let h = inner.height as usize;
    if h == 0 {
        return;
    }
    if app.follow {
        app.feed_sel = filtered.len().saturating_sub(1);
    }
    if app.feed_sel < app.feed_top {
        app.feed_top = app.feed_sel;
    } else if app.feed_sel >= app.feed_top + h {
        app.feed_top = app.feed_sel + 1 - h;
    }
    if filtered.len() < app.feed_top + h {
        app.feed_top = filtered.len().saturating_sub(h);
    }

    let w = inner.width as usize;
    let (top, sel) = (app.feed_top, app.feed_sel);
    let mut lines: Vec<Line> = Vec::new();
    let Some(sess) = app.current() else { return };
    for (i, &idx) in filtered.iter().enumerate().skip(top).take(h) {
        let Some(ev) = sess.events.get(idx) else { continue };
        let selected = focused && i == sel;
        let time = ev
            .ts
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "--:--:--".into());
        let (tag, color) = kind_tag(ev);
        let tag = clip_to(&tag, 14);
        let pad = " ".repeat(14usize.saturating_sub(tag.chars().count()));
        let text = clip_to(&ev.head, w.saturating_sub(25));
        let body_style = if selected {
            Style::new().fg(TEXT).bg(PANEL)
        } else {
            Style::new().fg(if ev.kind == Kind::Thinking { MUTED } else { BODY })
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { "▌" } else { " " }, Style::new().fg(GOLD)),
            Span::styled(format!("{time} "), Style::new().fg(MUTED)),
            Span::styled(format!("{tag}{pad} "), Style::new().fg(color)),
            Span::styled(text, body_style),
        ]));
    }
    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(" nothing matches this filter", muted())));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_files(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, focused) = pane(app, "files");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let keys = app.file_keys();
    let h = inner.height as usize;
    if h == 0 {
        return;
    }
    if app.file_sel >= keys.len() {
        app.file_sel = keys.len().saturating_sub(1);
    }
    if app.file_sel < app.file_top {
        app.file_top = app.file_sel;
    } else if app.file_sel >= app.file_top + h {
        app.file_top = app.file_sel + 1 - h;
    }

    let w = inner.width as usize;
    let (top, sel) = (app.file_top, app.file_sel);
    let mut lines: Vec<Line> = Vec::new();
    let Some(sess) = app.current() else { return };
    for (i, key) in keys.iter().enumerate().skip(top).take(h) {
        let Some(t) = sess.files.get(key) else { continue };
        let selected = focused && i == sel;
        let mut ops = String::new();
        if t.edits > 0 {
            ops.push_str(&format!("E{} ", t.edits));
        }
        if t.writes > 0 {
            ops.push_str(&format!("W{} ", t.writes));
        }
        if t.reads > 0 {
            ops.push_str(&format!("R{} ", t.reads));
        }
        let churn = if t.added + t.removed > 0 {
            format!("+{}/-{}", t.added, t.removed)
        } else {
            String::new()
        };
        let age = t.last.map(|l| fmt_age((chrono::Utc::now() - l).num_seconds())).unwrap_or_default();
        let right = format!("{ops} {churn}  {age} ");
        let path = clip_left(
            &crate::event::short_path(key),
            w.saturating_sub(right.chars().count() + 3),
        );
        lines.push(row(
            vec![
                Span::styled(if selected { "▌" } else { " " }, Style::new().fg(GOLD)),
                Span::styled(
                    path,
                    if selected {
                        Style::new().fg(TEXT).bg(PANEL)
                    } else {
                        Style::new().fg(BODY)
                    },
                ),
            ],
            vec![
                Span::styled(format!("{ops}"), muted()),
                Span::styled(
                    format!("{churn}  "),
                    Style::new().fg(if t.added + t.removed > 0 { OK } else { MUTED }),
                ),
                Span::styled(format!("{age} "), muted()),
            ],
            w,
        ));
    }
    if keys.is_empty() {
        lines.push(Line::from(Span::styled(" no files touched yet", muted())));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn bar(n: usize, max: usize, width: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let cells = ((n as f64 / max as f64) * width as f64).round() as usize;
    "█".repeat(cells.max(1))
}

fn draw_stats(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, _) = pane(app, "stats");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(s) = app.current() else { return };
    let t = &s.totals;
    let w = inner.width as usize;

    let mut lines: Vec<Line> = Vec::new();
    let head = |lines: &mut Vec<Line>, title: &str| {
        lines.push(Line::from(Span::styled(
            format!(" {title}"),
            Style::new().fg(GOLD).add_modifier(Modifier::BOLD),
        )));
    };

    head(&mut lines, "usage");
    let models: Vec<String> = s.models.iter().map(|(m, c)| format!("{m} {c}")).collect();
    lines.push(field("requests", format!("{} · {}", t.requests, models.join(" · "))));
    lines.push(field(
        "client",
        match &s.live {
            Some(l) => format!("claude {} · {} · pid {}", s.version, l.kind, l.pid),
            None => format!("claude {} · session closed", s.version),
        },
    ));
    lines.push(field(
        "tokens",
        format!("in {} · out {}", fmt_tokens(t.input), fmt_tokens(t.output)),
    ));
    lines.push(field(
        "cache",
        format!("read {} · write {}", fmt_tokens(t.cache_read), fmt_tokens(t.cache_write)),
    ));
    let window = s.window();
    let pct = (t.ctx as f64 / window as f64 * 100.0).min(100.0);
    let filled = ((pct / 100.0) * 24.0).round() as usize;
    lines.push(Line::from(vec![
        Span::styled(format!("  {:<9}", "context"), muted()),
        Span::styled(
            format!("{} of {} ", fmt_tokens(t.ctx), fmt_tokens(window)),
            Style::new().fg(BODY),
        ),
        Span::styled("█".repeat(filled), Style::new().fg(if pct > 80.0 { BAD } else { GOLD })),
        Span::styled("░".repeat(24 - filled), Style::new().fg(PANEL)),
        Span::styled(format!(" {pct:.0}%"), muted()),
    ]));
    if app.show_cost {
        lines.push(field(
            "cost",
            format!("~${:.2} if this ran on the API", t.cost),
        ));
    } else {
        lines.push(field("plan", "subscription · press $ for API-equivalent cost".into()));
    }

    lines.push(Line::from(""));
    head(&mut lines, "work");
    let avg_turn = if s.turn_ms.is_empty() {
        0
    } else {
        s.turn_ms.iter().sum::<u64>() as i64 / s.turn_ms.len() as i64
    };
    let max_turn = s.turn_ms.iter().copied().max().unwrap_or(0) as i64;
    lines.push(field(
        "turns",
        format!("{} · avg {} · longest {}", s.turns, fmt_ms(avg_turn), fmt_ms(max_turn)),
    ));
    let (avg_lat, max_lat) = s.latency();
    let calls: usize = s.tools.values().sum();
    lines.push(field(
        "tools",
        format!("{calls} calls · avg {} · worst {}", fmt_ms(avg_lat), fmt_ms(max_lat)),
    ));
    let (added, removed) = s.lines_changed();
    lines.push(field(
        "files",
        format!("{} touched · +{added} / -{removed} lines", s.files.len()),
    ));
    lines.push(Line::from(vec![
        Span::styled(format!("  {:<9}", "errors"), muted()),
        Span::styled(
            s.errors.to_string(),
            Style::new().fg(if s.errors > 0 { BAD } else { BODY }),
        ),
    ]));

    lines.push(Line::from(""));
    head(&mut lines, "tool calls");
    let mut tools: Vec<(&String, &usize)> = s.tools.iter().collect();
    tools.sort_by(|a, b| b.1.cmp(a.1));
    let max = tools.first().map(|(_, c)| **c).unwrap_or(0);
    let bar_w = w.saturating_sub(26).min(40);
    for (name, count) in tools.iter().take(8) {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<12}", clip_to(name, 12)), Style::new().fg(BODY)),
            Span::styled(format!("{:>5} ", count), muted()),
            Span::styled(bar(**count, max, bar_w), Style::new().fg(GOLD)),
        ]));
    }

    lines.push(Line::from(""));
    head(&mut lines, "activity · last 60 minutes");
    let acts = s.activity(60);
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(spark(&acts), Style::new().fg(GOLD)),
    ]));
    lines.push(row(
        vec![Span::styled("  -60m", muted())],
        vec![Span::styled(format!("now{}", " ".repeat(w.saturating_sub(65).min(8))), muted())],
        62.min(w),
    ));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!(
                "{} events · peak {}/min",
                acts.iter().sum::<u64>(),
                acts.iter().max().unwrap_or(&0)
            ),
            muted(),
        ),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let keys = "  j/k session · J/K move · enter open · 1 feed 2 files 3 stats · f filter · $ cost · ? help";
    let state = format!(
        "{} · {} · {} ",
        app.view.label(),
        app.filter.label(),
        if app.follow { "following" } else { "paused" }
    );
    f.render_widget(
        Paragraph::new(row(
            vec![Span::styled(keys.to_string(), muted())],
            vec![Span::styled(state, Style::new().fg(if app.follow { GOLD } else { DIM }))],
            area.width as usize,
        )),
        area,
    );
}

fn centered(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let [_, mid, _] = Layout::vertical([
        Constraint::Percentage((100 - pct_y) / 2),
        Constraint::Percentage(pct_y),
        Constraint::Percentage((100 - pct_y) / 2),
    ])
    .areas(area);
    let [_, cell, _] = Layout::horizontal([
        Constraint::Percentage((100 - pct_x) / 2),
        Constraint::Percentage(pct_x),
        Constraint::Percentage((100 - pct_x) / 2),
    ])
    .areas(mid);
    cell
}

/// Colour a diff by line prefix; leave anything else alone.
fn body_lines(text: &str) -> Vec<Line<'static>> {
    text.lines()
        .map(|l| {
            let style = match l.chars().next() {
                Some('+') => Style::new().fg(OK),
                Some('-') => Style::new().fg(BAD),
                Some('@') if l.starts_with("@@") => Style::new().fg(GOLD),
                Some('─') => Style::new().fg(GOLD),
                _ => Style::new().fg(BODY),
            };
            Line::from(Span::styled(l.to_string(), style))
        })
        .collect()
}

fn draw_popup(f: &mut Frame, app: &App, area: Rect) {
    let (title, color, body) = match app.view {
        View::Files => match app.file_history() {
            Some((path, text)) => (crate::event::short_path(&path), GOLD, text),
            None => return,
        },
        _ => match app.event_at(app.feed_sel) {
            Some(ev) => {
                let (tag, color) = kind_tag(ev);
                (tag, color, ev.body.clone())
            }
            None => return,
        },
    };
    let rect = centered(area, 84, 74);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_style(Style::new().fg(GOLD))
        .style(Style::new().bg(MIDNIGHT))
        .title(Span::styled(format!(" {title} "), Style::new().fg(color)))
        .title_bottom(Span::styled(" j/k scroll · esc close ", muted()));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(
        Paragraph::new(body_lines(&body))
            .wrap(Wrap { trim: false })
            .scroll((app.popup_scroll, 0)),
        inner,
    );
}

fn draw_help(f: &mut Frame, area: Rect) {
    let rect = centered(area, 64, 70);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_style(Style::new().fg(GOLD))
        .style(Style::new().bg(MIDNIGHT))
        .title(Span::styled(" keys ", Style::new().fg(GOLD)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let rows = [
        ("j / k, ↓ ↑", "select session, or move in the focused pane"),
        ("J / K", "move in the right pane from anywhere"),
        ("g / G", "top / bottom (G resumes following)"),
        ("enter, v", "open the full text — command, output, diff"),
        ("1 2 3", "feed · files · stats"),
        ("w", "cycle the right pane"),
        ("f", "filter feed: all, tools, bash, files, talk"),
        ("$", "subscription view or API-equivalent cost"),
        ("l", "only sessions with a running process"),
        ("tab", "switch pane focus"),
        ("r", "rescan for new sessions"),
        ("q, esc", "quit"),
        ("", ""),
        ("cost", "an estimate of what these tokens would cost"),
        ("", "at API rates — a subscription is not billed"),
        ("", "this way. * marks an unknown model rate."),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<12}"), Style::new().fg(GOLD)),
                Span::styled((*d).to_string(), Style::new().fg(TEXT)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
