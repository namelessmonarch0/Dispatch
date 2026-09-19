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

impl Widget for Approval<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} wants to delegate ", self.asking));
        let inner = block.inner(area);
        block.render(area, buf);

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

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .render(inner, buf);
    }
}
