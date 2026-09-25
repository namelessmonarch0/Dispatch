//! The project sidebar.
//!
//! With more than one machine, the list is split into a section per machine,
//! each headed by the machine's name and scrolled on its own.

use std::collections::HashMap;

use crate::theme::Theme;
use dispatch_config::HarnessRegistry;
use dispatch_config::harness::DEFAULT_ICON;
use dispatch_core::{
    AppState, DeviceId, Pane, PaneId, PaneStatus, Project, ProjectId, ProjectSource,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Width the sidebar asks for.
///
/// Every icon is followed by a blank column, because a Nerd Font glyph drawn
/// wider than its cell otherwise runs into whatever is next to it. Those gaps
/// cost two columns, and the sidebar is two wider so titles keep their room.
pub const WIDTH: u16 = 34;

/// What the sidebar is called when it is one list.
const TITLE: &str = "Projects";

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

/// Waiting on a decision only the user can make.
pub const BLOCKED: &str = "\u{f071}";

/// Exited cleanly.
pub const DONE: &str = "\u{f00c}";

/// Exited with a failure.
pub const FAILED: &str = "\u{f00d}";

/// Closed, and still listed only because something under it is not.
pub const CLOSED: &str = "\u{f05e}";

/// Finished while the user was looking elsewhere, until they look.
pub const UNSEEN: &str = "\u{f058}";

/// A working pane's glyph, one frame per tenth of a second.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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

/// The mark of a project kept in a git repository.
pub const REPOSITORY: &str = "\u{e725}";

/// A plain directory's mark.
pub const SHUT_FOLDER: &str = "\u{f07b}";

/// What a machine's name says when its connection is down.
const UNREACHABLE: &str = "unreachable";

/// How far a row's text sits from its start: twisty, blank, icon, blank.
const NAME: u16 = 4;

/// The area rows are drawn in: everything inside the frame.
///
/// Shared by rendering and hit testing, so a click lands on the row the eye
/// sees rather than one line off it.
#[must_use]
fn inner(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

/// Renders the project list.
#[derive(Debug, Clone, Copy)]
pub struct Sidebar<'a> {
    state: &'a AppState,
    harnesses: Option<&'a HarnessRegistry>,
    theme: Theme,
    scroll: Option<&'a Scroll>,
    spinner: Option<usize>,
}

impl<'a> Sidebar<'a> {
    /// Creates a sidebar for `state`.
    #[must_use]
    pub fn new(state: &'a AppState) -> Self {
        Self {
            state,
            harnesses: None,
            theme: Theme::fallback(),
            scroll: None,
            spinner: None,
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

    /// Draws in `theme` rather than the built-in one.
    #[must_use]
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }

    /// Draws each machine's section scrolled as `scroll` says.
    #[must_use]
    pub fn with_scroll(mut self, scroll: &'a Scroll) -> Self {
        self.scroll = Some(scroll);
        self
    }

    /// Draws working panes with spinner frame `frame`; `None` draws the still
    /// play glyph, as with motion off.
    #[must_use]
    pub fn with_spinner(mut self, frame: Option<usize>) -> Self {
        self.spinner = frame;
        self
    }

    /// A selected or focused row: the tint, and the palette's text on it.
    ///
    /// Text in the terminal's own colour would not do: the tint is the
    /// fallback's when the terminal did not say what its colours are, and a
    /// light theme's dark text is unreadable on that.
    fn tinted(&self) -> Style {
        Style::default().bg(self.theme.tint).fg(self.theme.text)
    }

    /// The mark for the harness running in `pane`.
    fn icon(&self, pane: &Pane) -> &str {
        self.harnesses
            .and_then(|harnesses| harnesses.get(pane.harness.as_str()))
            .map_or(DEFAULT_ICON, dispatch_config::HarnessDef::icon)
    }
}

/// A project's mark: git for a repository, a folder for a plain directory.
///
/// One mark rather than a folder with a git mark beside it: the twisty
/// already says whether the folder is open, and a second glyph there was one
/// more thing to collide.
///
/// A known branch makes a project a repository whatever its source says. The
/// source records only whether the project's own root holds `.git`, while
/// the branch is found by walking up, so a directory inside a repository —
/// or one `git init` reached after it was opened — has a branch line under
/// it, and a folder above that line would contradict it.
fn source_icon(project: &Project) -> &'static str {
    match project.source {
        ProjectSource::GitRepo { .. } => REPOSITORY,
        ProjectSource::LocalDir if project.branch.is_some() => REPOSITORY,
        ProjectSource::LocalDir => SHUT_FOLDER,
    }
}

/// Writes `text` at `(x, y)`, clipped to `area`, and returns the next column.
///
/// Measured in display columns rather than characters, so a wide character
/// takes the two cells it is drawn in rather than pushing the rest of the row
/// out of line.
fn write(buf: &mut Buffer, area: Rect, x: u16, y: u16, text: &str, style: Style) -> u16 {
    let right = area.x + area.width;
    if x >= right || y < area.y || y >= area.y + area.height {
        return x;
    }

    buf.set_stringn(x, y, text, usize::from(right - x), style).0
}

/// Paints the row from `x` to the list's right edge in `style`, under
/// whatever is written on it afterwards.
fn fill(buf: &mut Buffer, area: Rect, x: u16, y: u16, style: Style) {
    for column in x..area.x + area.width {
        if let Some(cell) = buf.cell_mut((column, y)) {
            cell.set_style(style);
        }
    }
}

/// Truncates `text` to `width` display columns, marking the cut with an
/// ellipsis.
///
/// Project names come from directory names and are routinely longer than the
/// sidebar. Public so every cut in the interface is measured the same way:
/// counting characters lets a wide one take two columns it was never given.
#[must_use]
pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }

    let mut kept = String::new();
    let mut used = 0;
    for character in text.chars() {
        let columns = character.width().unwrap_or(0);
        if used + columns > width - 1 {
            break;
        }
        kept.push(character);
        used += columns;
    }
    kept.push('…');
    kept
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
fn state_glyph(
    pane: &Pane,
    unseen: bool,
    spinner: Option<usize>,
    theme: &Theme,
) -> (&'static str, Style) {
    if pane.closed {
        return (CLOSED, Style::default().fg(theme.faded));
    }

    match pane.status {
        PaneStatus::Starting => (STARTING, Style::default().fg(Color::Yellow)),
        PaneStatus::Running => Rollup::Working.glyph(spinner, theme),
        PaneStatus::Blocked => Rollup::Blocked.glyph(spinner, theme),
        PaneStatus::Idle if unseen => Rollup::Done.glyph(spinner, theme),
        PaneStatus::Idle => (IDLE, Style::default().fg(theme.faded)),
        // A pane that exited stays listed until it is closed, so it has to be
        // visibly different from one that is still working.
        PaneStatus::Exited(0) => (DONE, Style::default().fg(theme.faded)),
        PaneStatus::Exited(_) => (FAILED, Style::default().fg(Color::Red)),
    }
}

