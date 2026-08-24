//! A session's screen, in a shape a window can draw.
//!
//! The terminal view can simply show a session's own drawing, because it is a
//! terminal. A window is not, so the screen has to be taken apart first: what
//! character is in each cell, what colour it is, where the cursor sits. That is
//! what a terminal emulator does, and Sightline already carries one for the sessions
//! it hosts itself, so the same parser is pointed at tmux's rendering too.
//!
//! Colours come out as CSS. The first sixteen are named rather than resolved —
//! `var(--ansi-3)` — because those sixteen are a theme, and the interface should
//! be allowed to have its own rather than being handed a terminal's.

use serde::{Deserialize, Serialize};

/// A stretch of one line with a single style, which is what a window wants to
/// draw. Cells are per character; runs are per span of identical ones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    /// CSS colour, or None for whatever the interface uses as its foreground
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Run {
    fn styled_like(&self, other: &Run) -> bool {
        self.fg == other.fg
            && self.bg == other.bg
            && self.bold == other.bold
            && self.dim == other.dim
            && self.italic == other.italic
            && self.underline == other.underline
            && self.inverse == other.inverse
    }
}

/// One rendering of a session's screen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    /// where the caret is, so a window can draw one
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub lines: Vec<Vec<Run>>,
    /// Terminals attached to this session other than Sightline.
    ///
    /// It decides who owns the size. A session nobody is sitting in can be
    /// reshaped to fit a window; one that a person is watching in their own
    /// terminal cannot, because the two would pull it between two widths and
    /// every line would wrap in the wrong place in both.
    pub attached: usize,
}

/// The six-by-six-by-six colour cube and the greys above it, which is what an
/// indexed colour past the first sixteen means.
fn indexed(i: u8) -> String {
    if i < 16 {
        // A theme's colour, named rather than resolved.
        return format!("var(--ansi-{i})");
    }
    if i >= 232 {
        let v = 8 + 10 * u32::from(i - 232);
        return format!("#{v:02x}{v:02x}{v:02x}");
    }
    let i = u32::from(i - 16);
    let step = |v: u32| if v == 0 { 0 } else { 55 + 40 * v };
    let (r, g, b) = (step(i / 36), step((i / 6) % 6), step(i % 6));
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn css(color: vt100::Color) -> Option<String> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
    }
}

/// Take a parsed screen apart into runs.
pub fn frame_of(screen: &vt100::Screen) -> Frame {
    let (rows, cols) = screen.size();
    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut runs: Vec<Run> = Vec::new();
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            // The second half of a wide character is not a character.
            if cell.is_wide_continuation() {
                continue;
            }
            let text = cell.contents();
            let next = Run {
                text: if text.is_empty() {
                    " ".into()
                } else {
                    text.to_string()
                },
                fg: css(cell.fgcolor()),
                bg: css(cell.bgcolor()),
                bold: cell.bold(),
                dim: cell.dim(),
                italic: cell.italic(),
                underline: cell.underline(),
                inverse: cell.inverse(),
            };
            match runs.last_mut() {
                Some(last) if last.styled_like(&next) => last.text.push_str(&next.text),
                _ => runs.push(next),
            }
        }
        // The right-hand side of a line is blank far more often than not, and
        // sending eighty spaces per row twenty times a second is most of the
        // traffic. A blank with a colour behind it is not blank, so it stays.
        while let Some(last) = runs.last_mut() {
            if last.bg.is_some() || last.inverse {
                break;
            }
            let keep = last.text.trim_end_matches(' ').len();
            if keep == last.text.len() {
                break;
            }
            last.text.truncate(keep);
            if last.text.is_empty() {
                runs.pop();
            }
        }
        lines.push(runs);
    }
    Frame {
        cols,
        rows,
        cursor: screen.cursor_position(),
        cursor_visible: !screen.hide_cursor(),
        lines,
        // Filled in by whoever knows: only the backend can say who else is
        // looking at this session.
        attached: 0,
    }
}

/// Parse a rendering of a screen — escape sequences and all — at a given size.
pub fn frame_from_render(render: &[u8], cols: u16, rows: u16) -> Frame {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(render);
    frame_of(parser.screen())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_colour_and_weight_and_joins_what_matches() {
        // Bold red "ERR", then plain " ok".
        let render = b"\x1b[1;31mERR\x1b[0m ok";
        let f = frame_from_render(render, 20, 2);
        let line = &f.lines[0];
        assert_eq!(line[0].text, "ERR");
        assert_eq!(line[0].fg.as_deref(), Some("var(--ansi-1)"));
        assert!(line[0].bold);
        assert_eq!(line[1].text, " ok", "same style, so one run");
        assert!(!line[1].bold);
    }

    #[test]
    fn keeps_the_faint_text_a_session_writes_its_hints_in() {
        let f = frame_from_render(b"\x1b[2mesc to interrupt", 30, 1);
        assert!(f.lines[0][0].dim, "dim is a colour decision, not noise");
    }

    #[test]
    fn resolves_colour_past_the_theme_and_keeps_true_colour() {
        // 24-bit, which is what Claude Code actually draws with.
        let f = frame_from_render(b"\x1b[38;2;192;133;66mgold", 10, 1);
        assert_eq!(f.lines[0][0].fg.as_deref(), Some("#c08542"));
        // Indexed, in the cube rather than the theme.
        let f = frame_from_render(b"\x1b[38;5;39mblue", 10, 1);
        assert_eq!(f.lines[0][0].fg.as_deref(), Some("#00afff"));
        // Indexed, in the greys.
        let f = frame_from_render(b"\x1b[38;5;240mgrey", 10, 1);
        assert_eq!(f.lines[0][0].fg.as_deref(), Some("#585858"));
    }

    #[test]
    fn draws_the_box_characters_a_session_is_made_of() {
        let f = frame_from_render("╭─┬─╮\r\n│ x │".as_bytes(), 8, 2);
        let first: String = f.lines[0].iter().map(|r| r.text.as_str()).collect();
        assert_eq!(first, "╭─┬─╮");
        let second: String = f.lines[1].iter().map(|r| r.text.as_str()).collect();
        assert_eq!(second, "│ x │");
    }

    #[test]
    fn a_wide_character_is_one_cell_not_two() {
        let f = frame_from_render("日本".as_bytes(), 8, 1);
        let text: String = f.lines[0].iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "日本", "no blank halves");
    }

    #[test]
    fn says_where_the_caret_is() {
        let f = frame_from_render(b"> hello", 20, 3);
        assert_eq!(f.cursor, (0, 7));
        assert!(f.cursor_visible);
    }

    #[test]
    fn does_not_send_the_empty_right_hand_side_of_every_line() {
        let f = frame_from_render(b"hi", 200, 1);
        assert_eq!(f.lines[0].len(), 1);
        assert_eq!(f.lines[0][0].text, "hi");
    }
}
