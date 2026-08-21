//! Rendering. Nyfe palette: midnight ground, gold accent, everything else muted.

use ironsight_core::app::{App, View};
use ironsight_core::event::{Ev, Kind};
use ironsight_core::session::{Session, Status};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use std::sync::OnceLock;

/// Colours resolve once at startup. A terminal that asks for no colour (the
/// NO_COLOR convention, or --plain) gets the same layout with nothing but the
/// terminal's own foreground, so the tool is readable anywhere.
pub struct Palette {
    pub midnight: Color,
    pub gold: Color,
    pub text: Color,
    pub body: Color,
    pub muted: Color,
    pub dim: Color,
    pub ok: Color,
    pub bad: Color,
    pub panel: Color,
}

static PALETTE: OnceLock<Palette> = OnceLock::new();

pub fn init_palette(plain: bool) {
    let p = if plain {
        Palette {
            midnight: Color::Reset,
            gold: Color::Reset,
            text: Color::Reset,
            body: Color::Reset,
            muted: Color::DarkGray,
            dim: Color::Gray,
            ok: Color::Reset,
            bad: Color::Reset,
            panel: Color::DarkGray,
        }
    } else {
        Palette {
            midnight: Color::Rgb(0x0B, 0x12, 0x20),
            gold: Color::Rgb(0xC0, 0x85, 0x42),
            text: Color::Rgb(0xD6, 0xDC, 0xE8),
            body: Color::Rgb(0xC2, 0xCA, 0xD9),
            muted: Color::Rgb(0x64, 0x70, 0x84),
            dim: Color::Rgb(0x8A, 0x94, 0xA6),
            ok: Color::Rgb(0x74, 0xA8, 0x7C),
            bad: Color::Rgb(0xC4, 0x5D, 0x4E),
            panel: Color::Rgb(0x1A, 0x24, 0x36),
        }
    };
    let _ = PALETTE.set(p);
}

fn pal() -> &'static Palette {
    PALETTE.get_or_init(|| {
        init_palette(false);
        // set() above filled it; this arm only runs if init was never called.
        Palette {
            midnight: Color::Rgb(0x0B, 0x12, 0x20),
            gold: Color::Rgb(0xC0, 0x85, 0x42),
            text: Color::Rgb(0xD6, 0xDC, 0xE8),
            body: Color::Rgb(0xC2, 0xCA, 0xD9),
            muted: Color::Rgb(0x64, 0x70, 0x84),
            dim: Color::Rgb(0x8A, 0x94, 0xA6),
            ok: Color::Rgb(0x74, 0xA8, 0x7C),
            bad: Color::Rgb(0xC4, 0x5D, 0x4E),
            panel: Color::Rgb(0x1A, 0x24, 0x36),
        }
    })
}

fn muted() -> Style {
    Style::new().fg(pal().muted)
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
        Span::styled(value, Style::new().fg(pal().body)),
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
        Status::Running(tool) => (format!("● {tool}"), pal().gold),
        Status::Working => ("● working".into(), pal().gold),
        Status::Waiting => ("○ waiting".into(), pal().dim),
        Status::Ended => ("· ended".into(), pal().muted),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(pal().midnight)), area);
    let blocked = app.approval().is_some();
    let [header, body, strip, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(if blocked { 5 } else { 0 }),
        Constraint::Length(1),
    ])
    .areas(area);
    // The session list takes a share of the window rather than a fixed number of
    // columns: 42 of them is half of a laptop terminal and a sliver of a large
    // display. The bounds are what a session row needs to be readable at all,
    // and the point past which more width only buys whitespace.
    let list_width = (body.width / 4)
        .clamp(30, 56)
        .min(body.width.saturating_sub(24));
    let [left, right] =
        Layout::horizontal([Constraint::Length(list_width), Constraint::Min(20)]).areas(body);
    // The detail card gives up rows on a short window so the session list keeps
    // enough to be useful, and takes more when there is room to spare.
    let card_rows = match body.height {
        h if h >= 48 => 13,
        h if h >= 26 => 10,
        _ => 6,
    };
    let [list, card] =
        Layout::vertical([Constraint::Min(4), Constraint::Length(card_rows)]).areas(left);

    draw_header(f, app, header);
    draw_sessions(f, app, list);
    draw_card(f, app, card);
    match app.view {
        View::Feed => draw_feed(f, app, right),
        View::Files => draw_files(f, app, right),
        View::Stats => draw_stats(f, app, right),
        View::Plan => draw_plan(f, app, right),
        View::Agents => draw_agents(f, app, right),
        View::Mirror => draw_mirror(f, app, right),
        View::Tree => draw_tree(f, app, right),
        View::Errors => draw_errors(f, app, right),
        View::Fleet => draw_fleet(f, app, right),
        View::Read => draw_read(f, app, right),
    }
    if blocked {
        draw_approval(f, app, strip);
    }
    draw_footer(f, app, footer);

    app.regions.menu = None;
    if app.menu {
        draw_menu(f, app, area);
    }
    if app.popup {
        draw_popup(f, app, area);
    }
    if app.help {
        draw_help(f, area);
    }
    if app.past_open {
        draw_past(f, app, area);
    }
}