/// The most urgent thing a group of panes is doing, for a row or a tab that
/// stands for them. Ordered by urgency, so the largest wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rollup {
    /// Something is working.
    Working,
    /// Something finished while the user was looking elsewhere.
    Done,
    /// Something is waiting on the user.
    Blocked,
}

impl Rollup {
    /// The most urgent state among `panes`; `None` when they are all idle,
    /// starting, exited or closed — nothing worth drawing for the group.
    #[must_use]
    pub fn of<'p>(state: &AppState, panes: impl IntoIterator<Item = &'p Pane>) -> Option<Rollup> {
        panes
            .into_iter()
            .filter(|pane| !pane.closed)
            .filter_map(|pane| match pane.status {
                PaneStatus::Blocked => Some(Rollup::Blocked),
                PaneStatus::Idle if state.is_unseen(pane.id) => Some(Rollup::Done),
                PaneStatus::Running => Some(Rollup::Working),
                _ => None,
            })
            .max()
    }

    /// Its glyph and colour.
    #[must_use]
    pub fn glyph(self, spinner: Option<usize>, theme: &Theme) -> (&'static str, Style) {
        match self {
            Rollup::Working => (
                spinner.map_or(RUNNING, |frame| SPINNER[frame % SPINNER.len()]),
                Style::default().fg(Color::Green),
            ),
            Rollup::Done => (UNSEEN, Style::default().fg(theme.accent)),
            Rollup::Blocked => (
                BLOCKED,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        }
    }
}

