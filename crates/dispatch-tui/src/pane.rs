//! Painting one pane's screen into a ratatui buffer.

use dispatch_pty::{Attrs, Cell, Rgb, Screen};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// Converts a resolved colour into ratatui's.
fn color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

/// Converts decoration into ratatui modifiers.
fn modifiers(attrs: &Attrs) -> Modifier {
    let mut m = Modifier::empty();
    m.set(Modifier::BOLD, attrs.bold);
    m.set(Modifier::ITALIC, attrs.italic);
    m.set(Modifier::DIM, attrs.faint);
    m.set(Modifier::UNDERLINED, attrs.underline);
    m.set(Modifier::REVERSED, attrs.inverse);
    m.set(Modifier::CROSSED_OUT, attrs.strikethrough);
    m.set(Modifier::HIDDEN, attrs.invisible);
    // Blink is deliberately dropped. Several agents blink a cursor or a
    // spinner, and a grid of blinking panes is unreadable.
    m
}

/// The style for one cell.
fn style(cell: &Cell) -> Style {
    let mut style = Style::default().add_modifier(modifiers(&cell.attrs));

    // A cell with no colour inherits the host terminal's, which is what makes
    // a pane look native inside whatever theme the user runs.
    if let Some(fg) = cell.fg {
        style = style.fg(color(fg));
    }
    if let Some(bg) = cell.bg {
        style = style.bg(color(bg));
    }

    style
}

/// Renders one pane's screen.
///
/// Borrows the screen rather than owning it, so a frame costs no copy of the
/// grid.
#[derive(Debug, Clone, Copy)]
pub struct PaneWidget<'a> {
    screen: &'a Screen,
    focused: bool,
}

impl<'a> PaneWidget<'a> {
    /// Creates a widget for `screen`.
    #[must_use]
    pub fn new(screen: &'a Screen) -> Self {
        Self {
            screen,
            focused: false,
        }
    }

    /// Marks the pane as focused, which is what draws its cursor.
    ///
    /// Only the focused pane shows a cursor: several visible cursors would
    /// give no clue where typing lands.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

impl Widget for PaneWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // The pane may be larger or smaller than the area it was given: a
        // resize reaches the emulator and the layout at different moments, so
        // drawing must clip rather than assume they agree.
        let rows = usize::from(area.height.min(self.screen.size.rows));
        let cols = usize::from(area.width.min(self.screen.size.cols));

        // Everything the screen does not cover is blanked first. A pane whose
        // emulator is smaller than its rectangle -- which is every pane for at
        // least a frame after the grid changes shape, and longer when a daemon
        // is between the two -- would otherwise leave whatever was drawn there
        // before on the screen. That is not a stale pixel here and there: it is
        // another pane's output sitting inside this one's borders.
        for y in 0..area.height {
            for x in 0..area.width {
                if usize::from(y) < rows && usize::from(x) < cols {
                    continue;
                }

                if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                    cell.reset();
                }
            }
        }

        for y in 0..rows {
            let Some(row) = self.screen.rows.get(y) else {
                break;
            };

            let mut x = 0usize;
            while x < cols {
                let Some(cell) = row.get(x) else {
                    break;
                };

                let Some(target) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) else {
                    break;
                };

                target.set_style(style(cell));

                if cell.text.is_empty() {
                    // A blank cell still carries its background, so it must be
                    // painted rather than skipped.
                    target.set_symbol(" ");
                    x += 1;
                    continue;
                }

                target.set_symbol(&cell.text);

                // A wide character occupies two cells. ratatui draws the
                // symbol in the first; the second must be cleared, or the
                // character underneath it shows through.
                let width = unicode_width::UnicodeWidthStr::width(cell.text.as_str()).max(1);
                for skipped in 1..width {
                    let sx = x + skipped;
                    if sx >= cols {
                        break;
                    }
                    if let Some(next) = buf.cell_mut((area.x + sx as u16, area.y + y as u16)) {
                        next.set_symbol("");
                        next.set_style(style(cell));
                    }
                }

                x += width;
            }
        }
    }
}

impl PaneWidget<'_> {
    /// Where the terminal cursor should be placed, in screen coordinates.
    ///
    /// Returns nothing when the pane is not focused or its cursor is hidden or
    /// outside `area`. The caller passes this to the backend so the real
    /// cursor lands in the right pane, which is what makes typing feel native.
    #[must_use]
    pub fn cursor_position(&self, area: Rect) -> Option<(u16, u16)> {
        if !self.focused {
            return None;
        }

        let (x, y) = self.screen.cursor?;
        if x >= area.width || y >= area.height {
            return None;
        }

        Some((area.x + x, area.y + y))
    }
}

#[cfg(test)]
mod tests;