/// Every conversation on the machine, to pick one and bring it back. This is
/// the other half of the session list: that one answers "what is running", this
/// one answers "what have I ever talked to Claude Code about".
fn draw_past(f: &mut Frame, app: &mut App, area: Rect) {
    let hits = app.past_hits();
    // Sized to what there is to show, up to most of the window: a short history
    // in a full-height box reads as something failing to load.
    let width = (area.width * 86 / 100).max(40).min(area.width);
    let wanted = hits.len() as u16 + 5;
    let height = wanted.clamp(8, area.height.saturating_sub(2).max(8));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_style(Style::new().fg(pal().gold))
        .style(Style::new().bg(pal().midnight))
        .title(Span::styled(
            format!(
                " resume a conversation · {} of {} ",
                hits.len(),
                app.past.len()
            ),
            Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " type to filter · ↑ ↓ move · enter resumes · esc closes ",
            muted(),
        ));
    let inner = block.inner(rect);

    // Keep the cursor on screen as it moves through a long history.
    let rows = inner.height.saturating_sub(2) as usize;
    let top = app.past_top.min(app.past_sel);
    let top = if app.past_sel >= top + rows {
        app.past_sel + 1 - rows
    } else {
        top
    };
    let width = inner.width as usize;

    let mut lines: Vec<Line> = Vec::new();
    for (i, p) in hits.iter().enumerate().skip(top).take(rows) {
        let selected = i == app.past_sel;
        let open = app
            .sessions
            .iter()
            .any(|s| s.id == p.id && s.live.is_some());
        let age = format!("{:>4}", fmt_age(p.age_secs()));
        let where_ = ironsight_core::event::short_path(&p.cwd);
        // Size stands in for how much was said: a two-line question and a
        // fortnight of work look very different in the list.
        let size = format!("{:>6}", fmt_tokens(p.bytes / 4));
        // The title earns whatever the age and folder do not need.
        let room = width.saturating_sub(age.len() + where_.chars().count() + size.len() + 7);
        let title = ironsight_core::event::clip(&p.label(), room.max(12));
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▌" } else { " " },
                Style::new().fg(pal().gold),
            ),
            Span::styled(format!("{age} "), muted()),
            Span::styled(if open { "● " } else { "  " }, Style::new().fg(pal().ok)),
            Span::styled(
                format!("{title:<room$}", room = room.max(12)),
                if selected {
                    Style::new().fg(pal().text).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(pal().body)
                },
            ),
            Span::styled(format!(" {where_}"), muted()),
            Span::styled(format!(" {size}"), Style::new().fg(pal().dim)),
        ]));
    }
    if hits.is_empty() {
        lines.push(Line::from(Span::styled("  nothing matches that", muted())));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  filter ", Style::new().fg(pal().gold)),
        Span::styled(app.past_filter.clone(), Style::new().fg(pal().text)),
        Span::styled("▏", Style::new().fg(pal().gold)),
    ]));
    app.past_top = top;
    f.render_widget(block, rect);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let (tokens, cost, working) = app.totals();
    let unpriced = app.sessions.iter().any(|s| s.totals.unpriced > 0);
    let left = vec![
        Span::styled("▌", Style::new().fg(pal().gold)),
        Span::styled(
            " Ironsight ",
            Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("· {} sessions · {working} working", app.sessions.len()),
            muted(),
        ),
    ];
    let mut right = Vec::new();
    let waiting = app.approvals.len();
    if waiting > 0 {
        right.push(Span::styled(
            format!(" {waiting} need you · "),
            Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
        ));
    }
    right.push(Span::styled(
        format!("{} out ", fmt_tokens(tokens)),
        Style::new().fg(pal().dim),
    ));
    if app.show_cost {
        right.push(Span::styled(
            format!("~${cost:.2} if API{} ", if unpriced { "*" } else { "" }),
            Style::new().fg(pal().gold),
        ));
    } else {
        right.push(Span::styled("subscription ", Style::new().fg(pal().gold)));
    }
    f.render_widget(Paragraph::new(row(left, right, area.width as usize)), area);
}

