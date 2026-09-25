//! The prompt that asks whether a pane may delegate.
//!
//! Shows the whole task. Approving something you cannot read is not approval, so
//! a long task wraps and scrolls rather than being cut to fit.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

/// One request, as the user needs to see it.
pub struct Approval<'a> {
    /// Title of the pane that asked.
    pub asking: &'a str,
    /// Which harness would run.
    pub harness: &'a str,
    /// Which project it would run in.
    pub project: &'a str,
    /// How deep the asking pane already is.
    pub depth: u8,
    /// What it would be asked to do.
    pub task: &'a str,
    /// How many further requests are queued behind this one.
    pub waiting: usize,
    /// First line of the task to show, for scrolling a long one.
    pub scroll: u16,
    /// The frame's colour.
    pub border: Style,
}

impl<'a> Approval<'a> {
    /// The rect this prompt draws its content into, inside the border.
    ///
    /// Shared with the caller so scrolling can be clamped against what the
    /// box can actually show, rather than a copy of the border math kept
    /// separately and left to drift out of sync with this one.
    #[must_use]
    pub fn inner(area: Rect) -> Rect {
        Block::default().borders(Borders::ALL).inner(area)
    }

    /// The lines this prompt renders, before wrapping: the harness/project/
    /// depth line, a blank line, the task itself, a blank line, an optional
    /// "N more waiting" line, and the key legend.
    fn lines(&self) -> Vec<Line<'a>> {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("harness  ", Style::default().fg(Color::DarkGray)),
                Span::raw(self.harness),
                Span::styled("   project  ", Style::default().fg(Color::DarkGray)),
                Span::raw(self.project),
                Span::styled("   depth  ", Style::default().fg(Color::DarkGray)),
                Span::raw(self.depth.to_string()),
            ]),
            Line::from(""),
        ];

        for line in self.task.lines() {
            lines.push(Line::from(line.to_string()));
        }

        lines.push(Line::from(""));
        if self.waiting > 0 {
            lines.push(Line::styled(
                format!("{} more waiting", self.waiting),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(vec![
            Span::styled("a", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" approve   "),
            Span::styled("d", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" deny   "),
            Span::styled("A", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" approve all from this pane   "),
            Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" later"),
        ]));

        lines
    }

    /// Renders the task and its surrounding chrome — everything but the
    /// border — at this prompt's own `scroll`.
    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.lines())
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .render(area, buf);
    }

    /// How many rows this prompt needs once wrapped to `width` columns — the
    /// true total, not an estimate of it.
    ///
    /// A caller clamping how far this can scroll needs this rather than
    /// `task.lines().count()`: `Paragraph::scroll` counts *wrapped* rows, and
    /// a task delivered as a single long line — exactly what `dispatch
    /// delegate "…"` sends — still wraps into several of them once rendered.
    ///
    /// Three attempts at reimplementing that count by hand — a character
    /// sum, a per-line `div_ceil`, a two-row probe — were each wrong for text
    /// a test at only one terminal width did not happen to exercise: interior
    /// whitespace that `Wrap { trim: false }` paints and a plain word count
    /// discards, multi-column graphemes that break "a row fills to its last
    /// column," a paragraph break that looks identical to having scrolled
    /// past the end. `Paragraph::line_count` is not a fourth guess: it drives
    /// the same `WordWrapper` the render path itself uses, on the same
    /// `Line`s and the same `Wrap`, so this is what will actually be drawn,
    /// not a prediction of it.
    #[must_use]
    pub fn total_rows(&self, width: u16) -> u16 {
        let count = Paragraph::new(self.lines())
            .wrap(Wrap { trim: false })
            .line_count(width);
        u16::try_from(count).unwrap_or(u16::MAX)
    }
}

impl Widget for Approval<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} wants to delegate ", self.asking))
            .border_style(self.border);
        let inner = block.inner(area);
        block.render(area, buf);

        self.render_content(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(task: &str) -> Approval<'_> {
        Approval {
            asking: "Claude Code",
            harness: "claude",
            project: "dispatch",
            depth: 0,
            task,
            waiting: 0,
            scroll: 0,
            border: Style::default(),
        }
    }

    /// The row count found by brute force: render into a buffer generous
    /// enough that nothing could possibly be clipped, then scan up for the
    /// last painted row.
    ///
    /// An independent check on `total_rows`, not a second implementation of
    /// it competing to be the one that is right — deliberately wasteful (a
    /// large fixed buffer) rather than clever, so it has no wrapping
    /// arithmetic of its own to get wrong.
    fn brute_force_rows(widget: &Approval<'_>, width: u16) -> u16 {
        const GENEROUS_HEIGHT: u16 = 4000;
        let area = Rect::new(0, 0, width, GENEROUS_HEIGHT);
        let mut buf = Buffer::empty(area);
        Paragraph::new(widget.lines())
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);

        (0..GENEROUS_HEIGHT)
            .rev()
            .find(|&y| {
                (0..width).any(|x| buf.cell((x, y)).is_some_and(|cell| cell.symbol() != " "))
            })
            .map_or(0, |y| y + 1)
    }

    #[test]
    fn total_rows_agrees_with_a_brute_force_render_across_shapes_and_widths() {
        // This is the test that would have caught every wrong bound this
        // widget has had: it does not assert what the right answer *should*
        // be, only that `total_rows` — whatever it does internally — never
        // disagrees with what actually gets painted.
        let long_word = "x".repeat(4000);
        let ordinary_prose = (0..300)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let paragraphs = (0..8)
            .map(|n| {
                (0..20)
                    .map(|i| format!("p{n}w{i}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let cjk = "日本語のテキストです".repeat(50);
        let whitespace_line = format!("{}\n   \n{}", "a".repeat(50), "b".repeat(50));
        let irregular_spacing = (0..40)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(&" ".repeat(25));

        let tasks: [&str; 7] = [
            "write the tests",
            &long_word,
            &ordinary_prose,
            &paragraphs,
            &cjk,
            &whitespace_line,
            &irregular_spacing,
        ];

        for task in tasks {
            let widget = approval(task);
            for width in [18u16, 20, 30, 74, 76] {
                assert_eq!(
                    widget.total_rows(width),
                    brute_force_rows(&widget, width),
                    "total_rows disagreed with a brute-force render for {task:?} at width {width}"
                );
            }
        }
    }
}
