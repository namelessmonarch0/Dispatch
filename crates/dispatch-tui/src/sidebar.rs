//! The project sidebar.
//!
//! Lists projects and, under each, its panes. The status column on the left of
//! every project is reserved for the federation slice, which lights it green
//! or red per device; in Slice 1 every project is local and the dot is dim.

use dispatch_core::{AppState, Pane, PaneId, PaneStatus, ProjectId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Widget};

/// Width the sidebar asks for.
///
/// Four columns wider than the list itself needs: two go to the frame and two
/// to the twisty column, so the room a title has to be read in is what it was
/// before either was drawn.
pub const WIDTH: u16 = 32;

/// The title on the frame.
const TITLE: &str = " Projects ";

/// Marker drawn in the reserved status column.
const DOT: &str = "●";

/// The twisty of a node whose children are drawn.
const OPEN: &str = "▾";

/// The twisty of a node whose children are hidden.
const SHUT: &str = "▸";

/// Drawn where a node has no children to hide.
const LEAF: &str = " ";

/// Marks the focused pane's row.
///
/// A bar rather than an arrow: the twisty beside it is already an arrow, and
/// two of them in one row read as one control.
const FOCUS: &str = "▌";

/// The frame drawn around the list.
fn block() -> Block<'static> {
    Block::bordered().title(TITLE)
}

/// The area rows are drawn in: everything inside the frame.
///
/// Shared by rendering and hit testing, so a click lands on the row the eye
/// sees rather than one line off it.
#[must_use]
fn inner(area: Rect) -> Rect {
    block().inner(area)
}

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

/// The glyph in a row's twisty column.
///
/// A node with nothing under it gets a blank rather than a twisty: a control
/// that toggles nothing invites a click that does nothing.
fn twisty(has_children: bool, collapsed: bool) -> &'static str {
    match (has_children, collapsed) {
        (false, _) => LEAF,
        (true, true) => SHUT,
        (true, false) => OPEN,
    }
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

/// What a pane's outcome looks like in one glyph, drawn at the end of its row.
///
/// Only delegated work gets one. The status dot already says what a pane is
/// doing, so a glyph beside it on an ordinary pane is the same fact twice —
/// and a column of `⋯` against every shell was noise that made the one row
/// actually reporting an outcome harder to find. A subagent is the case the
/// glyph exists for: its parent is waiting on how the run turned out.
///
/// A tombstone keeps its glyph whatever it is: a closed row with live work
/// beneath it has to look different from a row that is merely idle.
fn outcome(pane: &Pane) -> Option<(&'static str, Style)> {
    if pane.closed {
        return Some(("⊘", Style::default().fg(Color::DarkGray)));
    }

    pane.parent?;

    Some(match pane.status {
        PaneStatus::Exited(0) => ("✓", Style::default().fg(Color::Green)),
        PaneStatus::Exited(_) => ("!", Style::default().fg(Color::Red)),
        _ => ("⋯", Style::default().fg(Color::DarkGray)),
    })
}

/// One line of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A project heading.
    Project(ProjectId),
    /// A pane, drawn `indent` columns in from the list's edge.
    Pane(PaneId, u16),
}

/// Every row the sidebar draws, top to bottom.
///
/// Rendering and hit testing both walk this, so a click can only ever land on
/// a row that is actually drawn — which is what makes collapsing safe: a
/// hidden branch is missing from both at once.
///
/// `panes_for` is unfiltered, so a closed pane still appears here when it is
/// kept as a tombstone for live children below it — the sidebar is where that
/// row earns its keep.
///
/// One level of children, deliberately: at the default `max_depth` of 1 a
/// subagent cannot delegate, so one level is the whole tree. Raise that cap
/// and a subagent's own subagent is tracked in state — the daemon owns it,
/// `^a s` can focus it, and closing its ancestors will not delete it — but it
/// has no row here. Drawing an arbitrary depth in a column this narrow needs a
/// shape nobody has designed yet, so the honest thing is to say where the
/// drawing stops rather than imply it does not.
fn rows(state: &AppState) -> Vec<Row> {
    let mut rows = Vec::new();

    for project in state.projects() {
        rows.push(Row::Project(project.id));

        if state.is_project_collapsed(project.id) {
            continue;
        }

        for pane in state.panes_for(project.id) {
            if pane.parent.is_some() {
                // Drawn under its parent, below, not in its own right.
                continue;
            }

            rows.push(Row::Pane(pane.id, 2));

            if state.is_pane_collapsed(pane.id) {
                continue;
            }

            for child in state.children_of(pane.id) {
                rows.push(Row::Pane(child.id, 4));
            }
        }
    }

    rows
}

impl Widget for Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        block().render(area, buf);
        let area = inner(area);
        if area.width == 0 || area.height == 0 {
            return;
        }

        let selected = self.state.selected_project();
        let focused = self.state.focused_pane();

        for (offset, row) in rows(self.state).into_iter().enumerate() {
            let Ok(offset) = u16::try_from(offset) else {
                return;
            };
            let y = area.y + offset;
            if y >= area.y + area.height {
                return;
            }

            match row {
                Row::Project(id) => self.render_project(buf, area, y, id, selected),
                Row::Pane(id, indent) => {
                    let pane = self
                        .state
                        .pane(id)
                        .expect("every row names a pane that is still in state");
                    self.render_pane(buf, area, y, pane, focused, indent);
                }
            }
        }
    }
}