/// How far each machine's section is scrolled, in rows.
///
/// A machine missing from the map is at its top. Keyed by machine even when
/// the sidebar is one undivided list, so one code path serves both; that
/// list is keyed by its lone machine, or by the nil id before any machine has
/// registered.
pub type Scroll = HashMap<DeviceId, u16>;

/// One line of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row<'a> {
    /// A project heading.
    Project(ProjectId),
    /// A branch within a project: the project's own, or one some of its
    /// panes are working on.
    Branch(ProjectId, &'a str),
    /// A pane, drawn `indent` columns in from the list's edge.
    Pane(PaneId, u16),
}

impl Row<'_> {
    /// Whether this is the row `anchor` names.
    fn is(&self, anchor: Anchor) -> bool {
        match (*self, anchor) {
            (Row::Pane(id, _), Anchor::Pane(wanted)) => id == wanted,
            (Row::Project(id), Anchor::Project(wanted)) => id == wanted,
            _ => false,
        }
    }
}

/// A row the sidebar should keep in view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// The focused pane's row.
    Pane(PaneId),
    /// The selected project's row, when no pane is focused.
    Project(ProjectId),
}

/// Where a top-level pane's row starts: under its project's name.
const PANE: u16 = 4;

/// One machine's share of the sidebar.
struct Section<'a> {
    /// Which offset in [`Scroll`] this section reads.
    key: DeviceId,
    /// The machine named on the section's header, when the sidebar is split.
    device: Option<DeviceId>,
    /// The line naming it: the top border for the first, a divider after.
    header: u16,
    /// Where its rows are drawn.
    body: Rect,
    /// Every row it has, drawn or not.
    rows: Vec<Row<'a>>,
    /// How many of `rows` are scrolled away above the body.
    offset: usize,
}

impl<'a> Section<'a> {
    /// The rows actually drawn, with the line each is drawn on.
    fn visible(&self) -> impl Iterator<Item = (u16, &Row<'a>)> {
        (self.body.y..self.body.y + self.body.height).zip(self.rows.iter().skip(self.offset))
    }

    /// How many rows are hidden below the body.
    fn below(&self) -> usize {
        self.rows
            .len()
            .saturating_sub(self.offset + usize::from(self.body.height))
    }
}

/// Every section the sidebar at `area` is split into, top to bottom.
///
/// Rendering, hit testing and scrolling all walk this, so a click can only
/// ever land on a row that is actually drawn — which is what makes folding
/// and scrolling safe: a hidden row is missing from all of them at once.
fn sections<'a>(state: &'a AppState, area: Rect, scroll: &Scroll) -> Vec<Section<'a>> {
    let inner = inner(area);
    let devices = state.devices();

    // One machine is one list, with no name on it: a line naming this
    // machine would say what the user already knows.
    if devices.len() <= 1 {
        let key = devices
            .first()
            .map_or_else(DeviceId::nil, |device| device.id);
        return vec![section(key, None, area.y, inner, rows(state, None), scroll)];
    }

    let dividers = u16::try_from(devices.len() - 1).unwrap_or(u16::MAX);
    let weights: Vec<u32> = devices
        .iter()
        .map(|device| open_panes(state, device.id).saturating_add(1))
        .collect();
    let folded: Vec<bool> = devices
        .iter()
        .map(|device| state.is_device_collapsed(device.id))
        .collect();
    let heights = section_heights(&weights, &folded, inner.height.saturating_sub(dividers));

    let mut sections = Vec::new();
    let mut y = inner.y;

    for (index, (device, height)) in devices.iter().zip(heights).enumerate() {
        let header = if index == 0 {
            area.y
        } else {
            // A divider that would land on the bottom border has no room.
            if y >= inner.y + inner.height {
                break;
            }
            y += 1;
            y - 1
        };

        let body = Rect::new(inner.x, y, inner.width, height);
        y += height;

        let rows = if folded[index] {
            Vec::new()
        } else {
            rows(state, Some(device.id))
        };
        sections.push(section(
            device.id,
            Some(device.id),
            header,
            body,
            rows,
            scroll,
        ));
    }

    sections
}