fn draw_sessions(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = true;
    let block = Block::bordered()
        .border_style(Style::new().fg(if focused { pal().gold } else { pal().panel }))
        .title(Span::styled(
            {
                let rows = (area.height.saturating_sub(2) as usize) / 2;
                let shown = rows.min(app.sessions.len());
                if shown < app.sessions.len() {
                    format!(" sessions · {shown} of {} ", app.sessions.len())
                } else {
                    " sessions ".to_string()
                }
            },
            Style::new().fg(if focused { pal().gold } else { pal().dim }),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.list = (inner.x, inner.y, inner.width, inner.height);

    let rows = (inner.height as usize) / 2;
    if rows == 0 {
        return;
    }
    if app.sel < app.list_top {
        app.list_top = app.sel;
    } else if app.sel >= app.list_top + rows {
        app.list_top = app.sel + 1 - rows;
    }
    app.regions.list_top = app.list_top;

    let w = inner.width as usize;
    let steerable: Vec<bool> = app
        .sessions
        .iter()
        .map(|s| app.steer.contains_key(&s.id))
        .collect();
    let mut lines: Vec<Line> = Vec::new();
    for (i, s) in app
        .sessions
        .iter()
        .enumerate()
        .skip(app.list_top)
        .take(rows)
    {
        let selected = i == app.sel;
        let (mark, color) = status_mark(s);
        let dot = mark.chars().next().unwrap_or('·').to_string();
        let word: String = mark.chars().skip(2).collect();
        let age = if s.placeholder {
            "new".to_string()
        } else {
            fmt_age(s.age_secs())
        };
        let label_style = if selected {
            Style::new().fg(pal().text).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(pal().text)
        };
        let steer = if app.approvals.contains_key(&s.id) {
            "! "
        } else if *steerable.get(i).unwrap_or(&false) {
            "» "
        } else {
            ""
        };
        let label_w = w.saturating_sub(age.chars().count() + steer.chars().count() + 5);
        lines.push(row(
            vec![
                Span::styled(
                    if selected { "▌" } else { " " },
                    Style::new().fg(pal().gold),
                ),
                Span::styled(format!("{dot} "), Style::new().fg(color)),
                Span::styled(clip_to(&s.label(), label_w), label_style),
            ],
            vec![
                Span::styled(
                    steer.to_string(),
                    Style::new().fg(pal().gold).add_modifier(if steer == "! " {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
                Span::styled(format!("{age} "), muted()),
            ],
            w,
        ));
        let right = format!("ctx {} ", fmt_tokens(s.totals.ctx));
        // Assemble, then clip to what is left after the right-hand column, so a
        // long path can never run into it.
        let avail = w.saturating_sub(right.chars().count() + 4);
        let path_room = avail.saturating_sub(word.chars().count() + 3);
        let path = clip_left(&s.where_(), path_room);
        lines.push(row(
            vec![
                Span::raw("   "),
                Span::styled(word.clone(), Style::new().fg(color)),
                Span::styled(
                    clip_to(
                        &format!(" · {path}"),
                        avail.saturating_sub(word.chars().count()),
                    ),
                    muted(),
                ),
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
        .border_style(Style::new().fg(pal().panel))
        .title(Span::styled(" session ", Style::new().fg(pal().dim)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(s) = app.current() else { return };

    let mut tools: Vec<(&String, &usize)> = s.tools.iter().collect();
    tools.sort_by(|a, b| b.1.cmp(a.1));
    let top: Vec<String> = tools
        .iter()
        .take(3)
        .map(|(n, c)| format!("{n} {c}"))
        .collect();
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
    let control = if app.steer.contains_key(&s.id) {
        match app.steer.get(&s.id) {
            Some(p) => ironsight_core::control::where_hint(&p.session),
            None => "steerable".into(),
        }
    } else if s.live.is_some() {
        "watch only · A adopts it".into()
    } else if s.placeholder {
        "ended".into()
    } else {
        "ended · A reopens it".into()
    };
    let money = if app.show_cost {
        format!("~${:.2} if API", t.cost)
    } else {
        format!("{} requests", t.requests)
    };
    // Ordered so that the lines that answer "can I act on this?" survive when
    // the card is short.
    let lines = vec![
        field(
            "model",
            if s.model.is_empty() {
                "—".into()
            } else {
                s.model.clone()
            },
        ),
        field("control", control.clone()),
        field("started", format!("{started} · {} turns", s.turns)),
        field(
            "tools",
            if top.is_empty() {
                "—".into()
            } else {
                top.join(" · ")
            },
        ),
        field(
            "files",
            format!("{} touched · +{added}/-{removed}", s.files.len()),
        ),
        field(
            "tokens",
            format!("out {} · ctx {}", fmt_tokens(t.output), fmt_tokens(t.ctx)),
        ),
        field("usage", money),
        field("client", client),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn pane_legend(name: &str) -> String {
    match name {
        "files" => " files · E edit  W write  R read ".into(),
        "plan" => " plan · from the session's todo list ".into(),
        "agents" => " agents · subagents this session launched ".into(),
        "mirror" => " mirror · what the session's terminal shows ".into(),
        "tree" => " tree · working directory as it stands ".into(),
        "errors" => " errors · failed tools and API errors ".into(),
        "fleet" => " fleet · every session on one timeline ".into(),
        "read" => " read · the conversation, without the machinery ".into(),
        other => format!(" {other} "),
    }
}

/// The right-hand pane. Its cursor is always drawn — there is no focus to
/// lose, since j/k always move sessions and J/K always move this pane.
fn pane(app: &App, name: &str) -> (Block<'static>, bool) {
    let title = match app.current() {
        Some(s) => format!(
            " {} · {} ",
            s.label(),
            if s.model.is_empty() { "—" } else { &s.model }
        ),
        None => format!(" {name} "),
    };
    let block = Block::bordered()
        .border_style(Style::new().fg(pal().panel))
        .title(Span::styled(title, Style::new().fg(pal().dim)))
        .title_bottom(Span::styled(pane_legend(name), Style::new().fg(pal().gold)));
    (block, true)
}

fn kind_tag(ev: &Ev) -> (String, Color) {
    match ev.kind {
        Kind::Prompt => ("▸ you".into(), pal().gold),
        Kind::Text => ("◆ claude".into(), pal().text),
        Kind::Thinking => ("· think".into(), pal().muted),
        Kind::Tool => (
            format!("→ {}", ev.tool.clone().unwrap_or_default()),
            pal().gold,
        ),
        Kind::Result => (
            format!("← {}", ev.tool.clone().unwrap_or_default()),
            if ev.ok { pal().ok } else { pal().bad },
        ),
        Kind::System => ("⚙ sys".into(), if ev.ok { pal().dim } else { pal().bad }),
    }
}

fn draw_feed(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, focused) = pane(app, "feed");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);

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
    app.regions.right_top = app.feed_top;

    let w = inner.width as usize;
    let (top, sel) = (app.feed_top, app.feed_sel);
    let mut lines: Vec<Line> = Vec::new();
    let Some(sess) = app.current() else { return };
    for (i, &idx) in filtered.iter().enumerate().skip(top).take(h) {
        let Some(ev) = sess.events.get(idx) else {
            continue;
        };
        let selected = focused && i == sel;
        let time = ev
            .ts
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| "--:--:--".into());
        let (tag, color) = kind_tag(ev);
        // Narrow panes give their columns to the text rather than to
        // decoration: the clock goes first, then the tool name is squeezed.
        let (stamp, tag_w) = match w {
            0..=54 => (String::new(), 6),
            55..=79 => (String::new(), 14),
            _ => (format!("{time} "), 14),
        };
        let tag = clip_to(&tag, tag_w);
        let pad = " ".repeat(tag_w.saturating_sub(tag.chars().count()));
        let used = 2 + stamp.chars().count() + tag_w;
        let text = clip_to(&ev.head, w.saturating_sub(used));
        let body_style = if selected {
            Style::new().fg(pal().text).bg(pal().panel)
        } else {
            Style::new().fg(if ev.kind == Kind::Thinking {
                pal().muted
            } else {
                pal().body
            })
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▌" } else { " " },
                Style::new().fg(pal().gold),
            ),
            Span::styled(stamp, Style::new().fg(pal().muted)),
            Span::styled(format!("{tag}{pad} "), Style::new().fg(color)),
            Span::styled(text, body_style),
        ]));
    }
    if filtered.is_empty() {
        // Distinguish "the filter hides everything" from "nothing has happened".
        let empty = app.current().map(|s| s.events.is_empty()).unwrap_or(true);
        lines.push(Line::from(Span::styled(
            if empty {
                " no activity yet — press s to send it something"
            } else {
                " nothing matches this filter — press f to change it"
            },
            muted(),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_files(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, focused) = pane(app, "files");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);

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
        let Some(t) = sess.files.get(key) else {
            continue;
        };
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
        let age = t
            .last
            .map(|l| fmt_age((chrono::Utc::now() - l).num_seconds()))
            .unwrap_or_default();
        let right = format!("{ops} {churn}  {age} ");
        let path = clip_left(
            &ironsight_core::event::short_path(key),
            w.saturating_sub(right.chars().count() + 3),
        );
        lines.push(row(
            vec![
                Span::styled(
                    if selected { "▌" } else { " " },
                    Style::new().fg(pal().gold),
                ),
                Span::styled(
                    path,
                    if selected {
                        Style::new().fg(pal().text).bg(pal().panel)
                    } else {
                        Style::new().fg(pal().body)
                    },
                ),
            ],
            vec![
                Span::styled(format!("{ops}"), muted()),
                Span::styled(
                    format!("{churn}  "),
                    Style::new().fg(if t.added + t.removed > 0 {
                        pal().ok
                    } else {
                        pal().muted
                    }),
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

/// Draw a cursor-selectable list, keeping the selection in view.
fn draw_list(
    f: &mut Frame,
    inner: Rect,
    lines: Vec<Line<'static>>,
    sel: &mut usize,
    top: &mut usize,
    empty: &str,
) {
    let h = inner.height as usize;
    if h == 0 {
        return;
    }
    if lines.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {empty}"), muted()))),
            inner,
        );
        return;
    }
    if *sel >= lines.len() {
        *sel = lines.len() - 1;
    }
    if *sel < *top {
        *top = *sel;
    } else if *sel >= *top + h {
        *top = *sel + 1 - h;
    }
    let window: Vec<Line> = lines.into_iter().skip(*top).take(h).collect();
    f.render_widget(Paragraph::new(window), inner);
}

fn cursor(selected: bool) -> Span<'static> {
    Span::styled(
        if selected { "▌" } else { " " },
        Style::new().fg(pal().gold),
    )
}

/// The conversation as prose: what was asked and what was answered, wrapped
/// and readable, with the tool calls left out.
fn draw_read(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, _) = pane(app, "read");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);
    let Some(s) = app.current() else { return };

    let mut lines: Vec<Line> = Vec::new();
    for ev in &s.events {
        let who = match ev.kind {
            Kind::Prompt => "you",
            Kind::Text => "claude",
            _ => continue,
        };
        let time = ev
            .ts
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {who} "),
                Style::new()
                    .fg(if who == "you" { pal().gold } else { pal().text })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(time, muted()),
        ]));
        for para in ev.body.lines() {
            lines.push(Line::from(Span::styled(
                format!("   {para}"),
                Style::new().fg(pal().body),
            )));
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " nothing said yet in this session",
            muted(),
        )));
    }
    // Scrolling by line, and following the end until the reader scrolls back.
    let total = lines.len();
    let h = inner.height as usize;
    let max_top = total.saturating_sub(h);
    if app.list_top_right > max_top || app.follow {
        app.list_top_right = max_top;
    }
    let top = app.list_top_right.min(max_top);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((top as u16, 0)),
        inner,
    );
}

fn draw_plan(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, _) = pane(app, "plan");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);
    let Some(s) = app.current() else { return };
    let w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for t in &s.todos {
        let (glyph, color) = match t.status.as_str() {
            "completed" => ("✓", pal().ok),
            "in_progress" => ("▸", pal().gold),
            _ => ("○", pal().muted),
        };
        let style = if t.status == "completed" {
            Style::new()
                .fg(pal().muted)
                .add_modifier(Modifier::CROSSED_OUT)
        } else if t.status == "in_progress" {
            Style::new().fg(pal().text)
        } else {
            Style::new().fg(pal().body)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {glyph} "), Style::new().fg(color)),
            Span::styled(clip_to(&t.text, w.saturating_sub(5)), style),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no plan recorded for this session",
            muted(),
        )));
    }
    if !s.queued.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " queued prompts",
            Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
        )));
        for q in &s.queued {
            lines.push(Line::from(vec![
                Span::styled("  · ", muted()),
                Span::styled(clip_to(q, w.saturating_sub(5)), Style::new().fg(pal().body)),
            ]));
        }
    }
    let waiting = app.queues.get(&s.id).cloned().unwrap_or_default();
    if !waiting.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " held by Ironsight until idle",
            Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
        )));
        for q in &waiting {
            lines.push(Line::from(vec![
                Span::styled("  · ", muted()),
                Span::styled(clip_to(q, w.saturating_sub(5)), Style::new().fg(pal().body)),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_agents(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, _) = pane(app, "agents");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);
    let w = inner.width as usize;
    let (sel, top) = (app.list_sel, app.list_top_right);
    let lines: Vec<Line> = match app.current() {
        Some(s) => s
            .agents
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let took = match (a.started, a.finished) {
                    (Some(x), Some(y)) => fmt_ms((y - x).num_milliseconds()),
                    (Some(x), None) => {
                        format!("{} so far", fmt_age((chrono::Utc::now() - x).num_seconds()))
                    }
                    _ => "—".into(),
                };
                let right = format!("{} · {took} ", a.status);
                let text = format!("{} · {}", a.kind, a.description);
                row(
                    vec![
                        cursor(i == sel),
                        Span::styled(
                            clip_to(&text, w.saturating_sub(right.chars().count() + 3)),
                            Style::new().fg(if i == sel { pal().text } else { pal().body }),
                        ),
                    ],
                    vec![Span::styled(
                        right,
                        Style::new().fg(if a.finished.is_some() {
                            pal().muted
                        } else {
                            pal().gold
                        }),
                    )],
                    w,
                )
            })
            .collect(),
        None => Vec::new(),
    };
    let mut sel = sel;
    let mut top = top;
    draw_list(f, inner, lines, &mut sel, &mut top, "no subagents launched");
    app.list_sel = sel;
    app.list_top_right = top;
    app.regions.right_top = top;
}

