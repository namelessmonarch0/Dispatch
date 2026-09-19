//! The prompt that asks whether a pane may delegate.
//!
//! Shows the whole task. Approving something you cannot read is not approval, so
//! a long task wraps and scrolls rather than being cut to fit.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

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

    /// Renders the task and its surrounding chrome — everything but the
    /// border — at this prompt's own `scroll`.
    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.lines())
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .render(area, buf);
    }

    /// How many rows this prompt needs once wrapped to `width` columns, at
    /// no scroll — the true total, not an estimate.
    ///
    /// A caller clamping how far this can scroll needs this rather than
    /// `task.lines().count()`: `Paragraph::scroll` counts *wrapped* rows, and
    /// a task delivered as a single long line — exactly what `dispatch
    /// delegate "…"` sends — still wraps into several of them once rendered.
    ///
    /// Found by rendering into a buffer proven tall enough that nothing can
    /// be clipped, then scanning up for the last painted row. The proof:
    /// greedy word-wrap places at least one word-fragment on every row it
    /// emits — a row is only ever broken because the next fragment will not
    /// fit, and a word wider than `width` is itself split into
    /// `ceil(word_width / width)` fragments — so one logical line can never
    /// need more rows than the fragments its own words split into:
    ///
    /// `rows(line) <= max(1, sum over words in line of ceil(word_width / width))`
    ///
    /// `max(1, …)` covers a blank or whitespace-only line, which has no
    /// words at all but still occupies a row. Summing that over every line
    /// this prompt renders — its chrome included, not only the task — gives
    /// a buffer height that cannot clip, so the scan for the last painted
    /// row finds the exact total rather than a guess at it.
    ///
    /// A word's width is measured with `unicode-width` (pinned to the exact
    /// version ratatui itself depends on) via the same `UnicodeWidthStr`
    /// trait `Span::width`/`Line::width` use internally, so a task with wide
    /// characters or combining marks is bounded the same way this widget
    /// actually measures it.
    #[must_use]
    pub fn total_rows(&self, width: u16) -> u16 {
        if width == 0 {
            return 0;
        }

        let lines = self.lines();

        let bound: usize = lines
            .iter()
            .map(|line| {
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                text.split_whitespace()
                    .map(|word| UnicodeWidthStr::width(word).div_ceil(usize::from(width)))
                    .sum::<usize>()
                    .max(1)
            })
            .sum();
        let bound = u16::try_from(bound).unwrap_or(u16::MAX);

        let area = Rect::new(0, 0, width, bound);
        let mut buf = Buffer::empty(area);
        // Deliberately unscrolled: this measures the whole content once,
        // independent of whatever `self.scroll` happens to be right now.
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

        self.render_content(inner, buf);
    }
}