/// A section, its offset brought back inside its rows.
fn section<'a>(
    key: DeviceId,
    device: Option<DeviceId>,
    header: u16,
    body: Rect,
    rows: Vec<Row<'a>>,
    scroll: &Scroll,
) -> Section<'a> {
    let furthest = rows.len().saturating_sub(usize::from(body.height));
    let offset = usize::from(scroll.get(&key).copied().unwrap_or(0)).min(furthest);

    Section {
        key,
        device,
        header,
        body,
        rows,
        offset,
    }
}

/// Brings every section's scroll back inside its rows and, when `anchor` is
/// given, scrolls the section holding that row just far enough to show it.
///
/// The caller passes an anchor only when the focus or the selection has
/// moved: anchoring every frame would undo a wheel scroll as fast as it
/// happened.
pub fn settle(state: &AppState, area: Rect, scroll: &mut Scroll, anchor: Option<Anchor>) {
    let found: Vec<(DeviceId, usize, Option<usize>, usize)> = sections(state, area, scroll)
        .iter()
        .map(|section| {
            let at = anchor.and_then(|anchor| section.rows.iter().position(|row| row.is(anchor)));
            (
                section.key,
                section.offset,
                at,
                usize::from(section.body.height),
            )
        })
        .collect();

    for (key, offset, at, height) in found {
        let offset = match at {
            Some(at) if height > 0 && at < offset => at,
            Some(at) if height > 0 && at >= offset + height => at + 1 - height,
            _ => offset,
        };
        scroll.insert(key, u16::try_from(offset).unwrap_or(u16::MAX));
    }
}

/// How many panes `device` has open at the top level: what its section's
/// share of the height is weighed by.
fn open_panes(state: &AppState, device: DeviceId) -> u32 {
    let count = state
        .projects()
        .iter()
        .filter(|project| project.device == device)
        .flat_map(|project| state.panes_for(project.id))
        .filter(|pane| pane.parent.is_none() && !pane.closed)
        .count();

    u32::try_from(count).unwrap_or(u32::MAX)
}

/// How many rows each section's body gets, out of `height`.
///
/// Folded sections get none. Every other gets two when there is room for
/// that, one when there is not, and otherwise one each to as many as fit, in
/// order. What is left is shared in proportion to `weights`, the rows lost
/// to rounding going to the largest remainders, the higher section first on
/// a tie. Always sums to `height` when anything is unfolded, so the sections
/// fill the frame rather than leaving the blank at the bottom.
fn section_heights(weights: &[u32], folded: &[bool], height: u16) -> Vec<u16> {
    let mut heights = vec![0_u16; weights.len()];
    let open: Vec<usize> = (0..weights.len())
        .filter(|&index| !folded.get(index).copied().unwrap_or(false))
        .collect();
    if open.is_empty() {
        return heights;
    }

    let count = u16::try_from(open.len()).unwrap_or(u16::MAX);
    let floor = if height >= count.saturating_mul(2) {
        2
    } else if height >= count {
        1
    } else {
        0
    };

    if floor == 0 {
        for &index in open.iter().take(usize::from(height)) {
            heights[index] = 1;
        }
        return heights;
    }

    for &index in &open {
        heights[index] = floor;
    }

    let spare = u64::from(height - floor * count);
    let total: u64 = open
        .iter()
        .map(|&index| u64::from(weights[index].max(1)))
        .sum();

    let mut given = 0;
    let mut remainders = Vec::with_capacity(open.len());
    for &index in &open {
        let share = spare * u64::from(weights[index].max(1));
        let whole = share / total;
        heights[index] += u16::try_from(whole).unwrap_or(u16::MAX);
        given += whole;
        remainders.push((share % total, index));
    }

    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let leftover = usize::try_from(spare - given).unwrap_or(0);
    for &(_, index) in remainders.iter().take(leftover) {
        heights[index] += 1;
    }

    heights
}

