//! One line of typed input, for the questions a list cannot answer.
//!
//! A machine's ssh target and a path on another machine have nothing to pick
//! from: this client cannot see the other machine's filesystem, and the hosts
//! a user can reach live in their ssh configuration, not in Dispatch's.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::picker::{centred, write};

/// A line under the input saying what is happening, or what went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// Work in progress, such as a machine being dialled.
    Busy(String),
    /// Why the last answer was not accepted.
    Error(String),
}

/// A question and what has been typed in answer.
#[derive(Debug, Clone)]
pub struct Prompt {
    title: String,
    hint: String,
    input: String,
    note: Option<Note>,
}

/// The narrowest a prompt is drawn, so a short title still leaves room to
/// type a path.
const MIN_WIDTH: u16 = 44;

impl Prompt {
    /// An empty prompt.
    #[must_use]
    pub fn new(title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            hint: hint.into(),
            input: String::new(),
            note: None,
        }
    }

    /// The same prompt with something already typed, for a default the user
    /// can accept or edit.
    #[must_use]
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }

    /// The question.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Everything typed, as typed.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// What was typed without surrounding whitespace, or `None` when that
    /// leaves nothing.
    ///
    /// Enter on nothing must do nothing: an empty path or an empty machine
    /// name is never what the user meant.
    #[must_use]
    pub fn answer(&self) -> Option<&str> {
        let trimmed = self.input.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    /// Types a character.
    ///
    /// Clears an error, which was about what was typed before, but not a
    /// busy note, which is about work still going on.
    pub fn push(&mut self, c: char) {
        self.input.push(c);
        if matches!(self.note, Some(Note::Error(_))) {
            self.note = None;
        }
    }

    /// Removes the last character, if there is one.
    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// The line under the input, if there is one.
    #[must_use]
    pub fn note(&self) -> Option<&Note> {
        self.note.as_ref()
    }

    /// Replaces the line under the input.
    pub fn set_note(&mut self, note: Option<Note>) {
        self.note = note;
    }
}

impl Widget for &Prompt {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 8 || area.height < 5 {
            return;
        }

        let widest = [
            self.title.chars().count(),
            self.hint.chars().count(),
            self.input.chars().count(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let width = u16::try_from(widest + 6)
            .unwrap_or(u16::MAX)
            .clamp(MIN_WIDTH.min(area.width), area.width);

        // Border, input, hint, note.
        let rect = centred(area, width, 5);

        // Floats over the grid, so whatever it covers is erased rather than
        // left showing through.
        Clear.render(rect, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", self.title))
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(rect);
        block.render(rect, buf);

        // The end of a long input rather than its start: the end is where the
        // user is typing.
        let room = usize::from(inner.width.saturating_sub(3));
        let count = self.input.chars().count();
        let shown: String = self
            .input
            .chars()
            .skip(count.saturating_sub(room))
            .collect();
        let x = write(
            buf,
            inner,
            inner.x,
            inner.y,
            "> ",
            Style::default().fg(Color::Cyan),
        );
        let x = write(buf, inner, x, inner.y, &shown, Style::default());
        write(
            buf,
            inner,
            x,
            inner.y,
            "▏",
            Style::default().fg(Color::Cyan),
        );

        write(
            buf,
            inner,
            inner.x,
            inner.y + 1,
            &self.hint,
            Style::default().fg(Color::DarkGray),
        );

        if let Some(note) = &self.note {
            let (text, colour) = match note {
                Note::Busy(text) => (text, Color::Yellow),
                Note::Error(text) => (text, Color::Red),
            };
            write(
                buf,
                inner,
                inner.x,
                inner.y + 2,
                text,
                Style::default().fg(colour),
            );
        }
    }
}

#[cfg(test)]
mod tests;