fn draw_mirror(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, _) = pane(app, "mirror");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);
    let Some(s) = app.current() else { return };
    let text = match app.mirror.get(&s.id) {
        Some(t) => t.clone(),
        None => {
            let hint = if app.steer.contains_key(&s.id) {
                "reading the pane…"
            } else {
                "this session is not somewhere Ironsight can steer — press A to reopen it"
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(format!(" {hint}"), muted()))),
                inner,
            );
            return;
        }
    };
    let lines: Vec<Line> = text
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(pal().body))))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, _) = pane(app, "tree");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);
    let w = inner.width as usize;
    let (sel, top) = (app.list_sel, app.list_top_right);
    let tree = app.tree().cloned();
    let iso = app.isolation();
    let lines: Vec<Line> = match &tree {
        Some(t) => {
            let mut v = vec![Line::from(vec![
                Span::styled("  on ", muted()),
                Span::styled(t.branch.clone(), Style::new().fg(pal().gold)),
                Span::styled(
                    format!("  +{} / -{} unstaged", t.insertions, t.deletions),
                    muted(),
                ),
            ])];
            if let Some(i) = &iso {
                v.push(Line::from(vec![
                    Span::styled("  isolated ", Style::new().fg(pal().gold)),
                    Span::styled(
                        format!(
                            "· {} commit{} ahead of {} · M merge · X remove",
                            i.ahead,
                            if i.ahead == 1 { "" } else { "s" },
                            i.base
                        ),
                        muted(),
                    ),
                ]));
            }
            v.push(Line::from(""));
            v.extend(t.entries.iter().enumerate().map(|(i, e)| {
                let color = match e.code.trim() {
                    "??" => pal().muted,
                    c if c.starts_with('A') || c.starts_with('M') => pal().ok,
                    c if c.starts_with('D') => pal().bad,
                    _ => pal().body,
                };
                Line::from(vec![
                    cursor(i == sel),
                    Span::styled(format!("{:<3}", e.code.trim()), Style::new().fg(color)),
                    Span::styled(
                        clip_left(&e.path, w.saturating_sub(6)),
                        Style::new().fg(if i == sel { pal().text } else { pal().body }),
                    ),
                ])
            }));
            v
        }
        None => Vec::new(),
    };
    let mut sel = sel;
    let mut top = top;
    draw_list(
        f,
        inner,
        lines,
        &mut sel,
        &mut top,
        "not a git repository, or nothing changed",
    );
    app.list_sel = sel;
    app.list_top_right = top;
    app.regions.right_top = top;
}