/// Every row of one machine's projects, or of every project when `device` is
/// `None`, top to bottom.
///
/// `panes_for` is unfiltered, so a closed pane still appears here when it is
/// kept as a tombstone for live children below it — the sidebar is where that
/// row earns its keep.
///
/// One level of children, deliberately: at the default `max_depth` of 1 a
/// subagent cannot delegate, so one level is the whole tree. Raise that cap
/// and a subagent's own subagent is tracked in state — the daemon owns it,
/// `^a s` can focus it, and closing its ancestors will not delete it — but it
/// has no row here. Drawing an arbitrary depth in a column this narrow needs
/// a shape nobody has designed yet, so the honest thing is to say where the
/// drawing stops rather than imply it does not.
fn rows(state: &AppState, device: Option<DeviceId>) -> Vec<Row<'_>> {
    let mut rows = Vec::new();

    for project in state.projects() {
        if device.is_some_and(|device| project.device != device) {
            continue;
        }

        rows.push(Row::Project(project.id));

        let collapsed = state.is_project_collapsed(project.id);
        let top: Vec<&Pane> = state
            .panes_for(project.id)
            .into_iter()
            .filter(|pane| pane.parent.is_none())
            .collect();

        let Some(own) = project.branch.as_deref() else {
            if !collapsed {
                for pane in &top {
                    push_pane(state, &mut rows, pane, PANE);
                }
            }
            continue;
        };

        // The project's own branch stays when the project is folded: it is
        // part of what the project is, not one of the things folding hides.
        rows.push(Row::Branch(project.id, own));
        if collapsed {
            continue;
        }

        // A pane whose branch is not known yet is shown with the project
        // rather than held back until it is.
        for pane in top
            .iter()
            .filter(|pane| pane.branch.as_deref().is_none_or(|branch| branch == own))
        {
            push_pane(state, &mut rows, pane, PANE);
        }

        // Every other branch, in the order its first pane was opened.
        let mut others: Vec<&str> = Vec::new();
        for pane in &top {
            if let Some(branch) = pane.branch.as_deref()
                && branch != own
                && !others.contains(&branch)
            {
                others.push(branch);
            }
        }

        for branch in others {
            rows.push(Row::Branch(project.id, branch));
            for pane in top
                .iter()
                .filter(|pane| pane.branch.as_deref() == Some(branch))
            {
                push_pane(state, &mut rows, pane, PANE);
            }
        }
    }

    rows
}

/// A top-level pane's row, and its subagents' beneath it unless it is
/// folded.
///
/// The subagents follow their parent whatever branch they are on: the tree
/// is what the sidebar is, and grouping is for the panes at its top.
fn push_pane<'a>(state: &'a AppState, rows: &mut Vec<Row<'a>>, pane: &Pane, indent: u16) {
    rows.push(Row::Pane(pane.id, indent));

    if state.is_pane_collapsed(pane.id) {
        return;
    }
    for child in state.children_of(pane.id) {
        rows.push(Row::Pane(child.id, indent + 2));
    }
}

/// Draws the line between two machines' sections across the frame.
fn divider(buf: &mut Buffer, area: Rect, y: u16, style: Style) {
    let right = area.x + area.width - 1;

    for x in area.x..=right {
        let symbol = if x == area.x {
            "├"
        } else if x == right {
            "┤"
        } else {
            "─"
        };
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(symbol);
            cell.set_style(style);
        }
    }
}

/// The text counting rows hidden below `section`, for the line after it.
fn below_label(section: &Section<'_>) -> Option<String> {
    let below = section.below();
    (below > 0).then(|| format!(" ↓ {below} "))
}

impl Widget for Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let edge = Style::default().fg(self.theme.faded);
        Block::bordered().border_style(edge).render(area, buf);

        let empty = Scroll::new();
        let sections = sections(self.state, area, self.scroll.unwrap_or(&empty));
        let selected = self.state.selected_project();
        let focused = self.state.focused_pane();

        for (index, section) in sections.iter().enumerate() {
            if index > 0 {
                divider(buf, area, section.header, edge);
            }
            // The section above's `↓` count is drawn at the right end of this
            // same line, so the name leaves it room.
            let reserve = index
                .checked_sub(1)
                .and_then(|previous| below_label(&sections[previous]))
                .map_or(0, |label| label.width() + 1);
            self.render_label(buf, area, section, reserve);

            for (y, row) in section.visible() {
                match *row {
                    Row::Project(id) => self.render_project(buf, section.body, y, id, selected),
                    Row::Branch(_, branch) => self.render_branch(buf, section.body, y, branch),
                    Row::Pane(id, indent) => {
                        let pane = self
                            .state
                            .pane(id)
                            .expect("every row names a pane that is still in state");
                        self.render_pane(buf, section.body, y, pane, focused, indent);
                    }
                }
            }
        }

        // Counted on the line after each section — the next one's name line,
        // or the bottom border — right-aligned, one dash in from the corner.
        for section in &sections {
            if let Some(label) = below_label(section) {
                let y = section.body.y + section.body.height;
                let width = u16::try_from(label.width()).unwrap_or(u16::MAX);
                let x = (area.x + area.width).saturating_sub(2 + width);
                write(buf, area, x, y, &label, edge);
            }
        }
    }
}

