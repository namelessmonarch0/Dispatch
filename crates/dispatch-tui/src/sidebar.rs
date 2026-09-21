//! The project sidebar.
//!
//! Lists projects and, under each, its panes. The status column on the left of
//! every project is reserved for the federation slice, which lights it green
//! or red per device; in Slice 1 every project is local and the dot is dim.

use dispatch_config::HarnessRegistry;
use dispatch_config::harness::DEFAULT_ICON;
use dispatch_core::{AppState, DeviceId, Pane, PaneId, PaneStatus, ProjectId, ProjectSource};
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

/// What a pane is doing, one glyph per state, drawn in the row's last column.
///
/// Public because they are how a row is recognised as a pane's from outside:
/// nothing else in the list carries one.
///
/// A dot beside an outcome column said the same thing twice: the dot claimed
/// something was happening and the glyph beside it said how it had ended.
pub const STARTING: &str = "\u{f252}";

/// Running.
pub const RUNNING: &str = "\u{f04b}";

/// Waiting on its user.
pub const IDLE: &str = "\u{f04c}";

/// Exited cleanly.
pub const DONE: &str = "\u{f00c}";

/// Exited with a failure.
pub const FAILED: &str = "\u{f00d}";

/// Closed, and still listed only because something under it is not.
pub const CLOSED: &str = "\u{f05e}";

/// The twisty of a node whose children are drawn.
///
/// Nerd Font carets rather than the geometric triangles: those are
/// East-Asian-ambiguous, and a terminal that renders one two cells wide pushes
/// the rest of the row out of line with every other row.
const OPEN: &str = "\u{f0d7}";

/// The twisty of a node whose children are hidden.
const SHUT: &str = "\u{f0da}";

/// Drawn where a node has no children to hide.
const LEAF: &str = " ";

/// The mark beside the folder of a project kept in a git repository.
///
/// Its own column, blank on a project that is a plain directory, so both kinds
/// of row line their names up with each other.
pub const REPOSITORY: &str = "\u{e725}";

/// A project whose panes are listed below it.
pub const OPEN_FOLDER: &str = "\u{f115}";

/// A project whose panes are folded away, or which has none.
pub const SHUT_FOLDER: &str = "\u{f07b}";

/// The mark on a machine's row.
pub const MACHINE: &str = "\u{f109}";

/// What a machine's row says when its connection is down.
const UNREACHABLE: &str = "unreachable";

/// How far a row's text sits from the start of that row.
///
/// The same for both kinds of row, which is what lines a project's name up
/// with the titles beneath it. A project spends those columns on its twisty,
/// the git mark and its folder; a pane spends them on its twisty, its focus
/// marker and the icon of the harness running in it.
const NAME: u16 = 4;

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
    harnesses: Option<&'a HarnessRegistry>,
}

impl<'a> Sidebar<'a> {
    /// Creates a sidebar for `state`.
    #[must_use]
    pub fn new(state: &'a AppState) -> Self {
        Self {
            state,
            harnesses: None,
        }
    }

    /// Marks each pane with the icon of the harness running in it.
    ///
    /// Optional, because the registry is the client's: a sidebar drawn without
    /// one marks every pane generically rather than refusing to draw.
    #[must_use]
    pub fn with_harnesses(mut self, harnesses: &'a HarnessRegistry) -> Self {
        self.harnesses = Some(harnesses);
        self
    }

    /// The mark for the harness running in `pane`.
    fn icon(&self, pane: &Pane) -> &str {
        self.harnesses
            .and_then(|harnesses| harnesses.get(pane.harness.as_str()))
            .map_or(DEFAULT_ICON, dispatch_config::HarnessDef::icon)
    }
}

/// The mark for a project's folder, by whether you are looking inside it.
fn folder_icon(open: bool) -> &'static str {
    if open { OPEN_FOLDER } else { SHUT_FOLDER }
}