fn event_line(ev: &Ev, w: usize, selected: bool, tag_extra: Option<&str>) -> Line<'static> {
    let time = ev
        .ts
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "--:--:--".into());
    let (tag, color) = kind_tag(ev);
    let tag = clip_to(&tag, 14);
    let pad = " ".repeat(14usize.saturating_sub(tag.chars().count()));
    let extra = tag_extra.map(|e| format!("{e} ")).unwrap_or_default();
    let used = 25 + extra.chars().count();
    Line::from(vec![
        cursor(selected),
        Span::styled(format!("{time} "), Style::new().fg(pal().muted)),
        Span::styled(extra, Style::new().fg(pal().gold)),
        Span::styled(format!("{tag}{pad} "), Style::new().fg(color)),
        Span::styled(
            clip_to(&ev.head, w.saturating_sub(used)),
            Style::new().fg(if selected { pal().text } else { pal().body }),
        ),
    ])
}

fn draw_errors(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, _) = pane(app, "errors");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);
    let w = inner.width as usize;
    let (sel, top) = (app.list_sel, app.list_top_right);
    let idxs = app.errors();
    let lines: Vec<Line> = match app.current() {
        Some(s) => idxs
            .iter()
            .enumerate()
            .filter_map(|(i, e)| s.events.get(*e).map(|ev| event_line(ev, w, i == sel, None)))
            .collect(),
        None => Vec::new(),
    };
    let mut sel = sel;
    let mut top = top;
    draw_list(
        f,
        inner,
        lines,
        &mut sel,
        &mut top,
        "no errors — nothing failed in this session",
    );
    app.list_sel = sel;
    app.list_top_right = top;
    app.regions.right_top = top;
}