/// What sits under a click on the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A project heading. The whole row is its control: there is no pane on it
    /// to focus, so a click both selects the project and folds its panes.
    Project(ProjectId),
    /// The twisty of a pane that has children.
    Twisty(PaneId),
    /// The rest of a live pane's row.
    Pane(PaneId),
}

/// What, if anything, sits at `(x, y)` in a sidebar drawn at `area`.
///
/// The sidebar has no keyboard focus of its own, so a pointer is the only way
/// to pick one row out of the list; this is what lets a click reach a pane the
/// tiled grid does not currently show. A closed pane's tombstone row answers
/// `Hit::Twisty` over its twisty and nothing elsewhere: there is no process
/// left to focus, but the children it is still holding can be folded away.
#[must_use]
pub fn hit_test(state: &AppState, area: Rect, x: u16, y: u16) -> Option<Hit> {
    let area = inner(area);

    if x < area.x || x >= area.x + area.width || y < area.y || y >= area.y + area.height {
        return None;
    }

    let index = usize::from(y - area.y);

    match *rows(state).get(index)? {
        Row::Project(id) => Some(Hit::Project(id)),
        Row::Pane(id, indent) => {
            let pane = state.pane(id)?;

            // A blank twisty column is not a control, so a click there falls
            // through to the row it is part of.
            if x == area.x + indent && !state.children_of(id).is_empty() {
                return Some(Hit::Twisty(id));
            }

            (!pane.closed).then_some(Hit::Pane(id))
        }
    }
}

impl Sidebar<'_> {
    /// Draws one project row.
    fn render_project(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        id: ProjectId,
        selected: Option<ProjectId>,
    ) {
        let project = self
            .state
            .projects()
            .iter()
            .find(|p| p.id == id)
            .expect("the caller iterates over registered projects");

        let is_selected = selected == Some(id);
        let style = if is_selected {
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
        };

        let has_panes = self
            .state
            .panes_for(id)
            .iter()
            .any(|pane| pane.parent.is_none());

        // The bar runs the full width of the list rather than the width of the
        // name: a highlight that stops where a short name does reads as part
        // of the name instead of as the row being selected.
        if is_selected {
            let blanks = " ".repeat(area.width as usize);
            write(buf, area, area.x, y, &blanks, style);
        }

        // Reserved for the federation slice: green when the device hosting
        // this project is reachable, red when it is not. Everything is local
        // in Slice 1, so it stays dim.
        let dot_style = Style::default().fg(Color::DarkGray);
        let dot_style = if is_selected {
            dot_style.add_modifier(Modifier::REVERSED)
        } else {
            dot_style
        };
        let x = write(
            buf,
            area,
            area.x,
            y,
            twisty(has_panes, self.state.is_project_collapsed(id)),
            style,
        );
        let x = write(buf, area, x, y, DOT, dot_style);

        let room = (area.x + area.width).saturating_sub(x + 1) as usize;
        write(buf, area, x + 1, y, &truncate(&project.name, room), style);
    }

    /// Draws one pane row `indent` columns in from the sidebar's edge.
    ///
    /// A child sits two columns further in than its parent, which is the only
    /// difference between drawing a top-level pane and one of its subagents.
    fn render_pane(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        pane: &Pane,
        focused: Option<PaneId>,
        indent: u16,
    ) {
        // A tombstone has no process behind it, so it can never be the row
        // the user is focused on.
        let is_focused = !pane.closed && focused == Some(pane.id);
        let marker = if is_focused { FOCUS } else { " " };
        let style = if is_focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else if pane.closed {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };

        let dot_style = if pane.closed {
            Style::default().fg(Color::DarkGray)
        } else {
            status_style(pane.status)
        };

        let has_children = !self.state.children_of(pane.id).is_empty();
        let twisty = twisty(has_children, self.state.is_pane_collapsed(pane.id));

        // Twisty, focus marker and status dot sit in adjacent columns, so a
        // row's title starts one column after the dot whatever its depth —
        // the same shape a project row has.
        let x = write(buf, area, area.x + indent, y, twisty, style);
        let x = write(buf, area, x, y, marker, style);
        let x = write(buf, area, x, y, DOT, dot_style);

        // The outcome glyph lives in the row's last column, so a long title is
        // cut short before it rather than drawn under it. A row with no glyph
        // gives that column back to the title.
        let glyph = outcome(pane);
        let right = (area.x + area.width).saturating_sub(2);
        let room = if glyph.is_some() {
            right.saturating_sub(x + 1) as usize
        } else {
            (area.x + area.width).saturating_sub(x + 1) as usize
        };

        write(buf, area, x + 1, y, &truncate(&pane.title, room), style);

        if let Some((glyph, glyph_style)) = glyph {
            write(buf, area, right + 1, y, glyph, glyph_style);
        }
    }
}

#[cfg(test)]
mod tests;