/// What sits under a click on the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The line naming a machine. Folding is all there is to do on one.
    Device(DeviceId),
    /// A project heading. The whole row is its control: there is no pane on it
    /// to focus, so a click both selects the project and folds its panes.
    Project(ProjectId),
    /// The twisty of a pane that has children.
    Twisty(PaneId),
    /// The rest of a live pane's row.
    Pane(PaneId),
}

/// What, if anything, sits at `(x, y)` in a sidebar drawn at `area`,
/// scrolled by `scroll`.
///
/// The sidebar has no keyboard focus of its own, so a pointer is the only way
/// to pick one row out of the list; this is what lets a click reach a pane
/// the tiled grid does not currently show. A closed pane's tombstone row
/// answers `Hit::Twisty` over its twisty and nothing elsewhere: there is no
/// process left to focus, but the children it is still holding can be folded
/// away. A machine's name line answers for the machine along its whole
/// length, frame included — which, for the first machine, is the top border.
#[must_use]
pub fn hit_test(state: &AppState, area: Rect, scroll: &Scroll, x: u16, y: u16) -> Option<Hit> {
    if x < area.x || x >= area.x + area.width || y < area.y || y >= area.y + area.height {
        return None;
    }

    let inner = inner(area);

    for section in sections(state, area, scroll) {
        if let Some(device) = section.device
            && y == section.header
        {
            return Some(Hit::Device(device));
        }

        if x < inner.x || x >= inner.x + inner.width {
            continue;
        }

        if let Some((_, row)) = section.visible().find(|(row_y, _)| *row_y == y) {
            return hit_row(state, section.body, row, x);
        }
    }

    None
}

/// What a click at column `x` on `row` means.
fn hit_row(state: &AppState, body: Rect, row: &Row<'_>, x: u16) -> Option<Hit> {
    match *row {
        Row::Project(id) | Row::Branch(id, _) => Some(Hit::Project(id)),
        Row::Pane(id, indent) => {
            let pane = state.pane(id)?;

            // A blank twisty column is not a control, so a click there falls
            // through to the row it is part of.
            if x == body.x + indent && !state.children_of(id).is_empty() {
                return Some(Hit::Twisty(id));
            }

            (!pane.closed).then_some(Hit::Pane(id))
        }
    }
}

/// Which machine's section holds `(x, y)`, for the wheel to scroll.
///
/// Only a section's rows count: the frame and the lines naming machines
/// belong to no one section's scroll.
#[must_use]
pub fn section_at(
    state: &AppState,
    area: Rect,
    scroll: &Scroll,
    x: u16,
    y: u16,
) -> Option<DeviceId> {
    let inner = inner(area);
    if x < inner.x || x >= inner.x + inner.width {
        return None;
    }

    sections(state, area, scroll)
        .into_iter()
        .find(|section| y >= section.body.y && y < section.body.y + section.body.height)
        .map(|section| section.key)
}