fn draw_fleet(f: &mut Frame, app: &mut App, area: Rect) {
    let (block, _) = pane(app, "fleet");
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);
    let w = inner.width as usize;
    let (sel, top) = (app.list_sel, app.list_top_right);
    let merged = app.fleet();
    let lines: Vec<Line> = merged
        .iter()
        .enumerate()
        .filter_map(|(i, (si, ei))| {
            let s = app.sessions.get(*si)?;
            let ev = s.events.get(*ei)?;
            let tag = clip_to(&s.label(), 10);
            Some(event_line(ev, w, i == sel, Some(&format!("{tag:<10}"))))
        })
        .collect();
    let mut sel = if app.list_sel == 0 && !lines.is_empty() {
        lines.len() - 1
    } else {
        sel
    };
    let mut top = top;
    draw_list(f, inner, lines, &mut sel, &mut top, "nothing yet");
    app.list_sel = sel;
    app.list_top_right = top;
    app.regions.right_top = top;
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
    app.regions.right = (inner.x, inner.y, inner.width, inner.height);
    let Some(s) = app.current() else { return };
    let t = &s.totals;
    let w = inner.width as usize;

    let mut lines: Vec<Line> = Vec::new();
    let head = |lines: &mut Vec<Line>, title: &str| {
        lines.push(Line::from(Span::styled(
            format!(" {title}"),
            Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
        )));
    };

    head(&mut lines, "usage");
    let models: Vec<String> = s.models.iter().map(|(m, c)| format!("{m} {c}")).collect();
    lines.push(field(
        "requests",
        format!("{} · {}", t.requests, models.join(" · ")),
    ));
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
        format!(
            "read {} · write {}",
            fmt_tokens(t.cache_read),
            fmt_tokens(t.cache_write)
        ),
    ));
    let window = s.window();
    let pct = (t.ctx as f64 / window as f64 * 100.0).min(100.0);
    let filled = ((pct / 100.0) * 24.0).round() as usize;
    lines.push(Line::from(vec![
        Span::styled(format!("  {:<9}", "context"), muted()),
        Span::styled(
            format!("{} of {} ", fmt_tokens(t.ctx), fmt_tokens(window)),
            Style::new().fg(pal().body),
        ),
        Span::styled(
            "█".repeat(filled),
            Style::new().fg(if pct > 80.0 { pal().bad } else { pal().gold }),
        ),
        Span::styled("░".repeat(24 - filled), Style::new().fg(pal().panel)),
        Span::styled(format!(" {pct:.0}%"), muted()),
    ]));
    if app.show_cost {
        lines.push(field(
            "cost",
            format!("~${:.2} if this ran on the API", t.cost),
        ));
    } else {
        lines.push(field(
            "plan",
            "subscription · press $ for API-equivalent cost".into(),
        ));
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
        format!(
            "{} · avg {} · longest {}",
            s.turns,
            fmt_ms(avg_turn),
            fmt_ms(max_turn)
        ),
    ));
    let (avg_lat, max_lat) = s.latency();
    let calls: usize = s.tools.values().sum();
    lines.push(field(
        "tools",
        format!(
            "{calls} calls · avg {} · worst {}",
            fmt_ms(avg_lat),
            fmt_ms(max_lat)
        ),
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
            Style::new().fg(if s.errors > 0 { pal().bad } else { pal().body }),
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
            Span::styled(
                format!("  {:<12}", clip_to(name, 12)),
                Style::new().fg(pal().body),
            ),
            Span::styled(format!("{:>5} ", count), muted()),
            Span::styled(bar(**count, max, bar_w), Style::new().fg(pal().gold)),
        ]));
    }

    lines.push(Line::from(""));
    head(&mut lines, "activity · last 60 minutes");
    let acts = s.activity(60);
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(spark(&acts), Style::new().fg(pal().gold)),
    ]));
    lines.push(row(
        vec![Span::styled("  -60m", muted())],
        vec![Span::styled(
            format!("now{}", " ".repeat(w.saturating_sub(65).min(8))),
            muted(),
        )],
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

/// A session blocked on a question, shown wherever you are, with the answer
/// keys spelled out.
fn draw_approval(f: &mut Frame, app: &App, area: Rect) {
    let Some((s, a)) = app.approval() else { return };
    let block = Block::bordered()
        .border_style(Style::new().fg(pal().gold))
        .title(Span::styled(
            format!(" {} is waiting on you ", s.label()),
            Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " y accept · d decline · ctrl+<digit> pick an option ",
            muted(),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let mut lines = vec![Line::from(Span::styled(
        format!(" {}", clip_to(&a.question, w.saturating_sub(2))),
        Style::new().fg(pal().text),
    ))];
    for opt in a.options.iter().take(3) {
        let _ = &opt;
        lines.push(Line::from(Span::styled(
            format!("   {}", clip_to(opt, w.saturating_sub(4))),
            Style::new().fg(pal().body),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some(input) = &app.input {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▌", Style::new().fg(pal().gold)),
                Span::styled(format!(" {} ", input.label), Style::new().fg(pal().gold)),
                Span::styled("› ", muted()),
                Span::styled(
                    input.buf.chars().take(input.pos).collect::<String>(),
                    Style::new().fg(pal().text),
                ),
                Span::styled("▏", Style::new().fg(pal().gold)),
                Span::styled(
                    input.buf.chars().skip(input.pos).collect::<String>(),
                    Style::new().fg(pal().text),
                ),
                Span::styled("   enter send · esc close", muted()),
            ])),
            area,
        );
        return;
    }
    if app.passthrough {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "▌ passthrough ",
                    Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "every key goes to the selected session · ctrl+] or F12 to stop",
                    muted(),
                ),
            ])),
            area,
        );
        return;
    }
    // The strip already says what is being asked; do not say it twice.
    if app.note_visible() && app.approval().is_none() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▌ ", Style::new().fg(pal().gold)),
                Span::styled(app.note.clone(), Style::new().fg(pal().gold)),
            ])),
            area,
        );
        return;
    }
    if let Some(warning) = app.compatibility() {
        if !app.note_visible() {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("▌ ", Style::new().fg(pal().bad)),
                    Span::styled(warning, Style::new().fg(pal().bad)),
                ])),
                area,
            );
            return;
        }
    }
    // Deliberately short: everything else is one keypress away behind the
    // actions menu and the help sheet.
    let keys = "  j/k session · n new · s send · enter actions · 1…9 panes · / search · ? help";
    let state = format!(
        "{} · {} · {} ",
        app.view.label(),
        app.filter.label(),
        if app.follow { "following" } else { "paused" }
    );
    f.render_widget(
        Paragraph::new(row(
            vec![Span::styled(keys.to_string(), muted())],
            vec![Span::styled(
                state,
                Style::new().fg(if app.follow { pal().gold } else { pal().dim }),
            )],
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
                Some('+') => Style::new().fg(pal().ok),
                Some('-') => Style::new().fg(pal().bad),
                Some('@') if l.starts_with("@@") => Style::new().fg(pal().gold),
                Some('─') => Style::new().fg(pal().gold),
                _ => Style::new().fg(pal().body),
            };
            Line::from(Span::styled(l.to_string(), style))
        })
        .collect()
}

