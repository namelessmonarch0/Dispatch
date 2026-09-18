//! The project sidebar.
//!
//! Lists projects and, under each, its panes. The status column on the left of
//! every project is reserved for the federation slice, which lights it green
//! or red per device; in Slice 1 every project is local and the dot is dim.

use dispatch_core::{AppState, PaneStatus, ProjectId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// Width the sidebar asks for.
pub const WIDTH: u16 = 28;

/// Marker drawn in the reserved status column.
const DOT: &str = "●";

/// Renders the project list.
#[derive(Debug, Clone, Copy)]
pub struct Sidebar<'a> {
    state: &'a AppState,
}

impl<'a> Sidebar<'a> {
    /// Creates a sidebar for `state`.
    #[must_use]
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }
}

/// Writes `text` at `(x, y)`, clipped to `area`, and returns the next column.
fn write(buf: &mut Buffer, area: Rect, x: u16, y: u16, text: &str, style: Style) -> u16 {
    let mut cursor = x;

    for grapheme in text.chars() {
        if cursor >= area.x + area.width || y >= area.y + area.height {
            break;
        }
        if let Some(cell) = buf.cell_mut((cursor, y)) {
            cell.set_symbol(&grapheme.to_string());
            cell.set_style(style);
        }
        cursor += 1;
    }

    cursor
}

/// Truncates `text` to `width` columns, marking the cut with an ellipsis.
///
/// Project names come from directory names and are routinely longer than the
/// sidebar.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }

    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

/// The colour a pane's status is drawn in.
fn status_style(status: PaneStatus) -> Style {
    match status {
        PaneStatus::Starting => Style::default().fg(Color::Yellow),
        PaneStatus::Running => Style::default().fg(Color::Green),
        PaneStatus::Idle => Style::default().fg(Color::Blue),
        // An exited pane stays listed until it is closed, so it has to be
        // visibly different from one that is still working.
        PaneStatus::Exited(0) => Style::default().fg(Color::DarkGray),
        PaneStatus::Exited(_) => Style::default().fg(Color::Red),
    }
}

impl Widget for Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let selected = self.state.selected_project();
        let focused = self.state.focused_pane();
        let mut y = area.y;

        for project in self.state.projects() {
            if y >= area.y + area.height {
                return;
            }

            y = self.render_project(buf, area, y, project.id, selected);

            for pane in self.state.panes_for(project.id) {
                if y >= area.y + area.height {
                    return;
                }

                let marker = if focused == Some(pane.id) { "▸" } else { " " };
                let style = if focused == Some(pane.id) {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let x = write(buf, area, area.x + 2, y, marker, style);
                let x = write(buf, area, x + 1, y, DOT, status_style(pane.status));

                let room = (area.x + area.width).saturating_sub(x + 2) as usize;
                write(buf, area, x + 2, y, &truncate(&pane.title, room), style);

                y += 1;
            }
        }
    }
}

impl Sidebar<'_> {
    /// Draws one project row and returns the next line.
    fn render_project(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        id: ProjectId,
        selected: Option<ProjectId>,
    ) -> u16 {
        let project = self
            .state
            .projects()
            .iter()
            .find(|p| p.id == id)
            .expect("the caller iterates over registered projects");

        let is_selected = selected == Some(id);
        let style = if is_selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        // Reserved for the federation slice: green when the device hosting
        // this project is reachable, red when it is not. Everything is local
        // in Slice 1, so it stays dim.
        let x = write(
            buf,
            area,
            area.x,
            y,
            DOT,
            Style::default().fg(Color::DarkGray),
        );

        let room = (area.x + area.width).saturating_sub(x + 1) as usize;
        write(buf, area, x + 1, y, &truncate(&project.name, room), style);

        y + 1
    }
}

#[cfg(test)]
mod tests;