impl Sidebar<'_> {
    /// Draws one project row, at the left edge of its section.
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

        let x = area.x;
        let is_selected = selected == Some(id);
        let style = if is_selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        // The tint runs the full width of the list rather than the width of
        // the name: a highlight that stops where a short name does reads as
        // part of the name. It sets the text colour too, which everything
        // written on the row after it keeps.
        if is_selected {
            fill(buf, area, x, y, self.tinted());
        }

        let has_panes = self
            .state
            .panes_for(id)
            .iter()
            .any(|pane| pane.parent.is_none());
        let collapsed = self.state.is_project_collapsed(id);

        write(buf, area, x, y, twisty(has_panes, collapsed), style);
        write(buf, area, x + 2, y, source_icon(project), style);

        // A folded project stands for its panes, so it carries the most
        // urgent of their states where a pane row carries its own; an open
        // one leaves that to the rows below.
        let rollup = collapsed
            .then(|| Rollup::of(self.state, self.state.panes_for(id)))
            .flatten();
        let state_x = (area.x + area.width).saturating_sub(2);

        let name_x = x + NAME;
        let right = if rollup.is_some() {
            // The name stops a blank short of the glyph, as a pane title does.
            state_x.saturating_sub(1)
        } else {
            area.x + area.width
        };
        let room = right.saturating_sub(name_x) as usize;
        write(buf, area, name_x, y, &truncate(&project.name, room), style);

        if let Some(rollup) = rollup {
            let (glyph, glyph_style) = rollup.glyph(self.spinner, &self.theme);
            write(buf, area, state_x, y, glyph, glyph_style);
        }
    }

    /// Draws a branch line, faded, where its project's name starts.
    fn render_branch(&self, buf: &mut Buffer, area: Rect, y: u16, branch: &str) {
        let x = area.x + NAME;
        let room = (area.x + area.width).saturating_sub(x) as usize;

        write(
            buf,
            area,
            x,
            y,
            &truncate(branch, room),
            Style::default().fg(self.theme.faded),
        );
    }

    /// Writes a section's name onto the line that heads it, followed by how
    /// many of its rows are scrolled away above, leaving `reserve` columns
    /// free at the right for the count of the section above it.
    ///
    /// Dim and labelled when the machine's connection is down: its agents are
    /// still running, so the section stays, but a name that looks live while
    /// nothing can reach it is worse than no name.
    fn render_label(&self, buf: &mut Buffer, area: Rect, section: &Section<'_>, reserve: usize) {
        // Inside the corners, with a blank either side, the way a frame's own
        // title sits.
        let line = Rect::new(area.x + 1, section.header, area.width.saturating_sub(2), 1);
        let up = if section.offset > 0 {
            format!(" ↑ {}", section.offset)
        } else {
            String::new()
        };
        let room = usize::from(line.width.saturating_sub(2)).saturating_sub(up.width() + reserve);

        let Some(id) = section.device else {
            let x = write(
                buf,
                line,
                line.x,
                line.y,
                &format!(" {TITLE}"),
                Style::default(),
            );
            write(
                buf,
                line,
                x,
                line.y,
                &format!("{up} "),
                Style::default().fg(self.theme.faded),
            );
            return;
        };
        let Some(device) = self.state.device(id) else {
            return;
        };

        let (name, style) = if device.reachable {
            (
                truncate(&device.name, room),
                Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            // The name gives way rather than the word this line exists to
            // show: a real hostname is routinely long enough to push it off.
            let suffix = format!(" — {UNREACHABLE}");
            let name_room = room.saturating_sub(suffix.width());
            (
                format!("{}{suffix}", truncate(&device.name, name_room)),
                Style::default().fg(self.theme.faded),
            )
        };

        // The `↑ n` count is always faded, whatever the name's own style is:
        // the spec draws every hidden-row count the same, on both sides of a
        // section.
        let x = write(buf, line, line.x, line.y, &format!(" {name}"), style);
        write(
            buf,
            line,
            x,
            line.y,
            &format!("{up} "),
            Style::default().fg(self.theme.faded),
        );
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
        let style = if is_focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else if pane.closed {
            Style::default().fg(self.theme.faded)
        } else {
            Style::default()
        };

        let x = area.x + indent;
        if is_focused {
            fill(buf, area, x, y, self.tinted());
        }

        let has_children = !self.state.children_of(pane.id).is_empty();
        let twisty = twisty(has_children, self.state.is_pane_collapsed(pane.id));

        write(buf, area, x, y, twisty, style);
        write(buf, area, x + 2, y, self.icon(pane), style);

        // Two columns in from the frame, so the blank beside it keeps a glyph
        // drawn wider than its cell off the border. The title stops a blank
        // short of it.
        let (glyph, glyph_style) = state_glyph(
            pane,
            self.state.is_unseen(pane.id),
            self.spinner,
            &self.theme,
        );
        let state_x = (area.x + area.width).saturating_sub(2);

        let title_x = x + NAME;
        let room = state_x.saturating_sub(title_x + 1) as usize;
        write(buf, area, title_x, y, &truncate(&pane.title, room), style);

        write(buf, area, state_x, y, glyph, glyph_style);
    }
}

#[cfg(test)]
mod tests;
