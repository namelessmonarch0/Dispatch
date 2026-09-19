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

    /// Renders the task and its surrounding chrome — everything but the
    /// border — at this prompt's own `scroll`.
    ///
    /// Split out of [`Widget::render`] so [`Self::has_more_to_show`] can ask
    /// what one offset paints without nesting a second border inside the
    /// first.
    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.lines())
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .render(area, buf);
    }

    /// Whether this prompt, scrolled to its own `scroll`, still has fresh
    /// content to show at the bottom of a `width`×`height` box.
    ///
    /// How far the prompt can scroll is answered by asking this rather than
    /// by predicting a row count: a word-wrap can waste up to a whole row's
    /// width of columns when the next word will not fit, so any bound
    /// computed from the text is only ever an estimate, and an estimate that
    /// undercounts cuts off exactly the tail this prompt exists to show.
    ///
    /// Checked against the last two rows, not the whole box: once `scroll`
    /// has gone past the true end, `Paragraph::scroll` paints nothing there
    /// at all, but checking only the very last row would risk mistaking one
    /// of this prompt's own blank separator lines for that — there is
    /// always at least one, and this layout never stacks two in a row, so
    /// two rows absorbs it safely without needing to know how many rows the
    /// content actually has.
    #[must_use]
    pub fn has_more_to_show(&self, width: u16, height: u16) -> bool {
        if width == 0 || height == 0 {
            return false;
        }

        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        self.render_content(area, &mut buf);

        let checked = height.min(2);
        ((height - checked)..height)
            .any(|y| (0..width).any(|x| buf.cell((x, y)).is_some_and(|cell| cell.symbol() != " ")))
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
