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

    /// How many rows this prompt needs once wrapped to `width` columns.
    ///
    /// A task delivered as a single long line still wraps once rendered —
    /// `Paragraph::scroll` counts *wrapped* rows, not the logical lines
    /// `str::lines` sees — so a caller clamping how far this can scroll needs
    /// this, not `task.lines().count()`. Found by actually rendering the same
    /// lines [`Widget::render`] draws, rather than reimplementing ratatui's
    /// word-wrap by hand: the two can never drift apart, and this needs no
    /// unstable API.
    #[must_use]
    pub fn total_rows(&self, width: u16) -> u16 {
        if width == 0 {
            return 0;
        }

        let lines = self.lines();

        // A hard break — the worst case, forced when a line has no word
        // boundary to wrap at — needs exactly `line.width()` characters
        // packed `width` to a row: `line.width().div_ceil(width)`. A word-wrap
        // that instead breaks at whitespace can only need that many rows or
        // one more per line, for whatever a word boundary leaves unused at
        // the end of a row. Summed in `usize` rather than `u16` so a line
        // past 65,535 columns is divided before it is ever truncated, not
        // truncated first and divided short.
        let bound: usize = lines
            .iter()
            .map(|line| line.width().div_ceil(usize::from(width)).saturating_add(1))
            .fold(0usize, usize::saturating_add)
            .max(1);
        let bound = u16::try_from(bound).unwrap_or(u16::MAX);

        let area = Rect::new(0, 0, width, bound);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);

        // The greatest row that still has anything painted on it.
        (0..bound)
            .rev()
            .find(|&y| {
                (0..width).any(|x| buf.cell((x, y)).is_some_and(|cell| cell.symbol() != " "))
            })
            .map_or(0, |y| y + 1)
    }
}

impl Widget for Approval<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} wants to delegate ", self.asking));
        let inner = block.inner(area);
        block.render(area, buf);

        let scroll = self.scroll;
        Paragraph::new(self.lines())
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval<'a>(task: &'a str, waiting: usize) -> Approval<'a> {
        Approval {
            asking: "Claude Code",
            harness: "claude",
            project: "dispatch",
            depth: 0,
            task,
            waiting,
            scroll: 0,
        }
    }

    #[test]
    fn a_hard_break_needs_exactly_one_row_per_width_worth_of_characters() {
        // A single "word" with no whitespace at all cannot wrap at a word
        // boundary, so ratatui hard-breaks it every `width` columns — the
        // worst case `total_rows`'s bound is built around. 4000 columns at a
        // width of 68 needs ceil(4000 / 68) = 59 rows for the task alone.
        let task = "x".repeat(4000);
        let widget = approval(&task, 0);

        let task_rows = 4000usize.div_ceil(68);
        // harness/project/depth (1) + blank (1) + task + blank (1) + legend (1).
        let expected = u16::try_from(task_rows + 4).expect("fits comfortably in a u16");

        assert_eq!(widget.total_rows(68), expected);
    }

    #[test]
    fn a_queued_second_request_adds_exactly_one_row() {
        // The only way the queue's length changes what is rendered: an extra
        // "N more waiting" line, once there is one.
        let solo = approval("a short task", 0);
        let with_one_more = approval("a short task", 1);

        assert_eq!(with_one_more.total_rows(40), solo.total_rows(40) + 1);
    }

    #[test]
    fn an_empty_task_still_measures_the_chrome_around_it() {
        // `str::lines` yields nothing for an empty string, so the task
        // contributes no rows of its own here — only the chrome does:
        // harness/project/depth (1) + blank (1) + blank (1) + legend (1). A
        // wide enough box that neither of those two text lines wraps on its
        // own — this is about the chrome's line *count*, not its wrapping.
        let widget = approval("", 0);
        assert_eq!(widget.total_rows(100), 4);
    }
}