/// The mark beside that folder, by what kind of directory it is.
fn source_icon(source: &ProjectSource) -> &'static str {
    match source {
        ProjectSource::LocalDir => " ",
        ProjectSource::GitRepo { .. } => REPOSITORY,
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

/// What a pane's state looks like, and the colour it is drawn in.
///
/// A tombstone reports being closed whatever its process did: a closed row
/// with live work beneath it has to look different from one that is merely
/// finished.
fn state_glyph(pane: &Pane) -> (&'static str, Style) {
    if pane.closed {
        return (CLOSED, Style::default().fg(Color::DarkGray));
    }

    match pane.status {
        PaneStatus::Starting => (STARTING, Style::default().fg(Color::Yellow)),
        PaneStatus::Running => (RUNNING, Style::default().fg(Color::Green)),
        PaneStatus::Idle => (IDLE, Style::default().fg(Color::Blue)),
        // A pane that exited stays listed until it is closed, so it has to be
        // visibly different from one that is still working.
        PaneStatus::Exited(0) => (DONE, Style::default().fg(Color::DarkGray)),
        PaneStatus::Exited(_) => (FAILED, Style::default().fg(Color::Red)),
    }
}

/// One line of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A machine, when there is more than one.
    Device(DeviceId),
    /// A project heading, drawn `indent` columns in from the list's edge.
    Project(ProjectId, u16),
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

    // One machine draws no machine row: it would say what the user already
    // knows and indent everything under it to say it.
    let federated = state.devices().len() > 1;
    let step = if federated { 2 } else { 0 };

    let devices: Vec<Option<DeviceId>> = if federated {
        state.devices().iter().map(|d| Some(d.id)).collect()
    } else {
        vec![None]
    };

    for device in devices {
        if let Some(device) = device {
            rows.push(Row::Device(device));

            if state.is_device_collapsed(device) {
                continue;
            }
        }

        for project in state.projects() {
            if device.is_some_and(|device| project.device != device) {
                continue;
            }

            rows.push(Row::Project(project.id, step));

            if state.is_project_collapsed(project.id) {
                continue;
            }

            for pane in state.panes_for(project.id) {
                if pane.parent.is_some() {
                    // Drawn under its parent, below, not in its own right.
                    continue;
                }

                rows.push(Row::Pane(pane.id, step + 2));

                if state.is_pane_collapsed(pane.id) {
                    continue;
                }

                for child in state.children_of(pane.id) {
                    rows.push(Row::Pane(child.id, step + 4));
                }
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
                Row::Device(id) => self.render_device(buf, area, y, id),
                Row::Project(id, indent) => self.render_project(buf, area, y, id, indent, selected),
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
    /// A machine's row. Folding is all there is to do on one.
    Device(DeviceId),
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
        Row::Device(id) => Some(Hit::Device(id)),
        Row::Project(id, _) => Some(Hit::Project(id)),
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
    /// Draws one project row `indent` columns in from the sidebar's edge.
    ///
    /// The indent is nonzero only under a machine row, which is what pushes a
    /// project's own columns — its twisty, its git mark, its folder, its name
    /// — in to sit under that machine rather than under the frame.
    fn render_project(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        id: ProjectId,
        indent: u16,
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
        // of the name instead of as the row being selected. It starts at the
        // row's own indent, not the frame's edge, so a selected project under
        // a machine does not paint over that machine's row.
        if is_selected {
            let width = (area.width.saturating_sub(indent)) as usize;
            let blanks = " ".repeat(width);
            write(buf, area, area.x + indent, y, &blanks, style);
        }

        // Open only when there is something inside to be looking at: a folder
        // standing open on a project with no panes promises nothing.
        let collapsed = self.state.is_project_collapsed(id);
        let open = has_panes && !collapsed;

        write(
            buf,
            area,
            area.x + indent,
            y,
            twisty(has_panes, collapsed),
            style,
        );
        write(
            buf,
            area,
            area.x + indent + 1,
            y,
            source_icon(&project.source),
            style,
        );
        write(buf, area, area.x + indent + 2, y, folder_icon(open), style);

        let name_x = area.x + indent + NAME;
        let room = (area.x + area.width).saturating_sub(name_x) as usize;
        write(buf, area, name_x, y, &truncate(&project.name, room), style);
    }

    /// Draws one machine's row.
    ///
    /// Dim and labelled when its connection is down: its agents are still
    /// running, so the row stays, but a row that looks live while nothing can
    /// reach it is worse than no row.
    fn render_device(&self, buf: &mut Buffer, area: Rect, y: u16, id: DeviceId) {
        let Some(device) = self.state.device(id) else {
            return;
        };

        let style = if device.reachable {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let has_projects = self
            .state
            .projects()
            .iter()
            .any(|project| project.device == id);

        write(
            buf,
            area,
            area.x,
            y,
            twisty(has_projects, self.state.is_device_collapsed(id)),
            style,
        );
        write(buf, area, area.x + 1, y, MACHINE, style);

        let name = if device.reachable {
            device.name.clone()
        } else {
            format!("{} — {UNREACHABLE}", device.name)
        };
        let room = (area.x + area.width).saturating_sub(area.x + NAME) as usize;
        write(buf, area, area.x + NAME, y, &truncate(&name, room), style);
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

        let has_children = !self.state.children_of(pane.id).is_empty();
        let twisty = twisty(has_children, self.state.is_pane_collapsed(pane.id));

        write(buf, area, area.x + indent, y, twisty, style);
        write(buf, area, area.x + indent + 1, y, marker, style);
        write(buf, area, area.x + indent + 2, y, self.icon(pane), style);

        // The state glyph owns the row's last column, so a long title is cut
        // short before it rather than drawn under it.
        let (glyph, glyph_style) = state_glyph(pane);
        let state_x = (area.x + area.width).saturating_sub(1);

        let title_x = area.x + indent + NAME;
        let room = state_x.saturating_sub(title_x + 1) as usize;
        write(buf, area, title_x, y, &truncate(&pane.title, room), style);

        write(buf, area, state_x, y, glyph, glyph_style);
    }
}

#[cfg(test)]
mod tests;