/// Read a spilled output file, capped so a giant artefact cannot wedge the UI.
fn read_capped(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.chars().take(400_000).collect())
}

fn event_popup(ev: &Ev) -> (String, Color, String) {
    let (tag, color) = kind_tag(ev);
    let body = match ev.spill.as_deref().and_then(read_capped) {
        Some(full) => format!("{full}\n\n── summary ──\n{}", ev.body),
        None => ev.body.clone(),
    };
    (tag, color, body)
}

/// Everything you can do to the selected session, in one place, with the
/// reason spelled out when you cannot do it.
fn draw_menu(f: &mut Frame, app: &mut App, area: Rect) {
    let name = app.current().map(|s| s.label()).unwrap_or_default();
    let items = app.actions();
    if items.is_empty() {
        return;
    }
    if app.menu_sel >= items.len() {
        app.menu_sel = 0;
    }
    let height = (items.len() + 4).min(area.height as usize) as u16;
    let width = 62.min(area.width.saturating_sub(4)) as u16;
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_style(Style::new().fg(pal().gold))
        .style(Style::new().bg(pal().midnight))
        .title(Span::styled(
            format!(" {name} "),
            Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(" enter to run · esc to close ", muted()));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.regions.menu = Some((inner.x, inner.y, inner.width, inner.height));

    let mut lines: Vec<Line> = Vec::new();
    for (i, a) in items.iter().enumerate() {
        let selected = i == app.menu_sel;
        let key_style = if a.enabled {
            Style::new().fg(pal().gold)
        } else {
            Style::new().fg(pal().muted)
        };
        let label_style = if !a.enabled {
            Style::new().fg(pal().muted)
        } else if selected {
            Style::new().fg(pal().text).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(pal().body)
        };
        lines.push(Line::from(vec![
            cursor(selected),
            Span::styled(format!(" {}  ", a.key), key_style),
            Span::styled(a.label.to_string(), label_style),
        ]));
    }
    // The reason for the highlighted entry, so the fix is always on screen.
    if let Some(a) = items.get(app.menu_sel) {
        lines.push(Line::from(""));
        let note = if a.enabled {
            String::new()
        } else {
            format!(" {}", a.why)
        };
        lines.push(Line::from(Span::styled(note, Style::new().fg(pal().bad))));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_popup(f: &mut Frame, app: &App, area: Rect) {
    let (title, color, body) = match app.view {
        View::Files => match app.file_history() {
            Some((path, text)) => (ironsight_core::event::short_path(&path), pal().gold, text),
            None => return,
        },
        View::Agents => {
            let Some(s) = app.current() else { return };
            let Some(a) = s.agents.get(app.list_sel) else {
                return;
            };
            let body = a
                .output_file
                .as_deref()
                .and_then(read_capped)
                .unwrap_or_else(|| {
                    format!(
                        "{}\n\nkind: {}\nmodel: {}\nstatus: {}\n\nno output file recorded",
                        a.description, a.kind, a.model, a.status
                    )
                });
            (format!("agent · {}", a.kind), pal().gold, body)
        }
        View::Tree => {
            let Some(s) = app.current() else { return };
            let Some(t) = app.trees.get(&s.id) else {
                return;
            };
            let Some(e) = t.entries.get(app.list_sel) else {
                return;
            };
            let body = ironsight_core::git::diff(std::path::Path::new(&s.cwd), &e.path)
                .unwrap_or_else(|| "no diff available".into());
            (e.path.clone(), pal().gold, body)
        }
        View::Mirror => {
            let Some(s) = app.current() else { return };
            let body = app.mirror.get(&s.id).cloned().unwrap_or_default();
            (format!("mirror · {}", s.label()), pal().gold, body)
        }
        View::Errors => {
            let Some(s) = app.current() else { return };
            let idxs = app.errors();
            let Some(ev) = idxs.get(app.list_sel).and_then(|i| s.events.get(*i)) else {
                return;
            };
            event_popup(ev)
        }
        View::Fleet => {
            let merged = app.fleet();
            let Some((si, ei)) = merged.get(app.list_sel).copied() else {
                return;
            };
            let Some(ev) = app.sessions.get(si).and_then(|s| s.events.get(ei)) else {
                return;
            };
            event_popup(ev)
        }
        View::Plan => return,
        _ => match app.event_at(app.feed_sel) {
            Some(ev) => event_popup(ev),
            None => return,
        },
    };
    let rect = centered(area, 84, 74);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_style(Style::new().fg(pal().gold))
        .style(Style::new().bg(pal().midnight))
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
    let rect = centered(area, 70, 86);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_style(Style::new().fg(pal().gold))
        .style(Style::new().bg(pal().midnight))
        .title(Span::styled(" keys ", Style::new().fg(pal().gold)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let rows: [(&str, &str); 34] = [
        ("  look", ""),
        ("j / k, ↓ ↑", "select a session"),
        (
            "ctrl+↑ ↓",
            "move it up or down — the order is yours and it keeps",
        ),
        (
            "1 … 9, 0",
            "feed files stats plan agents mirror tree errors",
        ),
        ("", "  fleet · 0 reads the conversation on its own"),
        (
            "mouse",
            "click to select, click again to open, wheel scrolls",
        ),
        (
            "J / K",
            "move in the right pane · enter opens the full text",
        ),
        ("g / G", "top / bottom — G resumes following"),
        ("f", "filter the feed: all, tools, bash, files, talk"),
        ("/ then ] [", "search every session · step the matches"),
        ("", ""),
        ("  manage", ""),
        ("enter", "actions for the selected session"),
        ("y / d", "accept or decline what a session is asking"),
        ("ctrl+digit", "pick another option on that prompt"),
        ("p", "jump to the next session waiting on you"),
        (
            "s / Q",
            "send a message · queue it for the next idle moment",
        ),
        (
            "i / m",
            "interrupt · type into it directly, ctrl+] or F12 to stop",
        ),
        (
            "a / A",
            "attach full-screen · adopt, or reopen an ended one",
        ),
        ("", "  F12 comes back, wherever you are"),
        ("R", "resume any conversation on this machine, however old"),
        ("n / W", "new session · new isolated session on a branch"),
        ("M / X", "merge that branch back · remove the checkout"),
        ("b / L", "broadcast a message · launch the fleet file"),
        (
            "K / Z",
            "close this session · close everything Ironsight started",
        ),
        ("F2", "rename the selected session"),
        ("", ""),
        ("  other", ""),
        ("$", "subscription view or API-equivalent cost"),
        ("N", "desktop notifications on or off"),
        ("l / r", "only live sessions · rescan"),
        ("tab", "switch pane focus"),
        ("q", "quit — esc only dismisses"),
        ("", ""),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            if d.is_empty() {
                // section heading
                return Line::from(Span::styled(
                    (*k).to_string(),
                    Style::new().fg(pal().gold).add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(vec![
                Span::styled(format!(" {k:<12}"), Style::new().fg(pal().gold)),
                Span::styled((*d).to_string(), Style::new().fg(pal().text)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
