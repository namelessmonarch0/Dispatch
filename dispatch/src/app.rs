//! The running application: state, panes, and the event loop.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dispatch_client::Client;
use dispatch_config::{HarnessRegistry, Launch};
use dispatch_core::{
    AppState, Device, DeviceId, HarnessId, Pane as CorePane, PaneId, PaneStatus, Project,
    ProjectId, ProjectSource, RequestId,
};
use dispatch_layout::{tile, tile_zoomed};
use dispatch_proto::{ClientMessage, DelegateOutcome, PaneUpdate, ServerMessage};
use dispatch_pty::{
    KeyEncoder, MouseEncoder, MouseInput, PtySession, RunState, Screen, ScreenReader, ScrollTo,
    Size, TitleScanner,
};

use crate::approval::Approval;
use crate::backend::{Backend, RemotePane};
use dispatch_tui::browser::Browser;
use dispatch_tui::input::{
    Action, Direction, Event, InputRouter, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use dispatch_tui::{Item, PaneWidget, Picker, Sidebar, sidebar};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Widget};

/// How many panes are tiled at once.
///
/// Four is the most that stays readable in a terminal: past it every pane is
/// too narrow for a wrapped line of code and too short for a prompt plus its
/// answer. Panes beyond the fourth are not hidden — they go on the next tab,
/// and all of them are always listed in the sidebar.
const PANES_PER_TAB: usize = 4;

/// The border drawn around one pane.
///
/// A plain thin line, brighter on the focused pane. Rounded corners read as
/// softer than the square ones the sidebar and status row use, which is enough
/// to tell a pane's edge from the frame of the interface around it.
fn pane_block(focused: bool) -> Block<'static> {
    let colour = if focused {
        Color::White
    } else {
        Color::DarkGray
    };

    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(colour))
}

/// What to write on a pane's border.
///
/// The pane's own title when it has set one, so a bordered pane says which
/// agent it is without costing a row of output for a header.
fn pane_title(state: &AppState, id: PaneId) -> String {
    state
        .pane(id)
        .map(|pane| format!(" {} ", pane.title))
        .unwrap_or_default()
}

/// Which picker is open, decoupled from the picker itself so a selection can
/// be read out before the overlay is closed. `Overlay::Approval` has no
/// picker of its own and so no `OverlayKind` — a decision there is acted on
/// directly rather than looked up by kind.
#[derive(Debug, Clone, Copy)]
enum OverlayKind {
    Harness,
    Project,
    Register,
}

/// One delegation request waiting on a decision.
///
/// Held only as the daemon announced it: Dispatch keeps no ledger of its own,
/// so a reattach or a resolution from elsewhere is always taken as the truth.
struct PendingRequest {
    /// Which request, for the `DelegateDecision` that eventually answers it.
    request: RequestId,
    /// The pane asking.
    parent: PaneId,
    /// Its project, for display.
    project: ProjectId,
    /// Which harness would run.
    harness: String,
    /// What it would be asked to do, verbatim.
    task: String,
    /// How deep the asking pane already is.
    depth: u8,
}

/// A title with the agent's own mark taken off the front.
///
/// Agents announce themselves with one: Claude Code's terminal title is
/// "✳ Claude Code". The sidebar draws the harness's icon beside the row
/// already, so the mark in the title is the same fact twice — and two marks in
/// a row of four rows reads as a broken glyph rather than as a name.
///
/// Only symbols are taken: a title that opens with a letter, a digit or a path
/// is whatever the agent meant it to be.
fn strip_mark(title: &str) -> &str {
    title
        .trim_start_matches(|c: char| {
            matches!(c,
                '\u{2190}'..='\u{2bff}'      // arrows, dingbats, geometric shapes
                | '\u{e000}'..='\u{f8ff}'    // private use: every Nerd Font glyph
                | '\u{1f300}'..='\u{1faff}'  // emoji
            )
        })
        .trim()
}

/// Frame budget. A chatty agent can produce output faster than any terminal
/// can draw it, so redraws are coalesced rather than done per byte.
const FRAME: Duration = Duration::from_millis(16);

/// How much of a task's opening words becomes a subagent's first title.
///
/// The sidebar is [`sidebar::WIDTH`] columns wide and a child row is indented
/// four into it, so anything much longer than this could not be read there in
/// full anyway.
const TITLE_BUDGET: usize = 22;

/// The opening words of a task, for the row of the subagent running it.
///
/// Whole words wherever they fit, because a title cut mid-word reads as damage
/// rather than as brevity; a first word longer than the whole budget is cut, as
/// there is nothing else to fall back on. `None` for a task with no words at
/// all, which is the one case where the harness name is the better title.
///
/// This is a first title only: the title scanner replaces it the moment the
/// agent names itself.
fn task_title(task: &str) -> Option<String> {
    let mut title = String::new();

    for word in task.split_whitespace() {
        let taken = title.chars().count();

        if taken == 0 {
            if word.chars().count() > TITLE_BUDGET {
                let kept: String = word.chars().take(TITLE_BUDGET.saturating_sub(1)).collect();
                return Some(format!("{kept}…"));
            }
            title.push_str(word);
            continue;
        }

        if taken + 1 + word.chars().count() > TITLE_BUDGET {
            title.push('…');
            break;
        }

        title.push(' ');
        title.push_str(word);
    }

    (!title.is_empty()).then_some(title)
}

/// Centres a box for the approval prompt inside `area`.
///
/// Wide enough for a few sentences of task text without cropping the corners
/// off a small terminal.
fn centred_approval(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).clamp(20, 76).min(area.width);
    let height = area.height.saturating_sub(2).clamp(8, 20).min(area.height);

    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// Everything one pane owns.
struct Pane {
    backend: Backend,
    encoder: KeyEncoder,
    mouse: MouseEncoder,
    /// Whether the viewport is scrolled away from the newest output.
    scrolled_back: bool,
    reader: ScreenReader,
    /// Last screen read, redrawn each frame without re-reading when nothing
    /// changed.
    screen: Screen,
    /// Watches the output for the title the child sets for itself.
    titles: TitleScanner,
}

/// What has the keyboard, so a keystroke meant for an agent — or an approval
/// meant for one request — can never land on the wrong thing.
enum Overlay {
    /// A harness to spawn.
    Harness(Picker),
    /// A project to switch to.
    Project(Picker),
    /// A harness to register, found on PATH.
    Register(Picker),
    /// A directory to open as a project.
    Browse(Browser),
    /// A delegation request, shown from the front of `App::pending`.
    Approval {
        /// First line of the task text on screen, for a long one.
        scroll: u16,
    },
}

impl Overlay {
    /// The picker inside, for the variants that have one.
    fn picker(&self) -> Option<&Picker> {
        match self {
            Overlay::Harness(picker) | Overlay::Project(picker) | Overlay::Register(picker) => {
                Some(picker)
            }
            Overlay::Browse(_) | Overlay::Approval { .. } => None,
        }
    }

    /// The picker inside, mutably, for the variants that have one.
    fn picker_mut(&mut self) -> Option<&mut Picker> {
        match self {
            Overlay::Harness(picker) | Overlay::Project(picker) | Overlay::Register(picker) => {
                Some(picker)
            }
            Overlay::Browse(_) | Overlay::Approval { .. } => None,
        }
    }

    /// What kind of choice a picker overlay is making, for the variants that
    /// are one.
    fn kind(&self) -> Option<OverlayKind> {
        match self {
            Overlay::Harness(_) => Some(OverlayKind::Harness),
            Overlay::Project(_) => Some(OverlayKind::Project),
            Overlay::Register(_) => Some(OverlayKind::Register),
            Overlay::Browse(_) | Overlay::Approval { .. } => None,
        }
    }
}

/// What to call the machine Dispatch is running on.
///
/// The hostname, because a fleet of rows all saying "local" names nothing.
/// Asked of the operating system rather than the environment: `$HOSTNAME` is
/// a bash-only convention that bash itself does not export, so zsh, fish and
/// most CI runners never see it.
fn this_machine() -> String {
    dispatch_os::host::hostname()
}

/// One daemon this client is holding.
struct Attachment {
    /// The machine it is, as the sidebar names it.
    device: DeviceId,
    client: Client,
    /// That connection's generation. Per attachment, because one daemon
    /// restarting says nothing about the others.
    generation: u64,
    /// Roots asked of this daemon, so its own reconnect can ask again.
    opened: Vec<PathBuf>,
}

/// Where this Dispatch's agents run.
///
/// Attached is what lets the work outlive the interface; standalone is what
/// makes Dispatch usable with nothing else running, so both are kept.
enum Mode {
    /// The agents are this process's children.
    Standalone,
    /// The agents belong to daemons — one per machine.
    Attached(Vec<Attachment>),
}

/// The application.
pub struct App {
    mode: Mode,
    /// The machine Dispatch itself is, while it runs its own agents.
    ///
    /// `None` from the first `attach` on: the agents are a daemon's from then
    /// on, and every project belongs to the machine that announced it.
    local: Option<DeviceId>,
    /// Where the directory browser starts, when it has not been opened yet.
    ///
    /// The working directory Dispatch was started in, which is the directory
    /// the user is already thinking about.
    browse_from: PathBuf,
    /// Where the kept-projects file lives, when this client keeps one.
    ///
    /// `None` in a test, and in any client told to keep nothing: the list is a
    /// convenience, and a client that cannot write it still runs.
    kept: Option<PathBuf>,
    /// The browser as it was last closed, so reopening it lands where it was.
    browser: Option<Browser>,
    overlay: Option<Overlay>,
    state: AppState,
    panes: HashMap<PaneId, Pane>,
    harnesses: HarnessRegistry,
    router: InputRouter,
    /// Each pane's content rectangle last frame — inside its border.
    ///
    /// This is what a pointer is resolved against and what a pane is resized
    /// to, so it has to be the area the emulator actually owns rather than the
    /// tile drawn around it.
    layout: Vec<(PaneId, Rect)>,
    /// Each pane's tile last frame, border included.
    ///
    /// Only drawing wants this; everything else means [`App::layout`].
    frames: Vec<(PaneId, Rect)>,

    /// Where the sidebar was drawn last frame.
    ///
    /// The sidebar has no keyboard focus of its own, so a click is the only
    /// way to pick one of its rows out of the list — this is what a click is
    /// matched against.
    sidebar_area: Rect,
    /// Subagents the user has opened, so they join the tiled grid.
    ///
    /// Which rows are open is a per-client choice, not a property of the
    /// pane itself — two clients on one fleet can disagree about it, so this
    /// never goes to the daemon.
    expanded: HashSet<PaneId>,
    /// Delegation requests waiting on a decision, oldest first.
    pending: VecDeque<PendingRequest>,
    /// Tasks of requests this client itself approved, until the daemon says
    /// what became of them.
    ///
    /// `decide` pops a request out of `pending` under the keystroke that
    /// answers it, before the daemon's `DelegateResolved` can arrive, so this
    /// is the only place its task would still be when the subagent's pane is
    /// named.
    answered: HashMap<RequestId, String>,
    /// First titles for subagent panes the daemon has approved but not yet
    /// announced, taken from what each was asked to do.
    ///
    /// The daemon resolves a request before it announces the pane, so the
    /// title is known one message before there is a row to put it on.
    child_titles: HashMap<PaneId, String>,
    status: String,
    quit: bool,
    /// A project to reselect once its `ProjectOpened` comes back, keyed by
    /// the device it belongs to, because a reconnect forgot it out from
    /// under a selection that was pointed at it.
    ///
    /// `forget_device` clears the selection before the replay that would
    /// otherwise restore it has arrived, and by the time it does,
    /// `add_project` sees a selection already pointing elsewhere and leaves it
    /// there. Without this, reattaching to a machine mid-session silently
    /// moves the view to whichever project happens to be first — and moves
    /// keystrokes with it, since a pane only gets focus while its project is
    /// selected.
    ///
    /// A remembered intention, not a fact, so it has to stop being true
    /// before what it names does: consumed the moment a matching
    /// `ProjectOpened` restores it; overwritten the next time this same
    /// device reconnects, so a daemon that keeps restarting and minting
    /// fresh `ProjectId`s cannot leave this naming one that will never come
    /// back; cleared by `remove_device`, which took the device away it was
    /// promised to; and cleared by any explicit selection, because the
    /// user's own choice of where to look always outranks a stale one of
    /// ours.
    reopening_selection: Option<(DeviceId, ProjectId)>,
    /// Every `ClientMessage` a decision would have sent, kept only so tests
    /// can tell `a` and `A` apart without a real daemon to send it to.
    #[cfg(test)]
    sent: Vec<ClientMessage>,
}

impl App {
    /// Creates an application that owns its own agents.
    pub fn new(harnesses: HarnessRegistry) -> Self {
        let mut state = AppState::new();

        // Standalone runs its agents itself, but it is still a machine: one
        // code path beats asking "device or not" at every use.
        let local = state.add_device(Device::new(this_machine()));

        Self {
            mode: Mode::Standalone,
            local: Some(local),
            browse_from: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kept: None,
            browser: None,
            overlay: None,
            state,
            panes: HashMap::new(),
            harnesses,
            router: InputRouter::new(),
            frames: Vec::new(),
            layout: Vec::new(),
            sidebar_area: Rect::default(),
            expanded: HashSet::new(),
            pending: VecDeque::new(),
            answered: HashMap::new(),
            child_titles: HashMap::new(),
            status: String::new(),
            quit: false,
            reopening_selection: None,
            #[cfg(test)]
            sent: Vec::new(),
        }
    }

    /// Creates an application whose agents belong to `client`'s daemon.
    ///
    /// Construct, then attach once: the fleet has no shape of its own beyond
    /// the daemons in it, so one daemon is the same path as five.
    pub fn attached(harnesses: HarnessRegistry, client: Client) -> Self {
        let mut app = Self::new(harnesses);
        app.attach(client);
        app
    }

    /// Adds a daemon to the fleet, registering the machine it names itself as.
    ///
    /// The device is registered here, before the connection has said anything,
    /// so every project this daemon announces has a machine's row to be drawn
    /// under. A project stamped with a device the sidebar does not know is
    /// drawn nowhere at all.
    pub fn attach(&mut self, client: Client) {
        // The agents are no longer this process's to run, so the machine it
        // was standing in for goes with them: from here on what is on screen
        // belongs to a daemon. Through `forget_device` rather than
        // `AppState::remove_device` alone, because this client is holding the
        // children it started — dropping only the rows would leave a shell
        // running with no row, no reader and nobody left to stop it.
        if let Some(local) = self.local.take() {
            self.forget_device(local);
            self.remove_device(local);
        }

        let device = self.state.add_device(Device::new(client.device()));
        let generation = client.generation();

        let attachment = Attachment {
            device,
            client,
            generation,
            opened: Vec::new(),
        };

        match &mut self.mode {
            Mode::Attached(attachments) => attachments.push(attachment),
            Mode::Standalone => self.mode = Mode::Attached(vec![attachment]),
        }
    }

    /// Writes to the status line.
    ///
    /// Its own method rather than a public field: a `--daemon` endpoint that
    /// never answers at startup has to say so somewhere the user is actually
    /// looking, and the status line is the one place that survives to the
    /// first draw with no sidebar row and no attachment to hang a warning on.
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// The daemons this client is holding, or nothing when it holds none.
    fn attachments(&self) -> &[Attachment] {
        match &self.mode {
            Mode::Attached(attachments) => attachments,
            Mode::Standalone => &[],
        }
    }

    /// The daemon a pane is on.
    fn attachment_for_pane(&self, pane: PaneId) -> Option<&Attachment> {
        let project = self.state.pane(pane)?.project;
        self.attachment_for_project(project)
    }

    /// The daemon a project is on.
    fn attachment_for_project(&self, project: ProjectId) -> Option<&Attachment> {
        let device = self
            .state
            .projects()
            .iter()
            .find(|candidate| candidate.id == project)?
            .device;

        self.attachments()
            .iter()
            .find(|attachment| attachment.device == device)
    }

    /// The daemon a project is on, or `None` with the reason said out loud.
    fn reachable_for_project(&mut self, project: ProjectId) -> Option<&Attachment> {
        let device = self.attachment_for_project(project)?.device;

        if self.state.device(device).is_some_and(|d| d.reachable) {
            // Looked up a second time rather than kept: the borrow above has
            // to end before the status line below can be written.
            return self.attachment_for_project(project);
        }

        self.refuse(device);
        None
    }

    /// Whether the machine a pane is on will take a write, saying so when it
    /// will not.
    ///
    /// True when nothing is attached behind the pane: a standalone client's
    /// panes are this process's own children, and there is no machine between
    /// the keystroke and the process.
    fn reachable_for_pane(&mut self, pane: PaneId) -> bool {
        let Some(device) = self.attachment_for_pane(pane).map(|a| a.device) else {
            return true;
        };

        if self.state.device(device).is_some_and(|d| d.reachable) {
            return true;
        }

        self.refuse(device);
        false
    }

    /// Whether a pane's machine is reachable, without saying anything.
    ///
    /// For the frame's own work — resizing — which happens without the user
    /// asking and so has no business writing the status line every tick.
    fn can_reach_pane(&self, pane: PaneId) -> bool {
        let Some(device) = self.attachment_for_pane(pane).map(|a| a.device) else {
            return true;
        };

        self.state.device(device).is_some_and(|d| d.reachable)
    }

    /// Says which machine could not be reached.
    ///
    /// Named rather than "the daemon": on a fleet, which one is the whole
    /// question.
    fn refuse(&mut self, device: DeviceId) {
        let name = self
            .state
            .device(device)
            .map_or_else(|| "that machine".to_string(), |d| d.name.clone());

        self.status = format!("{name} is unreachable");
    }

    /// Folds or unfolds whatever the focus is in.
    ///
    /// A focused pane with subagents toggles those on their own -- a
    /// subagent list has nothing further to walk into, so folding and
    /// unfolding it is the same press in reverse. Past that, the project a
    /// pane is in and the machine that project is on cycle together: fold
    /// the project, then fold the machine, then the next press drops both
    /// at once, back to fully expanded -- so nothing it folds is ever left
    /// unreachable from the keyboard. With no pane focused, the same cycle
    /// runs over the selected project and its machine. On one machine there
    /// is no device rung to step -- the sidebar draws no row for it -- so
    /// the cycle is a plain fold, then unfold.
    fn toggle_fold(&mut self) {
        if let Some(pane) = self.state.focused_pane() {
            if !self.state.children_of(pane).is_empty() {
                self.state.toggle_pane_collapsed(pane);
                return;
            }

            if let Some(project) = self.state.pane(pane).map(|pane| pane.project) {
                self.toggle_project_and_device_fold(project);
                return;
            }
        }

        if let Some(project) = self.state.selected_project() {
            self.toggle_project_and_device_fold(project);
        }
    }

    /// Steps the project/machine half of the fold ladder one rung.
    ///
    /// Needs no memory of its own: the project and machine's own collapse
    /// flags already say where in the cycle a press lands, because folding
    /// only ever proceeds project-then-machine and unfolding always drops
    /// both at once -- there is no third shape a press has to remember its
    /// way through. A remembered direction would be one more thing that has
    /// to agree with what a click on the project or machine row just did
    /// behind this key's back; reading the flags fresh each press means
    /// there is nothing to fall out of sync.
    fn toggle_project_and_device_fold(&mut self, project: ProjectId) {
        let device = self
            .state
            .projects()
            .iter()
            .find(|candidate| candidate.id == project)
            .map(|candidate| candidate.device);

        // On one machine the sidebar draws no device row, so a folded device
        // is invisible: stepping that rung anyway would spend a press on
        // nothing, leaving the second press of `^a f` looking like a no-op
        // and a third one needed to unfold. Dropping the device out of the
        // ladder here, rather than trying to skip an invisible rung further
        // down, keeps the two branches below this in the same shape as the
        // federated case.
        let device = device.filter(|_| self.state.devices().len() > 1);

        if !self.state.is_project_collapsed(project) {
            self.state.toggle_project_collapsed(project);
            return;
        }

        if let Some(device) = device.filter(|d| !self.state.is_device_collapsed(*d)) {
            self.state.toggle_device_collapsed(device);
            return;
        }

        if let Some(device) = device {
            self.state.toggle_device_collapsed(device);
        }
        self.state.toggle_project_collapsed(project);
    }

    /// Records a title an agent set for one of its panes.
    ///
    /// Every title an agent announces arrives here, local or remote, because
    /// the two things worth fixing about one are the same on both sides: an
    /// agent that prefixes its own mark, and an agent that names its terminal
    /// after the directory it is working in.
    fn rename(&mut self, id: PaneId, title: &str) {
        let title = strip_mark(title);

        // Codex titles its terminal after the working directory, so its row
        // said what the project row above it already said. The harness's own
        // name is what tells one row from the next.
        let repeats_the_project = self
            .state
            .pane(id)
            .and_then(|pane| {
                self.state
                    .projects()
                    .iter()
                    .find(|project| project.id == pane.project)
            })
            .is_some_and(|project| project.name == title);

        if !title.is_empty() && !repeats_the_project {
            let _ = self.state.set_pane_title(id, title);
            return;
        }

        let Some(harness) = self.state.pane(id).map(|pane| pane.harness.clone()) else {
            return;
        };
        let name = self.harnesses.get(harness.as_str()).map_or_else(
            || harness.as_str().to_string(),
            |def| def.display_name.clone(),
        );

        let _ = self.state.set_pane_title(id, name);
    }

    /// Starts the directory browser at `dir` rather than the working
    /// directory.
    pub fn browse_from(&mut self, dir: impl Into<PathBuf>) {
        self.browse_from = dir.into();
    }

    /// Keeps the list of opened projects in `dir`, so it outlives the process.
    ///
    /// Without this a client forgets every project the moment it exits, which
    /// is what Dispatch did before the list existed.
    pub fn keep_projects_in(&mut self, dir: impl Into<PathBuf>) {
        self.kept = Some(dir.into());
    }

    /// Registers a project.
    ///
    /// Attached, the daemon is asked to open it and the project appears when it
    /// answers: it is the daemon that names projects, and both clients on a
    /// fleet have to use the same id for the same checkout.
    pub fn add_project(&mut self, root: PathBuf) {
        self.keep(&root);

        // The first daemon, because a root is a path and nothing has yet asked
        // the user which machine to read it on. For a client holding one — the
        // only shape there has ever been — it is that one.
        if let Mode::Attached(attachments) = &mut self.mode
            && let Some(attachment) = attachments.first_mut()
        {
            attachment
                .client
                .send(ClientMessage::OpenProject { root: root.clone() });
            // Remembered so a reconnection asks again: a daemon that was
            // restarted is serving whatever its own command line said, which
            // need not include what this client was opened with.
            if !attachment.opened.contains(&root) {
                attachment.opened.push(root);
            }
            return;
        }

        let source = if root.join(".git").exists() {
            ProjectSource::GitRepo { remote: None }
        } else {
            ProjectSource::LocalDir
        };

        // Stamped with this machine, like any other project: the sidebar draws
        // a project under its machine's row, so one naming a device that was
        // never registered is drawn nowhere at all — open, invisible and
        // unreachable.
        //
        // `local` is `Some` exactly while the mode is `Standalone`, which is
        // the only way to reach this line: `attach` takes it and leaves behind
        // an attachment the branch above always finds. Said out loud rather
        // than papered over with a nil device, because a project nobody can
        // see is the harder bug of the two.
        let device = self
            .local
            .expect("a standalone client registers its own machine in `App::new`");

        self.state
            .add_project(Project::new(root, source).with_device(device));
    }

    /// Adds `root` to the kept list, if this client keeps one.
    ///
    /// A list that cannot be written is reported in the status line rather
    /// than fatal: the project is open either way, and losing it on exit is
    /// not worth refusing to run over.
    fn keep(&mut self, root: &Path) {
        let Some(dir) = self.kept.clone() else {
            return;
        };

        if let Err(error) = dispatch_config::projects::remember(&dir, root) {
            self.status = format!("could not keep {}: {error}", root.display());
        }
    }

    /// Takes `root` off the kept list, if this client keeps one.
    fn unkeep(&mut self, root: &Path) {
        let Some(dir) = self.kept.clone() else {
            return;
        };

        if let Err(error) = dispatch_config::projects::forget(&dir, root) {
            self.status = format!("could not drop {}: {error}", root.display());
        }
    }

    /// What the daemons call themselves, when attached to any.
    ///
    /// All of them, comma-separated: the status line says where the agents are,
    /// and on a fleet that is more than one place.
    #[must_use]
    pub fn device(&self) -> Option<String> {
        let names: Vec<String> = self
            .attachments()
            .iter()
            .map(|attachment| attachment.client.device())
            .collect();

        (!names.is_empty()).then(|| names.join(", "))
    }

    /// Whether the loop should stop.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Starts a pane running `harness` in the selected project.
    ///
    /// Attached, this asks and returns: the pane appears when the daemon says it
    /// has started one, which is also how the other clients hear about it.
    pub fn spawn_pane(&mut self, harness: &str, area: Size) -> Result<()> {
        let Some(project_id) = self.state.selected_project() else {
            self.status = "no project selected".into();
            return Ok(());
        };

        // Cloned out of the registry rather than borrowed: naming the machine
        // that cannot be reached is a write to the status line, and the
        // registry is a field of the same `self`.
        let Some((display_name, launch)): Option<(String, Launch)> =
            self.harnesses.get(harness).map(|def| {
                (
                    def.display_name.clone(),
                    def.launch_for_current_platform().clone(),
                )
            })
        else {
            self.status = format!("unknown harness {harness:?}");
            return Ok(());
        };

        // Attached, the machine the project is on is the one asked — and a
        // machine out of reach is told to the user rather than written into a
        // socket nobody is reading.
        if !self.attachments().is_empty() {
            let Some(daemon) = self
                .reachable_for_project(project_id)
                .map(|attachment| attachment.client.handle())
            else {
                return Ok(());
            };

            daemon.send(ClientMessage::SpawnPane {
                project: project_id,
                harness: harness.to_string(),
                size: (area.cols, area.rows),
            });
            self.status = format!("starting {display_name}…");
            return Ok(());
        }

        let cwd = self
            .state
            .projects()
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.root.clone())
            .context("the selected project is registered")?;

        let session = PtySession::spawn(&launch, &cwd, area)
            .with_context(|| format!("failed to start {display_name}"))?;

        let id = self
            .state
            .spawn_pane(project_id, HarnessId::new(harness))
            .context("the selected project is registered")?;

        self.adopt(id, Backend::Local(session), &display_name)
    }

    /// Takes on a pane that now exists, wherever its process is.
    ///
    /// The sidebar should read "Claude Code", not "claude". The harness id is a
    /// filename; the display name is what the user chose to call it. A title
    /// sequence from the child replaces this later.
    fn adopt(&mut self, id: PaneId, backend: Backend, display_name: &str) -> Result<()> {
        let mut reader = ScreenReader::new().context("failed to create a screen reader")?;
        let screen = reader
            .read(backend.terminal())
            .context("failed to read the new pane")?;

        let _ = self.state.set_pane_title(id, display_name);

        self.panes.insert(
            id,
            Pane {
                backend,
                encoder: KeyEncoder::new().context("failed to create a key encoder")?,
                mouse: MouseEncoder::new().context("failed to create a mouse encoder")?,
                scrolled_back: false,
                reader,
                screen,
                titles: TitleScanner::new(),
            },
        );

        Ok(())
    }

    /// Acts on whatever the daemon has said since the last call.
    ///
    /// Returns whether anything needs redrawing. Standalone, there is nothing
    /// to hear and this does nothing.
    pub fn poll_daemon(&mut self) -> bool {
        let Mode::Attached(attachments) = &self.mode else {
            return false;
        };

        // Collected first: applying a message borrows `self` mutably, and the
        // attachments are borrowed from it. One entry per machine, so this
        // grows with the fleet rather than with the session.
        let snapshot: Vec<(DeviceId, u64, bool, String, Vec<ServerMessage>)> = attachments
            .iter()
            .map(|attachment| {
                (
                    attachment.device,
                    attachment.client.generation(),
                    attachment.client.is_connected(),
                    attachment.client.device(),
                    attachment.client.poll(),
                )
            })
            .collect();

        let mut changed = false;

        for (device, generation, connected, name, messages) in snapshot {
            changed |= self.sync_attachment(device, generation, connected, &name);

            for message in messages {
                changed |= self.apply_from(device, message);
            }
        }

        changed
    }

    /// Brings one attachment's device up to date, and rebuilds its rows when
    /// the connection behind them has been replaced.
    ///
    /// Returns whether anything changed on screen. A frame is asked for on the
    /// transitions only: saying "changed" every tick because a machine is still
    /// down would redraw the whole interface at the frame rate for as long as
    /// it stays down.
    fn sync_attachment(
        &mut self,
        device: DeviceId,
        generation: u64,
        connected: bool,
        name: &str,
    ) -> bool {
        let was = self.state.device(device).is_some_and(|d| d.reachable);
        self.state.set_device_reachable(device, connected);
        let mut changed = was != connected;

        // `client.device()` is read fresh every poll, but until now only
        // `reachable` was written back to state -- a daemon that came back
        // under a different name left the sidebar's row naming the old one
        // forever, with nothing past the one "reattached to {name}" status
        // line ever saying otherwise. Compared rather than written
        // unconditionally, so an unreachable machine's last-known name is not
        // stamped over itself, unchanged, on every poll.
        if self.state.device(device).is_some_and(|d| d.name != name) {
            self.state.set_device_name(device, name);
            changed = true;
        }

        let Mode::Attached(attachments) = &mut self.mode else {
            return changed;
        };
        let Some(attachment) = attachments.iter_mut().find(|a| a.device == device) else {
            return changed;
        };

        if attachment.generation == generation {
            return changed;
        }

        // Everything this machine was showing was described by a connection
        // that is gone. Its `Subscribe` replay describes its own fleet afresh,
        // so the rows are rebuilt from what it says rather than patched — and
        // only its rows: one daemon restarting says nothing about the others.
        attachment.generation = generation;
        let roots = attachment.opened.clone();
        let client = &attachment.client;

        for root in &roots {
            client.send(ClientMessage::OpenProject { root: root.clone() });
        }

        // Dropped first, regardless of what is on screen now: an entry left
        // over from this same device's last reconnect never got consumed
        // (the daemon behind it minted fresh ids on the way back, say), and
        // a second reconnect is no more likely to see it answered. Without
        // this a daemon that keeps restarting would pile up an intention
        // this device can never fulfil.
        self.reopening_selection = self
            .reopening_selection
            .take()
            .filter(|(d, _)| *d != device);

        // Remembered before it is forgotten: if what is on screen right now
        // belongs to this machine, its `ProjectOpened` is already on the way
        // back and should be selected again rather than left to the fallback
        // `forget_device` is about to trigger.
        if self.state.selected_project().is_some_and(|selected| {
            self.state
                .projects()
                .iter()
                .any(|p| p.id == selected && p.device == device)
        }) {
            self.reopening_selection = self
                .state
                .selected_project()
                .map(|selected| (device, selected));
        }

        self.forget_device(device);
        self.status = format!("reattached to {name}");

        true
    }

    /// Removes a device for good, rather than a connection that might come
    /// back.
    ///
    /// A promise to reselect one of this device's projects cannot be kept
    /// once the device it was made for is gone, so it is dropped here rather
    /// than left to sit unconsumed.
    fn remove_device(&mut self, device: DeviceId) {
        self.state.remove_device(device);
        self.reopening_selection = self
            .reopening_selection
            .take()
            .filter(|(d, _)| *d != device);
    }

    /// Selects a project by the user's own action, rather than by a
    /// reconnect restoring what was already on screen.
    ///
    /// The user's own choice of where to look always outranks a reconnect
    /// that is still waiting to put the view back where it was: without
    /// this, looking somewhere else during an outage would be undone the
    /// moment that machine's replay caught up.
    fn select_project(&mut self, project: ProjectId) {
        self.reopening_selection = None;
        let _ = self.state.select_project(project);
    }

    /// Forgets what one machine told us over a connection that has ended.
    ///
    /// A pane the old connection described may not exist any more: a daemon that
    /// was restarted names its panes afresh. Anything still running is described
    /// again by the new connection.
    fn forget_device(&mut self, device: DeviceId) {
        self.state.forget_device_projects(device);

        // Whatever the state dropped with those projects is dropped here too.
        // A view kept for a pane that is gone draws output nothing can reach
        // and takes keystrokes nothing would answer.
        let dropped: Vec<PaneId> = self
            .panes
            .keys()
            .copied()
            .filter(|id| self.state.pane(*id).is_none())
            .collect();

        for id in dropped {
            if let Some(mut pane) = self.panes.remove(&id) {
                // A local pane's process is this one's child: dropped without
                // being told, it keeps running with nothing left pointing at
                // it. A remote pane's is not — the daemon still has it, its
                // new connection announces it again, and a `ClosePane` here
                // would kill the very agent this rebuild is protecting.
                if matches!(pane.backend, Backend::Local(_)) {
                    pane.backend.terminate();
                }
            }

            self.expanded.remove(&id);
        }

        self.layout.retain(|(id, _)| self.state.pane(*id).is_some());

        // A request from a pane that no longer exists would be answered into a
        // void; the new connection's catch-up replays whatever is still
        // outstanding. `answered` and `child_titles` are left alone: they are
        // keyed by request and pane, which say nothing about which machine
        // they came from, and both are a handful of short strings.
        let live: Vec<ProjectId> = self.state.projects().iter().map(|p| p.id).collect();
        let was_shown = matches!(self.overlay, Some(Overlay::Approval { .. }))
            && self
                .pending
                .front()
                .is_some_and(|waiting| !live.contains(&waiting.project));
        self.pending
            .retain(|waiting| live.contains(&waiting.project));

        // A prompt whose request has just been forgotten cannot be answered,
        // and a picker offering projects that are gone would act on an id
        // nothing answers to.
        if was_shown || matches!(self.overlay, Some(Overlay::Project(_))) {
            self.overlay = None;
        }
    }

    /// Applies one message a daemon sent, stamped with the machine it came
    /// from. Returns whether to redraw.
    fn apply_from(&mut self, device: DeviceId, message: ServerMessage) -> bool {
        match message {
            ServerMessage::ProjectOpened { project } => {
                // Stamped here, the one place a project enters this client
                // from a daemon: the daemon knows nothing of the other
                // machines, so which one it is, is this client's to say.
                let id = project.id;
                self.state.add_project(project.with_device(device));

                // The other half of the reconnect fix in `sync_attachment`:
                // this is the project a reconnect forgot out from under the
                // selection, back again. Its panes have not replayed yet, so
                // this only recovers the selection itself — `adopt_pane`
                // picks up the focus once they do, the same way it would for
                // a project selected for the first time.
                if self.reopening_selection == Some((device, id)) {
                    self.reopening_selection = None;
                    let _ = self.state.select_project(id);
                }

                true
            }

            ServerMessage::ProjectClosed { project } => {
                // Whatever the daemon says about its own list is the truth:
                // this row is drawn from it.
                self.state.remove_project(project).is_ok()
            }

            ServerMessage::PaneSpawned {
                pane,
                project,
                harness,
                parent,
                durable,
            } => self.adopt_remote(pane, project, &harness, parent, durable),

            ServerMessage::PaneOutput { pane, bytes } => {
                let Some(target) = self.panes.get_mut(&pane) else {
                    return false;
                };

                if let Backend::Remote(remote) = &mut target.backend {
                    remote.feed(&bytes);
                }
                if let Ok(screen) = target.reader.read(target.backend.terminal()) {
                    target.screen = screen;
                }

                // Read here as well as for a local pane: the same bytes carry
                // the title whichever side the process is on, and a client that
                // has just been replayed a pane's output learns its name from
                // it.
                let title = target.titles.scan(&bytes);
                if let Some(title) = title {
                    self.rename(pane, &title);
                }

                true
            }

            ServerMessage::PaneChanged { pane, update } => match update {
                PaneUpdate::Status { status } => {
                    if let PaneStatus::Exited(code) = status
                        && let Some(target) = self.panes.get_mut(&pane)
                        && let Backend::Remote(remote) = &mut target.backend
                    {
                        remote.set_state(RunState::Exited(code));
                    }

                    self.state.set_pane_status(pane, status).is_ok()
                }
                PaneUpdate::Title { title } => {
                    self.rename(pane, &title);
                    true
                }
                // A newer daemon's update this build has no name for. The
                // protocol's promise is that it lands somewhere ignorable
                // rather than failing the frame and taking the connection with
                // it, and ignoring it is what that promise means here.
                PaneUpdate::Unknown => false,
            },

            ServerMessage::PaneClosed { pane } => {
                // Already gone if this client closed it; a pane another client
                // closed is removed here.
                self.panes.remove(&pane);
                // A pane that is gone cannot be opened into the grid, so it has
                // no business staying in `expanded` either.
                self.expanded.remove(&pane);
                self.state.close_pane(pane).is_ok()
            }

            ServerMessage::Error { error } => {
                self.status = error.to_string();
                true
            }

            ServerMessage::DelegatePending {
                request,
                parent,
                project,
                harness,
                task,
                depth,
            } => {
                self.pending.push_back(PendingRequest {
                    request,
                    parent,
                    project,
                    harness,
                    task,
                    depth,
                });

                // A keystroke meant for an agent must never land on an
                // approval, which cuts both ways: this only takes the
                // keyboard when nothing else already has it. Routed through
                // `open_next_approval` — the only place that ever constructs
                // `Overlay::Approval` — rather than written here directly, so
                // there is no second place that could show it over an empty
                // queue.
                if self.overlay.is_none() {
                    self.open_next_approval();
                }

                true
            }

            ServerMessage::DelegateResolved { request, outcome } => {
                // The subagent's row is titled with the opening words of its
                // task, not with the harness: four children of one pane all
                // reading "Claude Code" say nothing about which is which.
                // Recorded here rather than looked up on arrival because this
                // is the last moment the task and the new pane's id are both
                // in hand — and it always comes first, since the daemon
                // resolves a request before it announces the pane.
                if let DelegateOutcome::Approved { pane } = outcome
                    && let Some(task) = self.task_of(request)
                    && let Some(title) = task_title(&task)
                {
                    self.child_titles.insert(pane, title);
                }

                // Whether this client's own decision resolved it (already
                // popped out of `pending` — see `decide`), another client's
                // did, or the daemon's own deadline did, the queue has no
                // further reason to hold it.
                let showing_this_one = matches!(self.overlay, Some(Overlay::Approval { .. }))
                    && self
                        .pending
                        .front()
                        .is_some_and(|waiting| waiting.request == request);

                self.pending.retain(|waiting| waiting.request != request);

                // Only a genuine withdrawal reaches here: a decision this
                // client made itself already closed or advanced the overlay
                // in `decide`, before this message was ever sent. Substituting
                // the next request under the user's fingers would risk
                // approving something they never read; closing and leaving
                // the reminder (see `draw_status`) is what `decide` itself
                // does for the next request too, except deliberately, only
                // once the user asks with `^a a`.
                if showing_this_one {
                    self.overlay = None;
                }

                true
            }

            // The handshake is done by the client, and nothing here pings.
            // `DelegateFinished` is for the delegate caller, not interface
            // clients. Unknown messages from newer peers are ignored.
            ServerMessage::Welcome { .. }
            | ServerMessage::Pong { .. }
            | ServerMessage::DelegateFinished { .. }
            | ServerMessage::Unknown => false,
        }
    }

    /// Applies one message from no machine in particular.
    ///
    /// For tests that drive a message straight in rather than through a
    /// daemon's queue. Only for messages that name a pane or a request: a
    /// project arriving this way would be stamped with a machine the sidebar
    /// has never heard of, which is a project drawn nowhere at all.
    #[cfg(test)]
    fn apply(&mut self, message: ServerMessage) -> bool {
        debug_assert!(
            !matches!(message, ServerMessage::ProjectOpened { .. }),
            "a project has to arrive from a machine"
        );

        self.apply_from(DeviceId::nil(), message)
    }

    /// Takes on a pane the daemon has started.
    ///
    /// `parent` and `durable` come from the announcement rather than from
    /// anything this client decided: a subagent is drawn under the pane that
    /// asked for it, and whether it outlives that pane is what the tombstone
    /// rule in [`AppState::close_pane`] turns on. A client that dropped either
    /// would draw every subagent as a top-level pane and tile them all.
    fn adopt_remote(
        &mut self,
        id: PaneId,
        project: ProjectId,
        harness: &str,
        parent: Option<PaneId>,
        durable: bool,
    ) -> bool {
        if self.panes.contains_key(&id) {
            return false;
        }

        // The machine the pane's project is on, not "the daemon": a write to
        // this pane has to go to the connection that owns it and to no other.
        let Some(daemon) = self
            .attachment_for_project(project)
            .map(|attachment| attachment.client.handle())
        else {
            // A pane in a project this client has not been told about: the
            // announcement is on its way, and the pane arrives with the next
            // subscribe rather than being drawn with nowhere to belong.
            tracing::warn!(pane = %id, project = %project, "a pane for an unknown project");
            return false;
        };

        let mut pane = CorePane::new(project, HarnessId::new(harness));
        pane.id = id;
        pane.parent = parent;
        pane.durable = durable;
        if self.state.adopt_pane(pane).is_err() {
            // A pane in a project this client has not been told about: the
            // announcement is on its way, and the pane arrives with the next
            // subscribe rather than being drawn with nowhere to belong.
            tracing::warn!(pane = %id, project = %project, "a pane for an unknown project");
            return false;
        }

        // Sized to nothing much: the next frame's layout resizes it to the
        // rectangle it actually gets.
        let backend = match RemotePane::new(id, daemon, Size::new(80, 24)) {
            Ok(remote) => Backend::Remote(remote),
            Err(error) => {
                tracing::warn!(%error, "failed to prepare a pane");
                return false;
            }
        };

        // A subagent is named by what it was asked to do; every other pane by
        // the harness running in it. Either way the title scanner replaces this
        // as soon as the agent names itself. A child whose task never reached
        // this client — a reattach replaying panes that were spawned before it
        // was listening — falls back to the harness rather than to nothing.
        let title = self.child_titles.remove(&id).unwrap_or_else(|| {
            self.harnesses
                .get(harness)
                .map_or_else(|| harness.to_string(), |def| def.display_name.clone())
        });

        if let Err(error) = self.adopt(id, backend, &title) {
            tracing::warn!(%error, "failed to adopt a pane");
            return false;
        }

        true
    }

    /// Feeds pending output into every pane and refreshes what changed.
    ///
    /// Returns whether anything needs redrawing.
    pub fn poll_panes(&mut self) -> bool {
        let mut changed = false;
        let mut exited = Vec::new();
        let mut renamed = Vec::new();

        for (id, pane) in &mut self.panes {
            let output = pane.backend.drain();

            if !output.is_empty() {
                changed = true;

                if let Some(title) = pane.titles.scan(&output) {
                    renamed.push((*id, title));
                }

                if let Ok(screen) = pane.reader.read(pane.backend.terminal()) {
                    pane.screen = screen;
                }
            }

            if let RunState::Exited(code) = pane.backend.state() {
                exited.push((*id, code));
            }
        }

        for (id, title) in renamed {
            self.rename(id, &title);
        }

        for (id, code) in exited {
            let status = PaneStatus::Exited(code);

            // A pane's last output and its exit rarely arrive in the same
            // poll: the agent prints its goodbye, that poll draws it, and the
            // process is gone by the next one with nothing left to read. Only
            // reporting a change when there was output left the dead pane
            // holding its tile until some unrelated keystroke forced a frame.
            //
            // Asked rather than assumed, because the status is set on every
            // poll for as long as the pane is listed: reporting a change each
            // time would redraw the screen forever at the frame rate.
            if self
                .state
                .pane(id)
                .is_some_and(|pane| pane.status != status)
            {
                changed = true;
            }

            // An exited pane keeps its screen and stays selectable, so its
            // final output can be read before it is closed.
            let _ = self.state.set_pane_status(id, status);
        }

        changed
    }

    /// Acts on one input event.
    pub fn handle(&mut self, event: &Event, area: Size) -> Result<()> {
        // An overlay takes the keyboard while it is open, so arrow keys choose
        // and approval keys decide rather than either reaching an agent.
        if self.overlay.is_some() {
            return self.handle_overlay(event, area);
        }

        // The sidebar is not otherwise part of input routing — `layout` below
        // covers only the tiled grid — so a click on one of its rows is
        // resolved here rather than through the router.
        if let Event::Mouse(mouse) = event
            && matches!(mouse.kind, MouseEventKind::Down(_))
            && let Some(hit) =
                sidebar::hit_test(&self.state, self.sidebar_area, mouse.column, mouse.row)
        {
            match hit {
                sidebar::Hit::Device(id) => self.state.toggle_device_collapsed(id),
                // A heading carries no pane, so the whole row is the
                // project's: a click both moves the view there and folds the
                // panes away.
                sidebar::Hit::Project(id) => {
                    self.select_project(id);
                    self.state.toggle_project_collapsed(id);
                }
                sidebar::Hit::Twisty(id) => self.state.toggle_pane_collapsed(id),
                sidebar::Hit::Pane(id) => self.focus_pane(id),
            }
            return Ok(());
        }

        let layout = std::mem::take(&mut self.layout);
        let action = self.router.handle(event, &layout);
        self.layout = layout;

        match action {
            Action::None => {}
            Action::Quit => self.quit = true,
            Action::SendKey(key, mods) => self.send_key(key, mods),
            Action::Paste(text) => self.paste(&text),
            Action::FocusPane(id) => self.focus_pane(id),
            Action::FocusDirection(direction) => self.focus_direction(direction),
            Action::SendMouse(id, input) => self.send_mouse(id, input),
            Action::Scroll(rows) => self.scroll_focused(rows),
            Action::ToggleZoom => self.state.toggle_zoom(),
            Action::ClosePane => self.close_focused(),
            Action::NewPane => self.open_harness_picker(),
            Action::ProjectPicker => self.open_project_picker(),
            Action::HarnessManager => self.open_harness_manager(),
            Action::SelectTab(index) => self.select_tab(index),
            Action::NextTab => self.select_tab(self.current_tab() + 1),
            Action::Scrollback => self.scroll_focused(-10),
            Action::Approvals => self.open_next_approval(),
            Action::ToggleFold => self.toggle_fold(),
            Action::OpenProject => self.open_browser(),
            Action::ExpandChild => self.expand_child(),
            Action::CollapseChild => self.collapse_child(),
        }

        Ok(())
    }

    /// Shows a tab by focusing its first pane.
    ///
    /// Focusing is how a tab is shown at all, since the view follows the focus.
    /// Out of range wraps to the first, so `^a 9` on a two-tab fleet lands
    /// somewhere real rather than doing nothing.
    fn select_tab(&mut self, index: usize) {
        let index = if index < self.tab_count() { index } else { 0 };

        if let Some(first) = self
            .tileable()
            .chunks(PANES_PER_TAB)
            .nth(index)
            .and_then(<[PaneId]>::first)
            .copied()
        {
            let _ = self.state.focus(first);
        }
    }

    /// Focuses a pane and, if it is a subagent, brings it into the tiled grid.
    ///
    /// Every way of focusing a pane — the mouse moving over a tiled one, a
    /// sidebar click, `h`/`j`/`k`/`l` — goes through this, so it is the one
    /// place a child needs to be added to `expanded` rather than several.
    fn focus_pane(&mut self, id: PaneId) {
        if self.state.focus(id).is_err() {
            return;
        }
        if self
            .state
            .pane(id)
            .is_some_and(|pane| pane.parent.is_some())
        {
            self.expanded.insert(id);
        }
    }

    /// Opens the focused pane's first child into the grid and focuses it; if
    /// it has none of its own, cycles to the next sibling under the same
    /// parent instead.
    ///
    /// A subagent otherwise has no keyboard way in: `focus_direction` only
    /// searches the tiled grid, which excludes an unopened child by
    /// construction, and a sidebar click needs a mouse. Descending before
    /// cycling is what makes a grandchild reachable at all — the daemon
    /// enforces a depth cap greater than one, so there can be one — since
    /// cycling alone only ever visits siblings at a single generation.
    fn expand_child(&mut self) {
        let Some(focused) = self.state.focused_pane() else {
            return;
        };

        // Descend into the focused pane's own children first — the only way
        // a grandchild is reachable at all, since cycling only ever visits
        // one generation. Only once it has none of its own does this fall
        // back to cycling its siblings under the same parent.
        let own_children = self.open_children(focused);
        if let Some(&first) = own_children.first() {
            self.focus_pane(first);
            return;
        }

        let Some(parent) = self.state.pane(focused).and_then(|pane| pane.parent) else {
            return;
        };

        let siblings = self.open_children(parent);
        if siblings.is_empty() {
            return;
        }

        let next = siblings
            .iter()
            .position(|&id| id == focused)
            .map_or(0, |index| (index + 1) % siblings.len());

        self.focus_pane(siblings[next]);
    }

    /// The live — not tombstoned — children of `parent`, in spawn order.
    ///
    /// `AppState::children_of` keeps a closed pane's row around so a
    /// surviving child still has somewhere to be drawn under; `expand_child`
    /// has no use for a row with no process behind it and no place in the
    /// tiled grid to put it.
    fn open_children(&self, parent: PaneId) -> Vec<PaneId> {
        self.state
            .children_of(parent)
            .into_iter()
            .filter(|child| !child.closed)
            .map(|child| child.id)
            .collect()
    }

    /// If the focused pane is a subagent, removes it from the tiled grid and
    /// returns focus to its parent.
    fn collapse_child(&mut self) {
        let Some(focused) = self.state.focused_pane() else {
            return;
        };
        let Some(parent) = self.state.pane(focused).and_then(|pane| pane.parent) else {
            return;
        };

        self.expanded.remove(&focused);
        self.focus_pane(parent);
    }

    /// Handles input while an overlay has the keyboard.
    fn handle_overlay(&mut self, event: &Event, area: Size) -> Result<()> {
        let Event::Key(key) = event else {
            return Ok(());
        };
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        if matches!(self.overlay, Some(Overlay::Approval { .. })) {
            self.handle_approval_key(key);
            return Ok(());
        }

        // The browser takes every plain key: what is typed is a filter, and a
        // filter that reached the focused pane would be running commands in
        // an agent.
        if matches!(self.overlay, Some(Overlay::Browse(_))) {
            self.handle_browser_key(key);
            return Ok(());
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(picker) = self.overlay.as_mut().and_then(Overlay::picker_mut) {
                    picker.next();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(picker) = self.overlay.as_mut().and_then(Overlay::picker_mut) {
                    picker.previous();
                }
            }
            // Only the project picker: `d` in the harness picker would be a
            // keystroke away from deleting the wrong kind of thing.
            KeyCode::Char('d') if matches!(self.overlay, Some(Overlay::Project(_))) => {
                self.drop_selected_project();
            }
            KeyCode::Enter => {
                let chosen = self.overlay.as_ref().and_then(|overlay| {
                    let kind = overlay.kind()?;
                    let item = overlay.picker()?.selected()?;
                    Some((kind, item.id.clone()))
                });

                self.overlay = None;

                if let Some((kind, id)) = chosen {
                    self.choose(kind, &id, area)?;
                }
            }
            _ => {}
        }

        // A request that arrived while a picker had the keyboard was queued
        // rather than shown; once the picker is gone, this is the same free
        // keyboard `DelegatePending` would have found.
        if self.overlay.is_none() {
            self.open_next_approval();
        }

        Ok(())
    }

    /// Acts on one key while the directory browser is open.
    ///
    /// Plain letters are the filter, so every command here is an arrow, Enter,
    /// Tab, Escape or a Ctrl chord — there are no letters left to spend.
    fn handle_browser_key(&mut self, key: &KeyEvent) {
        let Some(Overlay::Browse(browser)) = &mut self.overlay else {
            return;
        };

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Down => browser.next(),
            KeyCode::Up => browser.previous(),
            KeyCode::Right => browser.descend(),
            KeyCode::Left => browser.ascend(),
            // Completing a typed path, or walking into the highlighted
            // directory: the same "finish what I started" key either way.
            KeyCode::Tab => {
                if browser.is_path() {
                    browser.complete();
                } else {
                    browser.descend();
                }
            }
            KeyCode::Backspace => browser.backspace(),
            KeyCode::Char('g') if ctrl => browser.toggle_scan(),
            KeyCode::Enter => {
                // A typed path names the project directly; otherwise it is the
                // row the user is sitting on.
                let chosen = if browser.is_path() {
                    let typed = browser.typed_dir();
                    if typed.is_none() {
                        self.status = format!("no such directory: {}", browser.input());
                    }
                    typed
                } else {
                    browser.selected().map(|entry| entry.path.clone())
                };

                if let Some(root) = chosen {
                    self.open_browsed(root);
                }
            }
            KeyCode::Char(c) if !ctrl => browser.push(c),
            _ => {}
        }
    }

    /// Modifiers that disqualify a key from being an approval action.
    ///
    /// `Ctrl-a` is the default prefix — the most-pressed combination in the
    /// program, and the first half of the very `^a a` chord that reopens this
    /// prompt — so reading its `Char('a')` as the plain `a` that approves
    /// would grant a subagent by reaching for a command. `SHIFT` is
    /// deliberately not here: Windows derives `SHIFT` from the physical key
    /// alone, but derives a letter's *case* from `shift XOR caps lock`, so
    /// with caps lock on, the plain `a` key arrives as `(Char('A'), NONE)`
    /// and physical `Shift-a` as `(Char('a'), SHIFT)`. Rejecting `SHIFT`
    /// outright would leave caps-lock users with no way to approve or deny at
    /// all — only `Esc` would ever match.
    const APPROVAL_REJECTED_MODIFIERS: KeyModifiers = KeyModifiers::CONTROL
        .union(KeyModifiers::ALT)
        .union(KeyModifiers::SUPER)
        .union(KeyModifiers::HYPER)
        .union(KeyModifiers::META);

    /// Handles a key while the approval prompt has the keyboard.
    ///
    /// Blanket intent is read off the character's own case (`'A'` versus
    /// `'a'`), not off the `SHIFT` modifier: on Windows with caps lock on, the
    /// physical `a` key arrives as `Char('A')` with no modifier at all, and
    /// `Shift-a` arrives as `Char('a')` *with* `SHIFT` — the modifier and the
    /// letter one might expect to go together do not.
    ///
    /// `Esc` defers rather than denies — a mistaken deny throws away work the
    /// agent has already reasoned about — so it is the only key here that
    /// leaves the request in `pending` rather than answering it.
    fn handle_approval_key(&mut self, key: &KeyEvent) {
        if key.modifiers.intersects(Self::APPROVAL_REJECTED_MODIFIERS) {
            return;
        }

        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Char('a') => self.decide(true, false),
            KeyCode::Char('A') => self.decide(true, true),
            KeyCode::Char('d') | KeyCode::Char('D') => self.decide(false, false),
            KeyCode::Up => self.scroll_approval(false),
            KeyCode::Down => self.scroll_approval(true),
            _ => {}
        }
    }

    /// Answers the request at the front of the queue, and moves on to the
    /// next one if there is one waiting.
    ///
    /// Advancing to that next request here — under the same keystroke that
    /// just answered the one before it — is deliberate: it is the user's own
    /// action carrying them forward through the queue, unlike a withdrawal
    /// (see `apply`'s `DelegateResolved` arm), which never puts a new request
    /// under their fingers uninvited.
    fn decide(&mut self, approve: bool, blanket: bool) {
        let Some(waiting) = self.pending.pop_front() else {
            self.overlay = None;
            return;
        };

        // Kept so the subagent's row can be named after the task: this request
        // leaves the queue here, one round trip before the daemon says whether
        // a pane came of it.
        if approve {
            self.answered.insert(waiting.request, waiting.task.clone());
        }

        let message = ClientMessage::DelegateDecision {
            request: waiting.request,
            approve,
            blanket,
        };

        #[cfg(test)]
        self.sent.push(message.clone());

        // Answered to the machine that asked. Another machine's daemon knows
        // nothing of this request and would only log an id it has never seen.
        if let Some(daemon) = self
            .attachment_for_project(waiting.project)
            .map(|attachment| attachment.client.handle())
        {
            daemon.send(message);
        }

        self.open_next_approval();
    }

    /// What a request asked for, wherever this client last had it.
    ///
    /// A request this client answered itself left `pending` under the user's own
    /// keystroke, so `answered` is where its task is; one settled by another
    /// client or by the daemon's deadline is still queued, where the `retain`
    /// below is about to drop it.
    fn task_of(&mut self, request: RequestId) -> Option<String> {
        if let Some(task) = self.answered.remove(&request) {
            return Some(task);
        }

        self.pending
            .iter()
            .find(|waiting| waiting.request == request)
            .map(|waiting| waiting.task.clone())
    }

    /// Records a scroll of the task text of the request currently shown.
    ///
    /// Not clamped here: only the draw knows the box's real inner width and
    /// height, so `draw_overlay` is what clamps this against
    /// [`Approval::total_rows`] before rendering, and persists the clamped
    /// value back here. Clamping on the keystroke instead would drop a `↓`
    /// that arrives before the prompt has ever been drawn — there is no box to
    /// measure against yet — and would leave a stale, too-large offset
    /// rendering blank after a resize to a wider box, until the next `↓`
    /// happened to nudge it back into range.
    fn scroll_approval(&mut self, down: bool) {
        let Some(Overlay::Approval { scroll }) = &mut self.overlay else {
            return;
        };

        if down {
            *scroll = scroll.saturating_add(1);
        } else {
            *scroll = scroll.saturating_sub(1);
        }
    }

    /// Builds the approval widget for the request at the front of the queue,
    /// if there is one, at the given scroll offset.
    fn approval_widget(&self, scroll: u16) -> Option<Approval<'_>> {
        let request = self.pending.front()?;

        let asking = self
            .state
            .pane(request.parent)
            .map_or("a pane", |pane| pane.title.as_str());
        let project = self
            .state
            .projects()
            .iter()
            .find(|project| project.id == request.project)
            .map_or("an unknown project", |project| project.name.as_str());

        Some(Approval {
            asking,
            harness: &request.harness,
            project,
            depth: request.depth,
            task: &request.task,
            waiting: self.pending.len().saturating_sub(1),
            scroll,
        })
    }

    /// Opens the approval prompt for whatever is at the front of the queue,
    /// or closes it when there is nothing left to ask about.
    ///
    /// This is `Action::Approvals`, reached with the queue as it stands
    /// whenever nothing new has arrived; it is also how the prompt advances
    /// to the next request after one is answered. It is also the *only*
    /// place `Overlay::Approval` is ever constructed, so it showing over an
    /// empty queue is not just unlikely — nothing else can make it happen.
    fn open_next_approval(&mut self) {
        self.overlay = if self.pending.is_empty() {
            None
        } else {
            Some(Overlay::Approval { scroll: 0 })
        };
    }

    /// Acts on a picker selection.
    fn choose(&mut self, kind: OverlayKind, id: &str, area: Size) -> Result<()> {
        match kind {
            OverlayKind::Harness => self.spawn_pane(id, area)?,
            OverlayKind::Project => {
                if let Some(project) = self
                    .state
                    .projects()
                    .iter()
                    .find(|p| p.id.to_string() == id)
                    .map(|p| p.id)
                {
                    self.select_project(project);
                }
            }
            OverlayKind::Register => {
                let dir = dispatch_os::paths::harnesses_dir()
                    .context("failed to locate the harness directory")?;
                dispatch_config::register_harness(&dir, id)
                    .with_context(|| format!("failed to register {id}"))?;

                // Reload so the new harness is offered immediately rather
                // than only after a restart.
                self.harnesses = HarnessRegistry::load_from_dir(&dir)
                    .context("failed to reload harness definitions")?;
                self.status = format!("registered {id}");
            }
        }

        Ok(())
    }

    fn open_harness_picker(&mut self) {
        let items: Vec<Item> = self
            .harnesses
            .all()
            .map(|h| Item::new(&h.id, &h.display_name).with_detail(&h.launch.command))
            .collect();

        if items.is_empty() {
            self.status = "no harnesses registered; press ^a H to add one".into();
            return;
        }

        self.overlay = Some(Overlay::Harness(Picker::new("New pane", items)));
    }

    /// Drops the project the picker is sitting on from the kept list.
    ///
    /// Refused while it still has panes: they would go on running with no row
    /// left to reach them by. Attached, the daemon is asked and the row goes
    /// when it answers — it keeps the list a client is handed on every
    /// subscribe, so a row dropped here alone would come back.
    fn drop_selected_project(&mut self) {
        let Some(id) = self
            .overlay
            .as_ref()
            .and_then(Overlay::picker)
            .and_then(Picker::selected)
            .map(|item| item.id.clone())
        else {
            return;
        };

        let Some(project) = self
            .state
            .projects()
            .iter()
            .find(|p| p.id.to_string() == id)
            .map(|p| (p.id, p.root.clone()))
        else {
            return;
        };
        let (project, root) = project;

        if !self.state.panes_for(project).is_empty() {
            self.status = "close its panes first".into();
            return;
        }

        self.unkeep(&root);
        if let Mode::Attached(attachments) = &mut self.mode {
            for attachment in attachments.iter_mut() {
                attachment.opened.retain(|kept| kept != &root);
            }
        }

        if !self.attachments().is_empty() {
            let Some(daemon) = self
                .reachable_for_project(project)
                .map(|attachment| attachment.client.handle())
            else {
                return;
            };

            daemon.send(ClientMessage::CloseProject { project });
            // The row goes when the daemon says so.
            return;
        }

        if self.state.remove_project(project).is_ok() {
            self.status = format!("dropped {}", root.display());
        }

        self.reopen_project_picker();
    }

    /// Redraws the project picker over the list as it now is, or closes it
    /// when nothing is left to choose.
    fn reopen_project_picker(&mut self) {
        if self.state.projects().is_empty() {
            self.overlay = None;
            return;
        }

        self.open_project_picker();
    }

    /// Opens the directory browser.
    ///
    /// Picks up where it was left: walking back to the same directory every
    /// time is the tax on adding a second project from the same tree.
    fn open_browser(&mut self) {
        let browser = match self.browser.take() {
            Some(browser) => browser,
            None => Browser::new(&self.browse_from),
        };

        self.overlay = Some(Overlay::Browse(browser));
    }

    /// Opens `root` as a project, from the browser.
    fn open_browsed(&mut self, root: PathBuf) {
        self.add_project(root.clone());

        // Attached, the project arrives when the daemon answers, so there is
        // nothing to select here yet.
        if let Some(project) = self
            .state
            .projects()
            .iter()
            .find(|project| project.root == root)
            .map(|project| project.id)
        {
            self.select_project(project);
        }

        self.status = format!("opened {}", root.display());

        if let Some(Overlay::Browse(browser)) = self.overlay.take() {
            // Kept for the next `^a o`, which is usually in the same tree.
            self.browser = Some(browser);
        }
    }

    /// Offers every known project, naming the machine alongside the path once
    /// more than one is attached.
    ///
    /// A path alone does not tell two projects apart when a checkout is
    /// mirrored at the same path on two machines — or, as in a federation
    /// reached by name rather than by filesystem, when nothing ties the
    /// paths together at all. On one machine there is nothing to
    /// disambiguate, so the row stays just the path, matching what it always
    /// showed.
    fn open_project_picker(&mut self) {
        let multiple_devices = self.state.devices().len() > 1;

        let items: Vec<Item> = self
            .state
            .projects()
            .iter()
            .map(|p| {
                let mut detail = p.root.display().to_string();
                if multiple_devices {
                    if let Some(device) = self.state.device(p.device) {
                        detail = format!("{detail} ({})", device.name);
                    }
                }
                Item::new(p.id.to_string(), &p.name).with_detail(detail)
            })
            .collect();

        self.overlay = Some(Overlay::Project(Picker::new("Project", items)));
    }

    /// Offers harnesses that are installed but not yet registered.
    fn open_harness_manager(&mut self) {
        let found = dispatch_config::discover_unregistered(&self.harnesses, &[]);

        if found.is_empty() {
            self.status = format!(
                "{} harness(es) registered; nothing else found on PATH",
                self.harnesses.len()
            );
            return;
        }

        let items: Vec<Item> = found
            .iter()
            .map(|id| {
                let detail = dispatch_config::which(id)
                    .map_or_else(String::new, |p| p.display().to_string());
                Item::new(id, id).with_detail(detail)
            })
            .collect();

        self.overlay = Some(Overlay::Register(Picker::new("Add harness", items)));
    }

    fn send_key(&mut self, key: dispatch_pty::Key, mods: dispatch_pty::Modifiers) {
        let Some(id) = self.state.focused_pane() else {
            return;
        };

        // A keystroke for a machine that is out of reach is refused and said
        // out loud: dropped silently, the user types a whole command into a
        // pane that never sees it.
        if !self.reachable_for_pane(id) {
            return;
        }

        // Typing jumps back to the newest output, as every terminal does:
        // otherwise the reply to what was just typed appears somewhere the
        // user is not looking.
        self.scroll_to_bottom(id);

        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };

        // Typing into a pane whose process has exited would go nowhere, and
        // the write would fail every keystroke.
        if !matches!(pane.backend.state(), RunState::Running) {
            return;
        }

        match pane.encoder.encode(pane.backend.terminal(), key, mods) {
            Ok(bytes) if !bytes.is_empty() => {
                if let Err(error) = pane.backend.write(&bytes) {
                    tracing::warn!(%error, "failed to write to a pane");
                }
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to encode a key"),
        }
    }

    /// Forwards a pointer event to a pane, or scrolls it.
    ///
    /// A pane that tracks the mouse receives the event. One that does not gets
    /// nothing, and a wheel event then scrolls its scrollback instead, which
    /// is what a terminal without mouse tracking does.
    fn send_mouse(&mut self, id: PaneId, input: MouseInput) {
        use dispatch_pty::MouseButton;

        let wheel = match input.button {
            MouseButton::WheelUp => Some(-3),
            MouseButton::WheelDown => Some(3),
            _ => None,
        };

        // A pointer event the agent would answer is refused out loud when its
        // machine is out of reach. A wheel is not: with nowhere to send it, it
        // falls through to this client's own scrollback below, and reading
        // never needed the daemon.
        if !self.can_reach_pane(id) {
            match wheel {
                Some(rows) => self.scroll_pane(id, rows),
                // Called for what it says, not what it answers: the machine is
                // already known to be out of reach.
                None => drop(self.reachable_for_pane(id)),
            }
            return;
        }

        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };

        let size = pane.backend.size();
        let bytes = match pane.mouse.encode(pane.backend.terminal(), size, input) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, "failed to encode a pointer event");
                return;
            }
        };

        if !bytes.is_empty() {
            if let Err(error) = pane.backend.write(&bytes) {
                tracing::warn!(%error, "failed to send a pointer event to a pane");
            }
            return;
        }

        // Nothing was encoded, so the pane is not tracking the mouse.
        let Some(rows) = wheel else {
            return;
        };

        self.scroll_pane(id, rows);
    }

    /// Scrolls the focused pane.
    fn scroll_focused(&mut self, rows: isize) {
        let Some(id) = self.state.focused_pane() else {
            return;
        };
        self.scroll_pane(id, rows);
    }

    /// Scrolls one pane and refreshes what it shows.
    fn scroll_pane(&mut self, id: PaneId, rows: isize) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };

        pane.backend.terminal_mut().scroll(ScrollTo::Delta(rows));
        pane.scrolled_back = true;

        if let Ok(screen) = pane.reader.read(pane.backend.terminal()) {
            pane.screen = screen;
        }

        self.status = "scrolled back — press End or type to return".into();
    }

    /// Returns a pane to the newest output.
    fn scroll_to_bottom(&mut self, id: PaneId) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        if !pane.scrolled_back {
            return;
        }

        pane.backend.terminal_mut().scroll(ScrollTo::Bottom);
        pane.scrolled_back = false;

        if let Ok(screen) = pane.reader.read(pane.backend.terminal()) {
            pane.screen = screen;
        }

        self.status.clear();
    }

    fn paste(&mut self, text: &str) {
        let Some(id) = self.state.focused_pane() else {
            return;
        };

        // As with a keystroke: a paste that vanishes is worse than one refused.
        if !self.reachable_for_pane(id) {
            return;
        }

        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };

        // Wrapped in bracketed paste markers only for a child that turned the
        // mode on. Dispatch used to wrap unconditionally, so a shell -- which
        // never asks for it -- received `\x1b[200~` as input and answered
        // `00~…: command not found`, with the pasted command mangled along with
        // it. The encoder also takes care of what an unwrapped paste needs
        // instead: newlines become carriage returns, as a keyboard would send.
        let bracketed = pane.backend.terminal().bracketed_paste();
        let bytes = dispatch_pty::encode_paste(text, bracketed);

        if let Err(error) = pane.backend.write(&bytes) {
            tracing::warn!(%error, "failed to paste into a pane");
        }
    }

    /// Moves focus to the nearest pane in `direction`.
    ///
    /// Compares against the last drawn layout, so movement follows what is on
    /// screen rather than spawn order.
    fn focus_direction(&mut self, direction: Direction) {
        let Some(current) = self.state.focused_pane() else {
            return;
        };
        let Some(from) = self
            .layout
            .iter()
            .find(|(id, _)| *id == current)
            .map(|(_, r)| *r)
        else {
            return;
        };

        let best = self
            .layout
            .iter()
            .filter(|(id, _)| *id != current)
            .filter(|(_, rect)| match direction {
                Direction::Left => rect.x + rect.width <= from.x,
                Direction::Right => rect.x >= from.x + from.width,
                Direction::Up => rect.y + rect.height <= from.y,
                Direction::Down => rect.y >= from.y + from.height,
            })
            .min_by_key(|(_, rect)| {
                let dx = i32::from(rect.x) - i32::from(from.x);
                let dy = i32::from(rect.y) - i32::from(from.y);
                dx * dx + dy * dy
            })
            .map(|(id, _)| *id);

        if let Some(id) = best {
            let _ = self.state.focus(id);
        }
    }

    fn close_focused(&mut self) {
        let Some(id) = self.state.focused_pane() else {
            return;
        };

        // Refused rather than done locally: the machine still has the process,
        // and a row taken off this client's screen is a running agent nobody
        // can find again.
        if !self.reachable_for_pane(id) {
            return;
        }

        if let Some(mut pane) = self.panes.remove(&id) {
            pane.backend.terminate();
        }

        let _ = self.state.close_pane(id);
        // A closed pane cannot be brought into the grid, so it has nothing
        // left to be expanded into.
        self.expanded.remove(&id);
    }

    /// Draws one frame.
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();

        let sidebar_width = sidebar::WIDTH.min(area.width);
        let sidebar_area = Rect::new(area.x, area.y, sidebar_width, area.height);

        // One row at the bottom for status.
        let body_height = area.height.saturating_sub(1);
        let panes_area = Rect::new(
            area.x + sidebar_width,
            area.y,
            area.width.saturating_sub(sidebar_width),
            body_height,
        );

        frame.render_widget(
            Sidebar::new(&self.state).with_harnesses(&self.harnesses),
            sidebar_area,
        );
        self.sidebar_area = sidebar_area;

        // One row above the grid, and only once there is a second tab: a row
        // saying "1" and nothing else is a row of output given away for no
        // information.
        let panes_area = if self.tab_count() > 1 {
            let tabs_row = Rect::new(panes_area.x, panes_area.y, panes_area.width, 1);
            self.draw_tabs(frame, tabs_row);
            Rect::new(
                panes_area.x,
                panes_area.y + 1,
                panes_area.width,
                panes_area.height.saturating_sub(1),
            )
        } else {
            panes_area
        };

        self.frames = self.compute_frames(panes_area);
        self.layout = self
            .frames
            .iter()
            .map(|(id, frame)| (*id, Self::interior(*frame)))
            .collect();
        self.draw_panes(frame);
        self.draw_status(frame, area);

        self.draw_overlay(frame, panes_area);
    }

    /// Draws whichever overlay is open, if any.
    fn draw_overlay(&mut self, frame: &mut Frame<'_>, panes_area: Rect) {
        let Some(overlay) = &self.overlay else {
            return;
        };

        if let Some(picker) = overlay.picker() {
            frame.render_widget(picker, panes_area);
            return;
        }

        if let Overlay::Browse(browser) = overlay {
            frame.render_widget(browser, panes_area);
            return;
        }

        let Overlay::Approval { scroll } = overlay else {
            return;
        };
        let scroll = *scroll;

        let rect = centred_approval(panes_area);

        // The queue can only be empty here for one frame, between the last
        // request being answered and `open_next_approval` closing the
        // overlay; nothing to draw is not a bug worth a fallback screen for.
        let Some(measured) = self.approval_widget(scroll) else {
            return;
        };

        // Clamped here rather than where `↓` recorded it: this is the one
        // place that knows the box's real inner size, this frame. Persisted
        // back so the next `↓`/`↑` starts from what is actually on screen,
        // not from an offset that keystroke-time clamping never saw. Measured
        // with its own widget, rebuilt below for the render itself, because
        // both borrow `self` and the persist in between needs it back.
        let inner = Approval::inner(rect);
        let max = measured
            .total_rows(inner.width)
            .saturating_sub(inner.height);
        let clamped = scroll.min(max);

        if clamped != scroll
            && let Some(Overlay::Approval { scroll }) = &mut self.overlay
        {
            *scroll = clamped;
        }

        let Some(widget) = self.approval_widget(clamped) else {
            return;
        };

        Clear.render(rect, frame.buffer_mut());
        frame.render_widget(widget, rect);
    }

    /// The panes to tile this frame.
    ///
    /// Children are left out unless the user has opened them: ten subagents
    /// would otherwise shrink every pane to nothing. Which rows are open is
    /// this client's business, so the daemon is never told.
    ///
    /// A pane whose process has exited leaves the grid at once and the others
    /// spread into its place. It stays in the sidebar, where selecting it shows
    /// what it printed — the output is worth keeping, the floor space is not.
    fn tileable(&self) -> Vec<PaneId> {
        let mut ordered = Vec::new();

        // A tree walk rather than the order panes were created in. An opened
        // subagent has to sit next to the pane that asked for it — that is what
        // `^a s` is for — and with a grid that holds four, creation order can
        // put a parent on one tab and its child on the next.
        for pane in self.state.visible_panes() {
            if pane.parent.is_some() || !pane.status.is_live() {
                continue;
            }

            ordered.push(pane.id);
            self.push_opened_children(pane.id, &mut ordered);
        }

        // A subagent whose parent is a tombstone — closed, but kept because
        // this work outlived it — has no parent row to follow and would drop
        // out of the grid entirely.
        for pane in self.state.visible_panes() {
            if pane.parent.is_some()
                && pane.status.is_live()
                && self.expanded.contains(&pane.id)
                && !ordered.contains(&pane.id)
            {
                ordered.push(pane.id);
            }
        }

        ordered
    }

    /// Appends `parent`'s opened, still-running descendants, deepest last.
    ///
    /// Guards against a pane reachable from itself: the tree comes from the
    /// daemon, and a cycle there would hang the interface rather than show a
    /// wrong pane.
    fn push_opened_children(&self, parent: PaneId, ordered: &mut Vec<PaneId>) {
        for child in self.state.children_of(parent) {
            if !child.status.is_live() || !self.expanded.contains(&child.id) {
                continue;
            }
            if ordered.contains(&child.id) {
                continue;
            }

            ordered.push(child.id);
            self.push_opened_children(child.id, ordered);
        }
    }

    /// How many tabs the tileable panes fill.
    ///
    /// Always at least one, so an empty project still has a tab to be on.
    fn tab_count(&self) -> usize {
        self.tileable().len().div_ceil(PANES_PER_TAB).max(1)
    }

    /// The tab on screen: the one holding the focused pane.
    ///
    /// Derived rather than stored, because a stored tab and the focus can
    /// disagree — a pane spawning, exiting, or being adopted from the daemon
    /// all move focus without going anywhere near a tab — and a view showing
    /// one tab while typing went to another would be the worst bug here.
    fn current_tab(&self) -> usize {
        let tileable = self.tileable();

        self.state
            .focused_pane()
            .and_then(|id| tileable.iter().position(|pane| *pane == id))
            .map_or(0, |index| index / PANES_PER_TAB)
    }

    /// The panes on the tab being shown.
    fn panes_on_tab(&self) -> Vec<PaneId> {
        self.tileable()
            .chunks(PANES_PER_TAB)
            .nth(self.current_tab())
            .map(<[PaneId]>::to_vec)
            .unwrap_or_default()
    }

    /// Which tile each visible pane gets this frame, border included.
    fn compute_frames(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let visible = self.panes_on_tab();

        if let Some(zoomed) = self.state.zoomed_pane()
            && visible.contains(&zoomed)
        {
            return vec![(zoomed, tile_zoomed(area))];
        }

        visible
            .iter()
            .copied()
            .zip(tile(visible.len(), area))
            .collect()
    }

    /// The area inside a tile's border, which is what the pane itself owns.
    fn interior(frame: Rect) -> Rect {
        pane_block(false).inner(frame)
    }

    fn draw_panes(&mut self, frame: &mut Frame<'_>) {
        let focused = self.state.focused_pane();
        let mut cursor = None;

        for ((id, outer), (_, inner)) in self.frames.iter().zip(&self.layout) {
            let Some(pane) = self.panes.get(id) else {
                continue;
            };

            let is_focused = focused == Some(*id);

            // Drawn before the contents, and over the whole tile, so the border
            // is what separates one agent's output from the next and from the
            // sidebar. Without it two panes of similarly-coloured text read as
            // one pane with a very confusing wrap.
            frame.render_widget(
                pane_block(is_focused).title(pane_title(&self.state, *id)),
                *outer,
            );

            let widget = PaneWidget::new(&pane.screen).focused(is_focused);

            if let Some(position) = widget.cursor_position(*inner) {
                cursor = Some(position);
            }

            frame.render_widget(widget, *inner);
        }

        // Placing the real cursor is what makes typing feel native rather
        // than like editing a picture of a terminal.
        if let Some((x, y)) = cursor {
            frame.set_cursor_position((x, y));
        }
    }

    /// Draws the row of tabs above the grid.
    fn draw_tabs(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.height == 0 {
            return;
        }

        let current = self.current_tab();
        let mut spans = Vec::new();

        for index in 0..self.tab_count() {
            let panes = self
                .tileable()
                .chunks(PANES_PER_TAB)
                .nth(index)
                .map_or(0, <[PaneId]>::len);

            let style = if index == current {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            spans.push(ratatui::text::Span::styled(
                format!(" {} ({panes}) ", index + 1),
                style,
            ));
        }

        Paragraph::new(ratatui::text::Line::from(spans)).render(area, frame.buffer_mut());
    }

    fn draw_status(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.height == 0 {
            return;
        }

        let row = Rect::new(area.x, area.y + area.height - 1, area.width, 1);

        // Derived live from the queue rather than stamped once when `Esc`
        // defers: a stamped string can be clobbered by any later write to
        // `self.status`, and a deadline the daemon enforces means a deferral
        // whose reminder went missing is decided by inaction — the one
        // outcome this whole feature exists to prevent. Silent while the
        // prompt itself is on screen, since it would only repeat what is
        // already in front of the user.
        //
        // Appended rather than shown in place of `self.status`: a daemon
        // disconnect or an error is worth knowing about more than a queued
        // prompt is, and when the daemon is gone the prompt cannot be acted
        // on anyway, so hiding the disconnect notice behind it would be
        // exactly backwards.
        let waiting_reminder = (!self.pending.is_empty()
            && !matches!(self.overlay, Some(Overlay::Approval { .. })))
        .then(|| format!("{} delegation(s) waiting — ^a a", self.pending.len()));

        // Read from the `Device.reachable` `sync_attachment` stamps each poll,
        // not asked live: this runs every frame, and re-checking the
        // connection here would mean answering the same question twice — the
        // sidebar and the refusal path already read this same field, and
        // disagreeing with them is worse than being a tick stale. Named,
        // because on a fleet "the daemon" says nothing about which machine
        // went.
        let unreachable: Vec<String> = self
            .state
            .devices()
            .iter()
            .filter(|device| !device.reachable)
            .map(|device| device.name.clone())
            .collect();

        let text = if self.router.is_armed() {
            // A prefix that armed invisibly is how a keystroke goes missing
            // with no explanation.
            "PREFIX".to_string()
        } else {
            let base = if !unreachable.is_empty() {
                // Ahead of `self.status`, which may still hold whatever was
                // happening when the connection went: a user needs to know the
                // agents are out of reach more than they need the last message.
                format!(
                    "waiting for {} — its agents are still running",
                    unreachable.join(", ")
                )
            } else if !self.status.is_empty() {
                self.status.clone()
            } else {
                let panes = self.state.visible_panes().len();
                let tabs = if self.tab_count() > 1 {
                    format!(
                        "  tab {}/{}  ^a 1-9",
                        self.current_tab() + 1,
                        self.tab_count()
                    )
                } else {
                    String::new()
                };
                // Attached is worth saying: it is the difference between
                // closing Dispatch and killing the agents.
                let where_ = self
                    .device()
                    .map_or_else(String::new, |device| format!("  {device}"));
                format!(
                    "{panes} pane(s){where_}{tabs}  ^a n new  ^a x close  ^a z zoom  ^a s child  ^a c collapse  ^a q quit"
                )
            };

            match waiting_reminder {
                Some(reminder) => format!("{base}  {reminder}"),
                None => base,
            }
        };

        let style = if self.router.is_armed() {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        Paragraph::new(text)
            .style(style)
            .render(row, frame.buffer_mut());
    }

    /// Resizes every visible pane to the rectangle it now occupies.
    ///
    /// A child that is not told its new size redraws to the old one, which is
    /// the most visible bug this layer can have.
    pub fn resize_panes(&mut self) {
        let layout = self.layout.clone();

        for (id, rect) in layout {
            // Silently, unlike a keystroke: this runs every frame, and a
            // machine that is down would otherwise rewrite the status line at
            // the frame rate. The pane keeps its old geometry until the
            // machine answers again, when the mismatch this leaves behind
            // resizes it on the next frame.
            if !self.can_reach_pane(id) {
                continue;
            }

            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };

            let size = Size::new(rect.width, rect.height);
            if size == pane.backend.size() {
                continue;
            }

            if let Err(error) = pane.backend.resize(size) {
                tracing::warn!(%error, "failed to resize a pane");
                continue;
            }

            if let Ok(screen) = pane.reader.read(pane.backend.terminal()) {
                pane.screen = screen;
            }
        }
    }

    /// How long to wait for input before drawing again.
    #[must_use]
    pub fn poll_timeout(last_draw: Instant) -> Duration {
        FRAME.saturating_sub(last_draw.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, Sender};

    use dispatch_core::{PaneRole, Project, ProjectSource};

    /// An `App` attached to a daemon that says only what a test tells it to,
    /// with one project already announced.
    ///
    /// Messages go in through the same queue a real daemon's arrive on and are
    /// picked up by `poll_daemon`, so what these tests drive is the client's own
    /// message path rather than an `AppState` built by hand — which is how a
    /// `PaneSpawned` field the client threw away passed every test there was.
    /// The outbox `Receiver` comes back rather than being dropped because a
    /// client whose outbox has no reader reports itself disconnected on the
    /// first send.
    fn attached_app() -> (
        App,
        ProjectId,
        Sender<ServerMessage>,
        Receiver<ClientMessage>,
    ) {
        let (client, daemon, sent) = Client::for_test();
        let mut app = App::attached(HarnessRegistry::default(), client);

        let project = Project::new("/tmp/attached", ProjectSource::LocalDir);
        let id = project.id;
        daemon
            .send(ServerMessage::ProjectOpened { project })
            .expect("the app is listening");
        app.poll_daemon();

        (app, id, daemon, sent)
    }

    /// The announcement of a pane the daemon has started.
    fn spawned(
        pane: PaneId,
        project: ProjectId,
        harness: &str,
        parent: Option<PaneId>,
        durable: bool,
    ) -> ServerMessage {
        ServerMessage::PaneSpawned {
            pane,
            project,
            harness: harness.to_string(),
            parent,
            durable,
        }
    }

    /// The column a row's title starts in, for comparing one row's indentation
    /// against another's.
    ///
    /// Counted in characters rather than bytes: the status dot and the focus
    /// marker are three bytes each, so a byte offset would make a focused row
    /// look indented further than an unfocused one at the same depth.
    /// Just the sidebar's columns of one rendered row.
    ///
    /// A pane's border carries its title, so a whole row can name a harness
    /// twice: once in the sidebar and once around the pane running it. A test
    /// about the sidebar has to say so.
    fn sidebar_column(line: &str) -> String {
        line.chars().take(sidebar::WIDTH as usize).collect()
    }

    fn column_of(line: &str, needle: &str) -> usize {
        let byte = line
            .find(needle)
            .unwrap_or_else(|| panic!("expected {needle:?} in {line:?}"));
        line[..byte].chars().count()
    }

    /// Announces `count` top-level panes and returns their ids in order.
    fn spawn_several(
        app: &mut App,
        daemon: &Sender<ServerMessage>,
        project: ProjectId,
        count: usize,
    ) -> Vec<PaneId> {
        let ids: Vec<PaneId> = (0..count).map(|_| PaneId::new()).collect();

        for id in &ids {
            daemon
                .send(spawned(*id, project, "shell", None, false))
                .expect("the app is listening");
        }
        app.poll_daemon();

        ids
    }

    #[test]
    fn an_opened_subagent_is_tiled_beside_the_pane_that_asked_for_it() {
        // `^a s` exists to put a subagent next to its parent. Creation order
        // would put it after every other pane, which with a grid of four means
        // a different tab — the one place it must never be.
        let (mut app, project, daemon, _outbox) = attached_app();
        let parents = spawn_several(&mut app, &daemon, project, 4);

        let child = PaneId::new();
        daemon
            .send(spawned(child, project, "claude", Some(parents[0]), true))
            .expect("the app is listening");
        app.poll_daemon();

        app.expanded.insert(child);

        assert_eq!(
            app.tileable(),
            vec![parents[0], child, parents[1], parents[2], parents[3]],
            "the child follows its parent rather than the last pane"
        );
        assert!(
            app.panes_on_tab().contains(&child),
            "so the two share a tab: {:?}",
            app.panes_on_tab()
        );
        assert!(
            app.panes_on_tab().contains(&parents[0]),
            "and the parent is on it too"
        );
    }

    #[test]
    fn a_subagent_that_outlived_its_parent_is_still_tiled() {
        // A blanket-approved subagent survives the pane that asked for it, and
        // the parent becomes a tombstone. Walking the tree from live parents
        // alone would drop the survivor out of the grid.
        let (mut app, project, daemon, _outbox) = attached_app();
        let parent = spawn_several(&mut app, &daemon, project, 1)[0];

        let child = PaneId::new();
        daemon
            .send(spawned(child, project, "claude", Some(parent), true))
            .expect("the app is listening");
        app.poll_daemon();
        app.expanded.insert(child);

        let _ = app.state.close_pane(parent);

        assert!(
            app.tileable().contains(&child),
            "the survivor keeps its place in the grid, got {:?}",
            app.tileable()
        );
    }

    #[test]
    fn a_fifth_pane_opens_a_second_tab_rather_than_shrinking_the_other_four() {
        // Past four, every pane is too narrow for a wrapped line of code and
        // too short for a prompt and its answer.
        let (mut app, project, daemon, _outbox) = attached_app();
        let ids = spawn_several(&mut app, &daemon, project, 5);

        assert_eq!(app.tab_count(), 2, "five panes need a second tab");
        assert_eq!(
            app.panes_on_tab().len(),
            1,
            "the fifth pane is alone on the tab it opened"
        );
        assert_eq!(
            app.current_tab(),
            1,
            "and the view followed it, because spawning focused it"
        );

        app.select_tab(0);
        assert_eq!(
            app.panes_on_tab(),
            ids[..4].to_vec(),
            "the first tab holds exactly the first four"
        );
    }

    #[test]
    fn the_tab_shown_is_the_one_holding_the_focused_pane() {
        // The view is derived from the focus rather than stored beside it, so
        // that no path can move one without the other.
        let (mut app, project, daemon, _outbox) = attached_app();
        let ids = spawn_several(&mut app, &daemon, project, 6);

        app.focus_pane(ids[0]);
        assert_eq!(app.current_tab(), 0);

        app.focus_pane(ids[5]);
        assert_eq!(
            app.current_tab(),
            1,
            "focusing across the cap moves the view"
        );
    }

    #[test]
    fn a_pane_that_exited_gives_its_tile_back() {
        // Typing `/exit` in an agent should hand the floor space to the panes
        // still working, while leaving the transcript in the sidebar.
        let (mut app, project, daemon, _outbox) = attached_app();
        let ids = spawn_several(&mut app, &daemon, project, 2);

        assert_eq!(app.tileable().len(), 2);

        daemon
            .send(ServerMessage::PaneChanged {
                pane: ids[0],
                update: PaneUpdate::Status {
                    status: PaneStatus::Exited(0),
                },
            })
            .expect("the app is listening");
        app.poll_daemon();

        assert_eq!(
            app.tileable(),
            vec![ids[1]],
            "the exited pane is out of the grid"
        );
        assert!(
            app.state
                .visible_panes()
                .iter()
                .any(|pane| pane.id == ids[0]),
            "but still listed, because its output is worth reading"
        );
    }

    #[test]
    fn a_daemons_subagent_is_nested_kept_out_of_the_grid_and_outlives_its_parent() {
        // The one test whose absence let the client throw `parent` away and
        // never learn `durable` at all: everything else about nesting was
        // proved against an `AppState` assembled in the test itself, which no
        // amount of dropping on the wire could break.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (mut app, project, daemon, _sent) = attached_app();
        let parent = PaneId::new();
        let child = PaneId::new();

        for message in [
            spawned(parent, project, "claude", None, true),
            spawned(child, project, "codex", Some(parent), true),
        ] {
            daemon.send(message).expect("the app is listening");
        }
        app.poll_daemon();

        let adopted = app.state.pane(child).expect("the child was adopted");
        assert_eq!(adopted.parent, Some(parent), "the parent link survives");
        assert!(adopted.durable, "and so does the blanket approval");
        assert_eq!(
            app.state.pane(parent).map(|pane| pane.role),
            Some(PaneRole::Orchestrator),
            "a pane whose child was approved is an orchestrator"
        );

        // Nested in the sidebar: one row below its parent, indented past it.
        let mut terminal =
            Terminal::new(TestBackend::new(100, 30)).expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");
        let drawn = rendered_text(&terminal);
        let lines: Vec<&str> = drawn.lines().collect();

        let sidebar: Vec<String> = lines.iter().map(|line| sidebar_column(line)).collect();
        let parent_row = sidebar
            .iter()
            .position(|line| line.contains("claude"))
            .expect("the parent has a row");
        let child_row = sidebar
            .iter()
            .position(|line| line.contains("codex"))
            .expect("the child has a row");
        assert_eq!(
            child_row,
            parent_row + 1,
            "the child belongs under its parent:\n{drawn}"
        );
        assert_eq!(
            column_of(&sidebar[child_row], "codex"),
            column_of(&sidebar[parent_row], "claude") + 2,
            "and indented one level in from it:\n{drawn}"
        );

        // Not tiled until the user opens it: ten subagents must not shrink the
        // grid to nothing.
        assert_eq!(
            app.tileable(),
            vec![parent],
            "a subagent stays out of the grid until it is opened"
        );
        app.focus_pane(child);
        assert_eq!(
            app.tileable(),
            vec![parent, child],
            "opening it brings it in"
        );

        // And the tombstone rule, which can only fire because `durable`
        // travelled: closing the parent keeps its row over a live child.
        daemon
            .send(ServerMessage::PaneClosed { pane: parent })
            .expect("the app is listening");
        app.poll_daemon();

        assert!(
            app.state.pane(parent).is_some_and(|pane| pane.closed),
            "the parent stays as a tombstone over live blanket-approved work"
        );
        assert!(
            app.state.pane(child).is_some(),
            "and the subagent keeps running"
        );
    }

    #[test]
    fn a_subagents_row_is_titled_with_its_task_not_its_harness() {
        // Four children of one pane all reading "Claude Code" say nothing about
        // which is which. The client has the task in hand from the prompt it
        // showed, and the daemon resolves a request before announcing the pane.
        let (mut app, project, daemon, _sent) = attached_app();
        let parent = PaneId::new();
        let child = PaneId::new();
        let request = RequestId::new();

        daemon
            .send(spawned(parent, project, "claude", None, true))
            .expect("the app is listening");
        app.poll_daemon();

        app.pending.push_back(PendingRequest {
            request,
            parent,
            project,
            harness: "claude".into(),
            task: "write the tests for the http client".into(),
            depth: 0,
        });

        for message in [
            ServerMessage::DelegateResolved {
                request,
                outcome: DelegateOutcome::Approved { pane: child },
            },
            spawned(child, project, "claude", Some(parent), false),
        ] {
            daemon.send(message).expect("the app is listening");
        }
        app.poll_daemon();

        let title = app
            .state
            .pane(child)
            .map(|pane| pane.title.clone())
            .expect("the child was adopted");
        assert!(
            title.starts_with("write the tests"),
            "the row should open with the task's own words, got {title:?}"
        );

        // The title scanner still owns the name from here on.
        app.apply(ServerMessage::PaneChanged {
            pane: child,
            update: PaneUpdate::Title {
                title: "pytest".into(),
            },
        });
        assert_eq!(
            app.state.pane(child).map(|pane| pane.title.as_str()),
            Some("pytest"),
            "what the agent calls itself replaces the task"
        );
    }

    #[test]
    fn a_pane_the_daemon_did_not_delegate_is_titled_with_its_harness() {
        // The task title is for subagents. A top-level pane is still named
        // after what is running in it.
        let (mut app, project, daemon, _sent) = attached_app();
        let pane = PaneId::new();

        daemon
            .send(spawned(pane, project, "codex", None, true))
            .expect("the app is listening");
        app.poll_daemon();

        assert_eq!(
            app.state.pane(pane).map(|pane| pane.title.as_str()),
            Some("codex")
        );
    }

    #[test]
    fn an_unknown_pane_update_is_ignored_rather_than_fatal() {
        // `PaneUpdate` travels inside `PaneChanged`, so a newer daemon's extra
        // variant must land somewhere ignorable rather than failing the frame
        // and taking the connection down with it.
        let (mut app, project, daemon, _sent) = attached_app();
        let pane = PaneId::new();

        daemon
            .send(spawned(pane, project, "claude", None, true))
            .expect("the app is listening");
        app.poll_daemon();

        assert!(!app.apply(ServerMessage::PaneChanged {
            pane,
            update: PaneUpdate::Unknown,
        }));
        assert!(
            app.state.pane(pane).is_some(),
            "the pane is untouched by an update this build cannot read"
        );
    }

    #[test]
    fn a_task_title_keeps_whole_words_and_says_when_it_cut() {
        assert_eq!(
            task_title("write the tests"),
            Some("write the tests".into())
        );
        assert_eq!(
            task_title("write the tests for the http client"),
            Some("write the tests for…".into()),
            "a long task is cut between words, and says so"
        );
        assert_eq!(
            task_title(&"x".repeat(80)),
            Some(format!("{}…", "x".repeat(TITLE_BUDGET - 1))),
            "a first word with nowhere to break is cut anyway"
        );
        assert_eq!(
            task_title("   \n  "),
            None,
            "a task with no words at all has no title to give"
        );
    }

    /// An `App` with one project, one parent pane, and one queued delegation
    /// request from it, with the approval prompt already open.
    fn app_with_one_pending() -> (App, RequestId) {
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = app
            .state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let request = RequestId::new();
        app.pending.push_back(PendingRequest {
            request,
            parent,
            project,
            harness: "claude".into(),
            task: "write the tests".into(),
            depth: 0,
        });
        app.overlay = Some(Overlay::Approval { scroll: 0 });

        (app, request)
    }

    #[test]
    fn the_sidebar_marks_a_pane_with_its_harnesss_icon() {
        // The registry is the client's, so the sidebar only has icons if the
        // client hands them over.
        let def = dispatch_config::HarnessDef {
            id: "shell".to_string(),
            display_name: "Shell".to_string(),
            icon: Some("\u{f0e7}".to_string()),
            ..dispatch_config::HarnessDef::default()
        };
        let mut app = App::new([def].into_iter().collect());

        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        app.state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("the frame is drawn");

        assert!(
            rendered_text(&terminal).contains('\u{f0e7}'),
            "the harness icon reaches the sidebar"
        );
    }

    /// An app with one project and one pane of `harness`, named `display`.
    fn app_with_a_pane(root: &str, harness: &str, display: &str) -> (App, PaneId) {
        let def = dispatch_config::HarnessDef {
            id: harness.to_string(),
            display_name: display.to_string(),
            ..dispatch_config::HarnessDef::default()
        };
        let mut app = App::new([def].into_iter().collect());

        let project = app
            .state
            .add_project(Project::new(root, ProjectSource::LocalDir));
        let pane = app
            .state
            .spawn_pane(project, HarnessId::new(harness))
            .expect("the project exists");

        (app, pane)
    }

    #[test]
    fn an_agents_own_mark_is_stripped_from_the_title_it_sets() {
        // Claude Code announces itself as "✳ Claude Code". The sidebar draws
        // the harness icon beside the row already, so the row showed two marks
        // for one agent.
        let (mut app, pane) = app_with_a_pane("/tmp/one", "claude", "Claude Code");

        app.rename(pane, "✳ Claude Code");

        assert_eq!(
            app.state.pane(pane).map(|p| p.title.as_str()),
            Some("Claude Code")
        );
    }

    #[test]
    fn a_title_that_only_repeats_the_project_names_the_harness_instead() {
        // Codex titles its terminal after the working directory, so every one
        // of its panes was called the same thing as the project above it.
        let (mut app, pane) = app_with_a_pane("/tmp/Dispatch", "codex", "Codex");

        app.rename(pane, "Dispatch");

        assert_eq!(
            app.state.pane(pane).map(|p| p.title.as_str()),
            Some("Codex")
        );
    }

    #[test]
    fn a_title_an_agent_actually_chose_is_left_alone() {
        let (mut app, pane) = app_with_a_pane("/tmp/one", "claude", "Claude Code");

        app.rename(pane, "fixing the sidebar");

        assert_eq!(
            app.state.pane(pane).map(|p| p.title.as_str()),
            Some("fixing the sidebar")
        );
    }

    #[test]
    fn a_title_with_nothing_left_in_it_names_the_harness() {
        let (mut app, pane) = app_with_a_pane("/tmp/one", "claude", "Claude Code");

        app.rename(pane, "✳ ");

        assert_eq!(
            app.state.pane(pane).map(|p| p.title.as_str()),
            Some("Claude Code")
        );
    }

    /// Presses the prefix, then `key`.
    fn command(app: &mut App, key: char) {
        app.handle(
            &Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Size::new(100, 30),
        )
        .expect("a keystroke is handled");
        press(app, KeyCode::Char(key));
    }

    /// A directory holding `dirs`, cleaned up when the test ends.
    struct Tree(PathBuf);

    impl Tree {
        fn new(label: &str, dirs: &[&str]) -> Self {
            let root = scratch(label);
            for dir in dirs {
                std::fs::create_dir_all(root.join(dir)).expect("temp dir is writable");
            }
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_browser_opens_on_a_directory_and_closes_on_escape() {
        let tree = Tree::new("browse-escape", &["alpha"]);
        let mut app = App::new(HarnessRegistry::default());
        app.browse_from(tree.path());

        command(&mut app, 'o');
        assert!(
            matches!(app.overlay, Some(Overlay::Browse(_))),
            "the browser is open"
        );

        press(&mut app, KeyCode::Esc);
        assert!(app.overlay.is_none(), "and escape closes it");
    }

    #[test]
    fn choosing_a_directory_in_the_browser_opens_it_as_a_project() {
        let dir = scratch("browse-open-kept");
        let tree = Tree::new("browse-open", &["alpha"]);
        let mut app = App::new(HarnessRegistry::default());
        app.keep_projects_in(&dir);
        app.browse_from(tree.path());

        command(&mut app, 'o');
        press(&mut app, KeyCode::Enter);

        assert_eq!(
            app.state
                .projects()
                .iter()
                .map(|p| p.root.clone())
                .collect::<Vec<_>>(),
            [tree.path().join("alpha")],
            "the directory is a project now"
        );
        assert_eq!(
            dispatch_config::projects::load(&dir).expect("it reads back"),
            [tree.path().join("alpha")],
            "and it is kept like any other"
        );
        assert!(app.overlay.is_none(), "the browser is done");
    }

    #[test]
    fn the_open_browser_is_drawn_over_the_grid() {
        let tree = Tree::new("browse-drawn", &["alpha"]);
        let mut app = App::new(HarnessRegistry::default());
        app.state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        app.browse_from(tree.path());

        command(&mut app, 'o');

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("the frame is drawn");

        let text = rendered_text(&terminal);
        assert!(text.contains("Open project"), "{text}");
        assert!(text.contains("alpha"), "{text}");
    }

    #[test]
    fn typing_in_the_browser_filters_rather_than_reaching_a_pane() {
        // Every plain letter belongs to the browser while it is open, or a
        // filter would run commands in whatever pane was focused.
        let tree = Tree::new("browse-filter", &["alpha", "beta"]);
        let mut app = App::new(HarnessRegistry::default());
        app.browse_from(tree.path());

        command(&mut app, 'o');
        press(&mut app, KeyCode::Char('b'));

        let Some(Overlay::Browse(browser)) = &app.overlay else {
            panic!("the browser is open");
        };
        assert_eq!(browser.input(), "b");
        assert_eq!(
            browser.selected().map(|entry| entry.label.clone()),
            Some("beta".into())
        );
    }

    #[test]
    fn the_browser_walks_into_a_directory_and_back_out() {
        let tree = Tree::new("browse-walk", &["outer/inner"]);
        let mut app = App::new(HarnessRegistry::default());
        app.browse_from(tree.path());

        command(&mut app, 'o');
        press(&mut app, KeyCode::Right);

        let Some(Overlay::Browse(browser)) = &app.overlay else {
            panic!("the browser is open");
        };
        assert_eq!(browser.dir(), tree.path().join("outer"));

        press(&mut app, KeyCode::Left);
        let Some(Overlay::Browse(browser)) = &app.overlay else {
            panic!("the browser is open");
        };
        assert_eq!(browser.dir(), tree.path());
    }

    #[test]
    fn a_typed_path_opens_that_directory_as_the_project() {
        let dir = scratch("browse-typed-kept");
        let tree = Tree::new("browse-typed", &["outer/inner"]);
        let mut app = App::new(HarnessRegistry::default());
        app.keep_projects_in(&dir);
        app.browse_from(tree.path());

        command(&mut app, 'o');
        for c in tree.path().join("outer").display().to_string().chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);

        assert_eq!(
            app.state
                .projects()
                .iter()
                .map(|p| p.root.clone())
                .collect::<Vec<_>>(),
            [tree.path().join("outer")]
        );
    }

    #[test]
    fn a_typed_path_that_is_not_there_says_so_and_stays_open() {
        let tree = Tree::new("browse-typed-bad", &["alpha"]);
        let mut app = App::new(HarnessRegistry::default());
        app.browse_from(tree.path());

        command(&mut app, 'o');
        for c in "/nowhere/at/all".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);

        assert!(app.state.projects().is_empty());
        assert!(
            matches!(app.overlay, Some(Overlay::Browse(_))),
            "the browser stays open to fix the path"
        );
        assert!(app.status.contains("no such directory"), "{:?}", app.status);
    }

    #[test]
    fn folding_from_the_keyboard_folds_the_focused_panes_subagents() {
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = app
            .state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        let mut child = dispatch_core::Pane::new(project, HarnessId::new("shell"));
        child.parent = Some(parent);
        app.state.adopt_pane(child).expect("the project exists");
        let _ = app.state.focus(parent);

        command(&mut app, 'f');

        assert!(app.state.is_pane_collapsed(parent));
        assert!(
            !app.state.is_project_collapsed(project),
            "the project is not what was folded"
        );

        command(&mut app, 'f');
        assert!(!app.state.is_pane_collapsed(parent), "and it unfolds again");
    }

    #[test]
    fn folding_a_pane_with_no_subagents_folds_its_project() {
        // Otherwise the key does nothing on the common case: most panes have
        // no children, and the row above them is what there is to fold.
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let pane = app
            .state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        let _ = app.state.focus(pane);

        command(&mut app, 'f');

        assert!(app.state.is_project_collapsed(project));
    }

    #[test]
    fn folding_with_no_pane_focused_folds_the_selected_project() {
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));

        command(&mut app, 'f');

        assert!(app.state.is_project_collapsed(project));
    }

    #[test]
    fn folding_past_a_folded_project_folds_its_machine_and_back_out_again() {
        // The cycle: fold the project, then the machine it is on, then one
        // more press drops both at once -- landing on fully expanded rather
        // than stranding either fold with no way back from the keyboard.
        let mut app = App::new(HarnessRegistry::default());
        let device = app.state.add_device(Device::new("laptop"));
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir).with_device(device));
        let pane = app
            .state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        let _ = app.state.focus(pane);

        let states = |app: &App| {
            (
                app.state.is_pane_collapsed(pane),
                app.state.is_project_collapsed(project),
                app.state.is_device_collapsed(device),
            )
        };

        assert_eq!(
            states(&app),
            (false, false, false),
            "fully expanded to start"
        );

        command(&mut app, 'f');
        assert_eq!(states(&app), (false, true, false), "fold the project");

        command(&mut app, 'f');
        assert_eq!(states(&app), (false, true, true), "and then the machine");

        command(&mut app, 'f');
        assert_eq!(
            states(&app),
            (false, false, false),
            "one press drops both -- back to fully expanded"
        );

        command(&mut app, 'f');
        assert_eq!(
            states(&app),
            (false, true, false),
            "and the cycle starts over"
        );
    }

    #[test]
    fn folding_on_one_machine_is_a_straight_fold_and_unfold() {
        // `App::new` already registers this machine as a device, so on its
        // own that is a single-device fleet -- the sidebar draws no device
        // row for it and never consults `is_device_collapsed` when deciding
        // what to draw. Stepping the device rung anyway, as the federated
        // ladder does, folds something invisible: the second press changes
        // nothing on screen and the cycle needs a third press to unfold.
        // Before federation `^a f` was a plain fold/unfold, and that is what
        // one machine has to keep being.
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let pane = app
            .state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        let _ = app.state.focus(pane);

        assert_eq!(app.state.devices().len(), 1, "one machine, to start");

        command(&mut app, 'f');
        assert!(app.state.is_project_collapsed(project), "first press folds");

        command(&mut app, 'f');
        assert!(
            !app.state.is_project_collapsed(project),
            "second press unfolds -- there is no device rung visible to step"
        );
    }

    #[test]
    fn a_pane_that_exits_quietly_still_redraws_the_grid() {
        // An agent's last output and its exit rarely land in the same poll:
        // `/exit` prints its goodbye, that poll redraws, and the process is
        // gone by the next one with nothing left to read. A poll that reports
        // no change leaves the dead pane holding its tile until some unrelated
        // keystroke forces a frame — which is what "press Enter again to close
        // it" was.
        let (mut app, project, daemon, _sent) = attached_app();
        let pane = PaneId::new();
        daemon
            .send(spawned(pane, project, "shell", None, false))
            .expect("the app is listening");
        app.poll_daemon();

        let Some(Backend::Remote(remote)) = app.panes.get_mut(&pane).map(|p| &mut p.backend) else {
            panic!("the pane is a remote one");
        };
        remote.set_state(RunState::Exited(0));

        assert!(app.poll_panes(), "the exit is worth a frame");
        assert_eq!(
            app.state.pane(pane).map(|p| p.status),
            Some(PaneStatus::Exited(0))
        );
    }

    #[test]
    fn a_pane_that_is_still_dead_is_not_worth_another_frame() {
        // The status is set on every poll, so reporting a change each time
        // would redraw the whole screen forever at the frame rate.
        let (mut app, project, daemon, _sent) = attached_app();
        let pane = PaneId::new();
        daemon
            .send(spawned(pane, project, "shell", None, false))
            .expect("the app is listening");
        app.poll_daemon();

        let Some(Backend::Remote(remote)) = app.panes.get_mut(&pane).map(|p| &mut p.backend) else {
            panic!("the pane is a remote one");
        };
        remote.set_state(RunState::Exited(0));

        assert!(app.poll_panes());
        assert!(!app.poll_panes(), "nothing changed the second time");
    }

    /// Types one key at the app.
    fn press(app: &mut App, code: KeyCode) {
        app.handle(
            &Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            Size::new(100, 30),
        )
        .expect("a keystroke is handled");
    }

    /// Opens the project picker the way a user does: the prefix, then `p`.
    fn open_project_picker(app: &mut App) {
        app.handle(
            &Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Size::new(100, 30),
        )
        .expect("a keystroke is handled");
        press(app, KeyCode::Char('p'));
    }

    /// A scratch directory for a test that writes the kept-projects file.
    fn scratch(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let path = std::env::temp_dir().join(format!(
            "dispatch-app-{}-{label}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir is writable");
        path
    }

    #[test]
    fn an_opened_project_is_kept_until_it_is_dropped() {
        // The sidebar is the list of projects the user keeps, so opening one
        // is what puts it there and only dropping it takes it off.
        let dir = scratch("kept");
        let mut app = App::new(HarnessRegistry::default());
        app.keep_projects_in(&dir);

        app.add_project(PathBuf::from("/tmp/alpha"));

        assert_eq!(
            dispatch_config::projects::load(&dir).expect("it reads back"),
            [PathBuf::from("/tmp/alpha")]
        );
    }

    /// The detail text of every row the project picker is currently showing.
    fn picker_details(app: &App) -> Vec<String> {
        let Some(Overlay::Project(picker)) = &app.overlay else {
            panic!("the project picker should be open");
        };

        picker
            .items()
            .iter()
            .map(|item| item.detail.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn two_devices_with_a_same_named_project_get_distinguishable_rows() {
        // The same path on purpose -- a checkout mirrored at an identical
        // location on two machines -- so nothing but the device tells the two
        // rows apart. Two different paths would pass this test even without
        // the fix, since the paths alone would already differ.
        let (first, first_daemon, _first_sent) = Client::for_test();
        first.handle().rename_for_test("near");
        let (second, second_daemon, _second_sent) = Client::for_test();
        second.handle().rename_for_test("far");

        let mut app = App::new(HarnessRegistry::default());
        app.attach(first);
        app.attach(second);

        first_daemon
            .send(ServerMessage::ProjectOpened {
                project: Project::new("/checkout/project", ProjectSource::LocalDir),
            })
            .expect("the app is listening");
        second_daemon
            .send(ServerMessage::ProjectOpened {
                project: Project::new("/checkout/project", ProjectSource::LocalDir),
            })
            .expect("the app is listening");
        app.poll_daemon();

        open_project_picker(&mut app);
        let details = picker_details(&app);

        assert_eq!(details.len(), 2, "one row per project: {details:?}");
        assert_ne!(
            details[0], details[1],
            "two identically-rooted projects on different machines must read differently: {details:?}"
        );
        assert!(
            details[0].contains("near") && details[1].contains("far"),
            "and each row should say which machine it is: {details:?}"
        );
    }

    #[test]
    fn one_device_leaves_the_project_picker_unchanged() {
        // The common case, unaffected: with nothing to tell apart, the row is
        // just the path, exactly as it was before a second device existed.
        let mut app = App::new(HarnessRegistry::default());
        app.add_project(PathBuf::from("/tmp/alpha"));

        open_project_picker(&mut app);
        let details = picker_details(&app);

        assert_eq!(
            details,
            ["/tmp/alpha".to_string()],
            "one machine's row should carry nothing but the path"
        );
    }

    #[test]
    fn dropping_a_project_from_the_picker_forgets_it_for_good() {
        let dir = scratch("dropped");
        let mut app = App::new(HarnessRegistry::default());
        app.keep_projects_in(&dir);
        app.add_project(PathBuf::from("/tmp/alpha"));
        app.add_project(PathBuf::from("/tmp/beta"));

        open_project_picker(&mut app);
        // The picker opens on the first project.
        press(&mut app, KeyCode::Char('d'));

        assert_eq!(app.state.projects().len(), 1, "its row is gone");
        assert_eq!(
            dispatch_config::projects::load(&dir).expect("it reads back"),
            [PathBuf::from("/tmp/beta")],
            "and it is not there on the next start"
        );
    }

    #[test]
    fn a_project_with_panes_is_not_dropped() {
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        app.state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");

        open_project_picker(&mut app);
        press(&mut app, KeyCode::Char('d'));

        assert_eq!(app.state.projects().len(), 1, "it stays");
        assert!(
            app.status.contains("panes"),
            "and the user is told why: {:?}",
            app.status
        );
    }

    #[test]
    fn dropping_a_project_while_attached_asks_the_daemon() {
        // The daemon owns the project list a client is handed on every
        // subscribe, so dropping a row it still keeps would bring it back.
        let (mut app, project, daemon, sent) = attached_app();
        daemon
            .send(ServerMessage::ProjectOpened {
                project: Project::new("/tmp/second", ProjectSource::LocalDir),
            })
            .expect("the app is listening");
        app.poll_daemon();

        open_project_picker(&mut app);
        press(&mut app, KeyCode::Char('d'));

        let asked = std::iter::from_fn(|| sent.try_recv().ok())
            .any(|m| matches!(m, ClientMessage::CloseProject { project: p } if p == project));
        assert!(asked, "the daemon is asked to close it");
        assert_eq!(
            app.state.projects().len(),
            2,
            "and the row stays until the daemon answers"
        );
    }

    #[test]
    fn a_project_the_daemon_closed_leaves_the_sidebar() {
        let (mut app, project, daemon, _sent) = attached_app();

        daemon
            .send(ServerMessage::ProjectClosed { project })
            .expect("the app is listening");
        app.poll_daemon();

        assert!(app.state.projects().is_empty());
    }

    /// A left-button press at `(column, row)`, as the terminal reports one.
    fn click(app: &mut App, column: u16, row: u16) {
        let event = Event::Mouse(dispatch_tui::input::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });

        app.handle(&event, Size::new(100, 30))
            .expect("a click is handled");
    }

    /// An app with two projects, a pane in the first, and a subagent under
    /// that pane — drawn once, so `sidebar_area` is the rectangle a click is
    /// resolved against.
    fn app_with_a_drawn_sidebar() -> (
        App,
        ratatui::Terminal<ratatui::backend::TestBackend>,
        ProjectId,
        PaneId,
        PaneId,
    ) {
        let mut app = App::new(HarnessRegistry::default());
        let first = app
            .state
            .add_project(Project::new("/tmp/first", ProjectSource::LocalDir));
        app.state
            .add_project(Project::new("/tmp/second", ProjectSource::LocalDir));

        let parent = app
            .state
            .spawn_pane(first, HarnessId::new("shell"))
            .expect("the project exists");

        let mut child = dispatch_core::Pane::new(first, HarnessId::new("shell"));
        child.parent = Some(parent);
        child.title = "subagent".into();
        let child = app.state.adopt_pane(child).expect("the project exists");

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("the frame is drawn");

        (app, terminal, first, parent, child)
    }

    #[test]
    fn a_click_on_a_project_row_selects_it_and_folds_its_panes() {
        let (mut app, _terminal, first, _parent, _child) = app_with_a_drawn_sidebar();

        // The first row inside the sidebar's frame is the first project.
        click(&mut app, 1, 1);

        assert_eq!(app.state.selected_project(), Some(first));
        assert!(app.state.is_project_collapsed(first), "and it folds");

        click(&mut app, 1, 1);
        assert!(!app.state.is_project_collapsed(first), "and unfolds again");
    }

    #[test]
    fn a_click_on_a_panes_twisty_folds_its_children_without_focusing_it() {
        let (mut app, _terminal, _first, parent, child) = app_with_a_drawn_sidebar();

        let _ = app.state.focus(child);
        // A pane row is indented two columns inside the frame, and its twisty
        // is the first of them.
        click(&mut app, 3, 2);

        assert!(app.state.is_pane_collapsed(parent));
        assert_eq!(
            app.state.focused_pane(),
            Some(child),
            "folding is not focusing"
        );
    }

    #[test]
    fn a_click_on_the_rest_of_a_pane_row_still_focuses_it() {
        let (mut app, _terminal, _first, parent, child) = app_with_a_drawn_sidebar();

        let _ = app.state.focus(child);
        click(&mut app, 8, 2);

        assert_eq!(app.state.focused_pane(), Some(parent));
        assert!(!app.state.is_pane_collapsed(parent), "and folds nothing");
    }

    /// Every cell of the last-drawn frame, as one string with a newline
    /// between rows.
    fn rendered_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A terminal sized so the approval prompt's own inner content area comes
    /// out to exactly `inner_width` columns by 18 rows — worked backwards
    /// through the sidebar's width and `centred_approval`'s
    /// `clamp(20, 76)`, then the block's one-cell border on each side.
    ///
    /// Below the clamp's floor of 20, the only way to reach a narrower outer
    /// width is through its own `.min(area.width)` escape hatch, which needs
    /// `area.width` — the panes area, sidebar already subtracted — to equal
    /// the target outer width exactly. At or above the floor, the ordinary
    /// `area.width - 4` path is what reaches it, so the panes area has to be
    /// four columns wider than the target instead.
    fn terminal_for_inner_width(
        inner_width: u16,
    ) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let outer = inner_width + 2;
        let panes_width = if outer >= 20 { outer + 4 } else { outer };
        let terminal_width = panes_width + sidebar::WIDTH;
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(terminal_width, 30))
            .expect("a test backend can be created")
    }

    #[test]
    fn a_delegated_pane_is_not_tiled_until_the_user_opens_it() {
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = app
            .state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut child = CorePane::new(project, HarnessId::new("claude"));
        child.parent = Some(parent);
        app.state.adopt_pane(child).expect("the project exists");

        assert_eq!(app.tileable(), vec![parent], "the child is left out");

        let child_id = app
            .state
            .children_of(parent)
            .first()
            .expect("the child is registered")
            .id;
        app.expanded.insert(child_id);

        assert_eq!(
            app.tileable(),
            vec![parent, child_id],
            "opening the child brings it into the grid"
        );
    }

    #[test]
    fn focusing_a_child_pane_is_how_it_gets_opened() {
        // The sidebar has no keyboard focus of its own, so a click on one of
        // its rows and a hover over an already-tiled child both end up here —
        // this is the one place `expanded` needs to gain an entry.
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = app
            .state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut child = CorePane::new(project, HarnessId::new("claude"));
        child.parent = Some(parent);
        let child_id = app.state.adopt_pane(child).expect("the project exists");

        app.focus_pane(child_id);

        assert!(app.expanded.contains(&child_id));
        assert_eq!(app.state.focused_pane(), Some(child_id));
    }

    #[test]
    fn the_keyboard_alone_can_reach_and_leave_a_child_pane() {
        // A subagent otherwise has no keyboard way in: the mouse is not
        // available over SSH without mouse reporting, and `focus_direction`
        // only ever searches the tiled grid, which by construction excludes
        // an unopened child. `^a s` and `^a c` are that way in and back out.
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = app
            .state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut first = CorePane::new(project, HarnessId::new("claude"));
        first.parent = Some(parent);
        let first_id = app.state.adopt_pane(first).expect("the project exists");

        let mut second = CorePane::new(project, HarnessId::new("claude"));
        second.parent = Some(parent);
        let second_id = app.state.adopt_pane(second).expect("the project exists");

        app.state.focus(parent).expect("the pane exists");

        let area = Size::new(80, 24);
        let prefix = Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        let s = Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));

        app.handle(&prefix, area).expect("handling never fails");
        app.handle(&s, area).expect("handling never fails");
        assert_eq!(
            app.state.focused_pane(),
            Some(first_id),
            "^a s should open and focus the first child"
        );
        assert!(app.expanded.contains(&first_id));

        app.handle(&prefix, area).expect("handling never fails");
        app.handle(&s, area).expect("handling never fails");
        assert_eq!(
            app.state.focused_pane(),
            Some(second_id),
            "a second ^a s should cycle to the next child"
        );

        app.handle(&prefix, area).expect("handling never fails");
        app.handle(&c, area).expect("handling never fails");
        assert_eq!(
            app.state.focused_pane(),
            Some(parent),
            "^a c should return focus to the parent"
        );
        assert!(
            !app.expanded.contains(&second_id),
            "collapsing should remove the child from the grid"
        );
    }

    #[test]
    fn expand_child_descends_into_a_grandchild_before_cycling_siblings() {
        // The daemon enforces a depth cap greater than one, so a grandchild
        // is a real pane that needs a real way in. Cycling siblings alone
        // only ever visits one generation; descending first is what makes a
        // second `^a s` reach it rather than the first child's sibling.
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = app
            .state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut child = CorePane::new(project, HarnessId::new("claude"));
        child.parent = Some(parent);
        let child_id = app.state.adopt_pane(child).expect("the project exists");

        let mut grandchild = CorePane::new(project, HarnessId::new("claude"));
        grandchild.parent = Some(child_id);
        let grandchild_id = app
            .state
            .adopt_pane(grandchild)
            .expect("the project exists");

        // A sibling of `child`, to prove descending is preferred over cycling
        // to it.
        let mut sibling = CorePane::new(project, HarnessId::new("claude"));
        sibling.parent = Some(parent);
        app.state.adopt_pane(sibling).expect("the project exists");

        app.state.focus(parent).expect("the pane exists");

        app.expand_child();
        assert_eq!(app.state.focused_pane(), Some(child_id));

        app.expand_child();
        assert_eq!(
            app.state.focused_pane(),
            Some(grandchild_id),
            "a child with its own child should be descended into, not cycled past"
        );
    }

    #[test]
    fn expand_child_skips_a_closed_sibling() {
        // `children_of` keeps a closed pane's row for a surviving durable
        // child's sake; `tileable` already excludes a closed pane from the
        // grid, and focusing one would land on a pane with nowhere to draw.
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = app
            .state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut doomed = CorePane::new(project, HarnessId::new("claude"));
        doomed.parent = Some(parent);
        let doomed_id = app.state.adopt_pane(doomed).expect("the project exists");

        // Kept alive so `doomed` survives as a tombstone (`closed: true`)
        // rather than being removed outright.
        let mut survivor = CorePane::new(project, HarnessId::new("claude"));
        survivor.parent = Some(doomed_id);
        survivor.durable = true;
        app.state.adopt_pane(survivor).expect("the project exists");

        let mut open_child = CorePane::new(project, HarnessId::new("claude"));
        open_child.parent = Some(parent);
        let open_child_id = app
            .state
            .adopt_pane(open_child)
            .expect("the project exists");

        app.state.close_pane(doomed_id).expect("the pane exists");
        assert!(
            app.state
                .pane(doomed_id)
                .expect("kept as a tombstone")
                .closed,
            "set up: the first child should be a tombstone, not gone"
        );

        app.state.focus(parent).expect("the pane exists");
        app.expand_child();

        assert_eq!(
            app.state.focused_pane(),
            Some(open_child_id),
            "the closed child should be skipped in favour of the open one"
        );
    }

    #[test]
    fn closing_a_pane_forgets_that_it_was_opened() {
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = app
            .state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut child = CorePane::new(project, HarnessId::new("claude"));
        child.parent = Some(parent);
        let child_id = app.state.adopt_pane(child).expect("the project exists");

        app.focus_pane(child_id);
        assert!(
            app.expanded.contains(&child_id),
            "set up: the child is open"
        );

        app.close_focused();

        assert!(
            !app.expanded.contains(&child_id),
            "a closed pane has nothing left to be expanded into"
        );
    }

    #[test]
    fn a_delegation_queued_behind_a_picker_appears_once_the_picker_closes() {
        // `DelegatePending` only opens the approval prompt when the keyboard is
        // free; a request that arrives mid-pick is queued silently rather than
        // yanking the keyboard out from under whatever the user was doing.
        // Once that picker is gone, nothing else will surface the request
        // except this.
        let mut app = App::new(HarnessRegistry::default());
        app.overlay = Some(Overlay::Harness(Picker::new("New pane", Vec::new())));
        app.pending.push_back(PendingRequest {
            request: RequestId::new(),
            parent: PaneId::new(),
            project: ProjectId::new(),
            harness: "claude".into(),
            task: "write the tests".into(),
            depth: 0,
        });

        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_overlay(&esc, Size::new(80, 24))
            .expect("handling an escape never fails");

        assert!(
            matches!(app.overlay, Some(Overlay::Approval { .. })),
            "the queued request should be shown now that the picker is gone"
        );
    }

    #[test]
    fn a_modified_key_is_never_an_approval() {
        // Ctrl-a is the prefix, the most-pressed combination in the program and
        // the first half of the chord that reopens this very prompt. If it
        // approves, the user grants a subagent by reaching for a command.
        let (mut app, _) = app_with_one_pending();

        let modified = [
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Up, KeyModifiers::ALT),
        ];

        for key in modified {
            app.handle_overlay(&Event::Key(key), Size::new(80, 24))
                .expect("handling a key never fails");
        }

        assert_eq!(
            app.pending.len(),
            1,
            "no modified key should have answered the request"
        );
        assert!(
            matches!(app.overlay, Some(Overlay::Approval { .. })),
            "the prompt should still be open"
        );
    }

    #[test]
    fn plain_a_approves_without_a_blanket() {
        let (mut app, request) = app_with_one_pending();

        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        app.handle_overlay(&Event::Key(key), Size::new(80, 24))
            .expect("handling a key never fails");

        assert!(app.pending.is_empty(), "the request should be answered");
        assert_eq!(
            app.sent,
            vec![ClientMessage::DelegateDecision {
                request,
                approve: true,
                blanket: false,
            }],
            "plain `a` should approve without a blanket"
        );
    }

    #[test]
    fn shift_a_approves_with_a_blanket() {
        let (mut app, request) = app_with_one_pending();

        let key = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        app.handle_overlay(&Event::Key(key), Size::new(80, 24))
            .expect("handling a key never fails");

        assert!(app.pending.is_empty(), "the request should be answered");
        assert_eq!(
            app.sent,
            vec![ClientMessage::DelegateDecision {
                request,
                approve: true,
                blanket: true,
            }],
            "`A` should approve every later request from this pane too"
        );
    }

    #[test]
    fn caps_lock_shaped_a_still_grants_a_blanket() {
        // Windows derives `SHIFT` from the physical key alone, but derives a
        // letter's case from `shift XOR caps lock`: with caps lock on, the
        // plain `a` key arrives as `(Char('A'), NONE)` — no modifier at all.
        // Requiring `SHIFT` for a blanket would leave this key doing nothing.
        let (mut app, request) = app_with_one_pending();

        let key = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE);
        app.handle_overlay(&Event::Key(key), Size::new(80, 24))
            .expect("handling a key never fails");

        assert_eq!(
            app.sent,
            vec![ClientMessage::DelegateDecision {
                request,
                approve: true,
                blanket: true,
            }],
            "an unmodified `A` should still grant a blanket"
        );
    }

    #[test]
    fn caps_lock_shaped_shift_a_approves_without_a_blanket() {
        // The other half of the same quirk: with caps lock on, physical
        // Shift-a arrives as `(Char('a'), SHIFT)` — lowercase, but modified.
        // Rejecting every modified key outright (Critical 1's first fix)
        // would have left this doing nothing too.
        let (mut app, request) = app_with_one_pending();

        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT);
        app.handle_overlay(&Event::Key(key), Size::new(80, 24))
            .expect("handling a key never fails");

        assert_eq!(
            app.sent,
            vec![ClientMessage::DelegateDecision {
                request,
                approve: true,
                blanket: false,
            }],
            "a caps-lock-shaped Shift-a should approve without a blanket"
        );
    }

    #[test]
    fn esc_defers_and_leaves_the_queue_intact() {
        let (mut app, request) = app_with_one_pending();

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        app.handle_overlay(&Event::Key(esc), Size::new(80, 24))
            .expect("handling an escape never fails");

        assert_eq!(
            app.pending.front().map(|waiting| waiting.request),
            Some(request),
            "Esc must not answer the request, only stop showing it"
        );
        assert!(app.overlay.is_none(), "the prompt should have closed");
    }

    #[test]
    fn the_status_line_shows_a_disconnect_notice_and_the_waiting_count_together() {
        // A user needs to know the daemon is gone more than they need to know
        // a prompt is queued — and when the daemon is gone, the queued prompt
        // cannot be acted on anyway. Neither should hide the other.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (mut app, _) = app_with_one_pending();
        app.overlay = None; // deferred, as `Esc` leaves it
        app.status = "waiting for the daemon — the agents are still running".into();

        let mut terminal =
            Terminal::new(TestBackend::new(100, 30)).expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        let buf = terminal.backend().buffer();
        let last_row: String = (0..buf.area.width)
            .filter_map(|x| buf.cell((x, buf.area.height - 1)))
            .map(|cell| cell.symbol())
            .collect();

        assert!(
            last_row.contains("waiting for the daemon"),
            "the disconnect notice must stay visible: {last_row}"
        );
        assert!(
            last_row.contains("delegation(s) waiting"),
            "the queued prompt should still be mentioned: {last_row}"
        );
    }

    #[test]
    fn a_withdrawal_closes_the_prompt_rather_than_advancing_it() {
        // The daemon broadcasts `DelegateResolved` to every subscribed
        // interface client, not only the one that answered — this is what
        // withdraws a prompt someone else just settled, or one that expired
        // on the daemon's own deadline.
        let (mut app, request) = app_with_one_pending();

        // A second request queued behind the first: if withdrawal substituted
        // it under the user's fingers, this is the one that would appear.
        app.pending.push_back(PendingRequest {
            request: RequestId::new(),
            parent: app.pending.front().expect("set up").parent,
            project: app.pending.front().expect("set up").project,
            harness: "claude".into(),
            task: "a second task".into(),
            depth: 0,
        });

        app.apply(ServerMessage::DelegateResolved {
            request,
            outcome: DelegateOutcome::Denied,
        });

        assert!(
            app.overlay.is_none(),
            "withdrawal should close the prompt, not show the next request"
        );
        assert_eq!(
            app.pending.len(),
            1,
            "the second request should still be queued, just not shown"
        );
    }

    #[test]
    fn a_long_task_on_one_line_can_be_scrolled_to_its_final_words() {
        // `Paragraph::scroll` counts *wrapped* rows, not `str::lines` — a task
        // delivered as a single long line (exactly what `dispatch delegate
        // "…"` sends) still wraps into several rows once rendered, so
        // clamping against logical lines left everything past the first
        // screenful unreachable. Approving something you cannot read is not
        // approval.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (mut app, _) = app_with_one_pending();
        let words: Vec<String> = (0..700).map(|i| format!("word{i}")).collect();
        let task = format!("start {} final-words-right-here", words.join(" "));
        app.pending.front_mut().expect("set up").task = task.clone();

        let mut terminal =
            Terminal::new(TestBackend::new(100, 30)).expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        // Scroll well past where the end could possibly be; the clamp should
        // stop it there rather than blank the box.
        for _ in 0..500 {
            app.scroll_approval(true);
        }

        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        let buf = terminal.backend().buffer();
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("final-words-right-here"),
            "scrolling should reach the end of a long task:\n{text}"
        );
    }

    #[test]
    fn ordinary_prose_at_the_boxs_default_width_can_be_scrolled_to_its_final_words() {
        // The row-count formula this scroll used to be clamped against
        // summed `ceil(line width / box width)` per line, which is not
        // actually a bound: greedy word-wrap can waste up to a whole row's
        // width of columns when the next word will not fit. Measured against
        // this same widget, that undercounted at a box width of 74 — this
        // prompt's own default on any terminal 80 columns or wider — with
        // twelve-character words, cutting the box off before its own last
        // line. The fix computes no count at all, so this is regression
        // coverage for that exact shape, not proof of a formula.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (mut app, _) = app_with_one_pending();
        let words: Vec<String> = (0..300).map(|i| format!("{i:012}")).collect();
        let task = format!("{} final-words-right-here", words.join(" "));
        app.pending.front_mut().expect("set up").task = task;

        // 120 columns puts the prompt at its own default content width of 74
        // once the sidebar and the centring math are accounted for.
        let mut terminal =
            Terminal::new(TestBackend::new(120, 30)).expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        for _ in 0..1000 {
            app.scroll_approval(true);
        }

        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        let buf = terminal.backend().buffer();
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("final-words-right-here"),
            "scrolling should reach the end of ordinary prose too:\n{text}"
        );
    }

    #[test]
    fn paragraphs_separated_by_blank_lines_can_be_scrolled_to_their_final_words_and_legend() {
        // A task is prose an agent wrote, and a blank line between
        // paragraphs is ordinary in it. Checking only the last row or two of
        // the viewport for "still has content" cannot tell a deliberate
        // paragraph break apart from having scrolled past the true end —
        // eight paragraphs at this box's own default width of 74 stalled
        // three rows in when seventeen were needed, leaving the whole key
        // legend unreachable. An exact total does not confuse the two.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (mut app, _) = app_with_one_pending();
        let mut paragraphs: Vec<String> = (0..7)
            .map(|n| {
                (0..20)
                    .map(|i| format!("paragraph{n}word{i}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        paragraphs.push(format!(
            "{} final-words-right-here",
            (0..20)
                .map(|i| format!("paragraph7word{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        let task = paragraphs.join("\n\n");
        app.pending.front_mut().expect("set up").task = task;

        // 120 columns puts the prompt at its own default content width of 74.
        let mut terminal =
            Terminal::new(TestBackend::new(120, 30)).expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        for _ in 0..100 {
            app.scroll_approval(true);
        }

        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        let buf = terminal.backend().buffer();
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("final-words-right-here"),
            "scrolling should reach the task's own final words:\n{text}"
        );
        assert!(
            text.contains("approve"),
            "and the key legend after them, not stall on a paragraph break:\n{text}"
        );
    }

    #[test]
    fn a_whitespace_only_line_can_be_scrolled_past() {
        // A line of only spaces has no words at all, so it must still count
        // for exactly the one row it occupies — the same accounting a blank
        // line needs — rather than being skipped and undercounting the total.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (mut app, _) = app_with_one_pending();
        let filler: Vec<String> = (0..80).map(|i| format!("word{i}")).collect();
        let task = format!(
            "{}\n   \n{} final-words-right-here",
            filler.join(" "),
            filler.join(" ")
        );
        app.pending.front_mut().expect("set up").task = task;

        let mut terminal =
            Terminal::new(TestBackend::new(100, 30)).expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        for _ in 0..100 {
            app.scroll_approval(true);
        }

        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        let buf = terminal.backend().buffer();
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("final-words-right-here"),
            "a whitespace-only line must not undercount the rows before it:\n{text}"
        );
    }

    #[test]
    fn a_single_hard_broken_word_can_be_scrolled_to_its_end_at_a_narrow_width() {
        // A single 4000-character word has no whitespace to wrap at, so it
        // is hard-broken every `width` columns — at a much narrower width
        // than the box's own 74-column default, which is where every
        // earlier attempt at this bound happened to be checked.
        // "END" rather than the "final-words-right-here" marker other tests
        // use at their much wider box: short enough, and its own whitespace-
        // separated word, that it can never itself be hard-broken across
        // rows here — a marker that wraps would need joining logic of its
        // own to search for, which is exactly the kind of extra arithmetic
        // this fix is trying to avoid needing at all.
        let (mut app, _) = app_with_one_pending();
        let task = format!("{} END", "x".repeat(4000));
        app.pending.front_mut().expect("set up").task = task;

        let mut terminal = terminal_for_inner_width(18);
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        for _ in 0..300 {
            app.scroll_approval(true);
        }

        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        let text = rendered_text(&terminal);
        assert!(
            text.contains("END"),
            "scrolling should reach the end at a narrow width too:\n{text}"
        );
    }

    #[test]
    fn irregular_interior_whitespace_is_accounted_for_at_a_narrow_width() {
        // `Wrap { trim: false }` paints interior whitespace runs, but a
        // measure that counts only words and discards the space between them
        // does not — forty words joined by a run of twenty-five spaces each,
        // at an inner width of eighteen, is the shape a fuzz run over this
        // exact widget found undercounted by fourteen rows against a real
        // render, once every earlier attempt's arithmetic is checked rather
        // than trusted.
        let (mut app, _) = app_with_one_pending();
        let gap = " ".repeat(25);
        let words: Vec<String> = (0..40).map(|i| format!("word{i}")).collect();
        let task = format!("{} END", words.join(&gap));
        app.pending.front_mut().expect("set up").task = task;

        let mut terminal = terminal_for_inner_width(18);
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        for _ in 0..300 {
            app.scroll_approval(true);
        }

        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        let text = rendered_text(&terminal);
        assert!(
            text.contains("END"),
            "scrolling should not stall on the whitespace between words:\n{text}"
        );
        assert!(
            text.contains("approve"),
            "and the key legend after them:\n{text}"
        );
    }

    #[test]
    fn a_cjk_task_can_be_scrolled_to_its_end_at_two_narrow_widths() {
        // Every character in this text is two columns wide, which breaks
        // "a row fills to its last column": a row two columns short of
        // `width` cannot take one more of these characters, unlike an ASCII
        // one. Checked at two widths odd and even relative to that.
        for width in [7u16, 9] {
            let (mut app, _) = app_with_one_pending();
            // "END" as its own whitespace-separated word, short enough to
            // never itself be hard-broken at either width, the way it would
            // be if appended straight onto the CJK text with nothing to wrap
            // it away from.
            let task = format!("{} END", "日本語のテキストです".repeat(50));
            app.pending.front_mut().expect("set up").task = task;

            let mut terminal = terminal_for_inner_width(width);
            terminal
                .draw(|frame| app.draw(frame))
                .expect("drawing succeeds");

            for _ in 0..1000 {
                app.scroll_approval(true);
            }

            terminal
                .draw(|frame| app.draw(frame))
                .expect("drawing succeeds");

            let text = rendered_text(&terminal);
            assert!(
                text.contains("END"),
                "scrolling should reach the end at inner width {width}:\n{text}"
            );
        }
    }

    #[test]
    fn scrolling_past_the_end_of_a_short_task_still_shows_its_key_legend() {
        // A content-shorter-than-the-viewport task should not scroll at all:
        // the clamp is `total_rows - height`, which for a five-line prompt in
        // an eighteen-row viewport is zero. Pressing down anyway should not
        // move it, and the key legend should stay exactly where it always
        // was rather than being scrolled away.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (mut app, _) = app_with_one_pending(); // task: "write the tests"

        let mut terminal =
            Terminal::new(TestBackend::new(100, 30)).expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        // Far more presses than a five-line prompt could ever need.
        for _ in 0..50 {
            app.scroll_approval(true);
        }

        terminal
            .draw(|frame| app.draw(frame))
            .expect("drawing succeeds");

        let buf = terminal.backend().buffer();
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("approve"),
            "the key legend should still be visible, not scrolled away:\n{text}"
        );
    }

    /// What [`two_daemons`] hands back: the app, each daemon's project id, the
    /// `Sender<ServerMessage>` half of each fake daemon (for replaying a
    /// reconnect, which needs to speak as the daemon), and the outbox
    /// `Receiver` most callers already used, one pair per daemon.
    type TwoDaemons = (
        App,
        ProjectId,
        ProjectId,
        Sender<ServerMessage>,
        Sender<ServerMessage>,
        Receiver<ClientMessage>,
        Receiver<ClientMessage>,
    );

    /// Two attached daemons, each with a project of its own.
    fn two_daemons() -> TwoDaemons {
        let (first, first_daemon, first_sent) = Client::for_test();
        let (second, second_daemon, second_sent) = Client::for_test();

        let mut app = App::new(HarnessRegistry::default());
        app.attach(first);
        app.attach(second);

        let alpha = Project::new("/tmp/alpha", ProjectSource::LocalDir);
        let beta = Project::new("/tmp/beta", ProjectSource::LocalDir);
        let (alpha_id, beta_id) = (alpha.id, beta.id);

        first_daemon
            .send(ServerMessage::ProjectOpened { project: alpha })
            .expect("the app is listening");
        second_daemon
            .send(ServerMessage::ProjectOpened { project: beta })
            .expect("the app is listening");
        app.poll_daemon();

        (
            app,
            alpha_id,
            beta_id,
            first_daemon,
            second_daemon,
            first_sent,
            second_sent,
        )
    }

    /// The device a project is on, as the sidebar would read it.
    fn device_of(app: &App, project: ProjectId) -> Option<DeviceId> {
        app.state
            .projects()
            .iter()
            .find(|candidate| candidate.id == project)
            .map(|candidate| candidate.device)
    }

    #[test]
    fn each_daemons_projects_are_attributed_to_its_own_device() {
        let (app, alpha, beta, _, _, _, _) = two_daemons();

        assert_eq!(app.state.devices().len(), 2);
        assert_ne!(device_of(&app, alpha), device_of(&app, beta));
    }

    #[test]
    fn a_project_from_a_daemon_lands_on_a_machine_the_sidebar_knows() {
        // The invariant the whole slice rests on: the sidebar groups projects
        // under their machine's row, so a project stamped with a device that
        // was never registered is drawn nowhere at all — open, invisible and
        // unreachable. `attach` registers the machine before its connection
        // can announce anything, which is what makes that impossible.
        let (app, alpha, beta, _, _, _, _) = two_daemons();

        for project in [alpha, beta] {
            let device = device_of(&app, project).expect("the project is registered");
            assert!(
                app.state.device(device).is_some(),
                "the machine a project names must be one the sidebar draws"
            );
        }
    }

    #[test]
    fn a_keystroke_reaches_the_daemon_the_pane_is_on() {
        // The whole point of the slice: one screen, two machines, and no
        // chance of typing into the wrong one.
        let (mut app, _alpha, beta, _, _, first_sent, second_sent) = two_daemons();
        let pane = PaneId::new();
        // The pane belongs to the second daemon's project.
        app.apply(spawned(pane, beta, "shell", None, false));
        // Focus follows the selection, so the second machine's project is
        // what the user is looking at.
        let _ = app.state.select_project(beta);
        let _ = app.state.focus(pane);

        press(&mut app, KeyCode::Char('x'));

        assert!(
            std::iter::from_fn(|| second_sent.try_recv().ok())
                .any(|m| matches!(m, ClientMessage::WritePane { pane: p, .. } if p == pane)),
            "the daemon that owns the pane hears it"
        );
        assert!(
            !std::iter::from_fn(|| first_sent.try_recv().ok())
                .any(|m| matches!(m, ClientMessage::WritePane { .. })),
            "and the other one hears nothing"
        );
    }

    #[test]
    fn one_daemon_restarting_rebuilds_only_its_own_rows() {
        // Driven the way the real thing arrives: the first machine's
        // connection counts a new generation, and `poll_daemon` is what
        // notices. Everything that daemon described belongs to a socket that
        // is gone, and nothing the other one described does.
        let (mut app, alpha, beta, _, _, first_sent, second_sent) = two_daemons();

        // A root this client asked the first daemon for, so its reconnect has
        // something of its own to ask again.
        app.add_project(PathBuf::from("/tmp/alpha"));
        // Whatever the setup and that request already sent: the assertions
        // below are about what the *reconnection* sends.
        while first_sent.try_recv().is_ok() {}
        while second_sent.try_recv().is_ok() {}

        app.attachments()[0].client.handle().reconnect_for_test();
        assert!(app.poll_daemon(), "a reattach is worth a frame");

        assert!(
            !app.state.projects().iter().any(|p| p.id == alpha),
            "that machine's project is gone until it announces it again"
        );
        assert!(
            app.state.projects().iter().any(|p| p.id == beta),
            "and the other machine's is not"
        );
        assert_eq!(
            app.state.devices().len(),
            2,
            "both machines keep their rows"
        );
        assert!(
            app.status.contains("reattached"),
            "and the user is told: {:?}",
            app.status
        );

        assert!(
            std::iter::from_fn(|| first_sent.try_recv().ok()).any(
                |m| matches!(m, ClientMessage::OpenProject { root } if root == Path::new("/tmp/alpha"))
            ),
            "the restarted machine is asked for its roots again"
        );
        assert!(
            std::iter::from_fn(|| second_sent.try_recv().ok())
                .next()
                .is_none(),
            "and the machine that never went is asked for nothing"
        );
    }

    #[test]
    fn a_reconnect_restores_the_project_that_was_selected() {
        // The other half of `one_daemon_restarting_rebuilds_only_its_own_rows`:
        // the reconnect does not just rebuild the rows it forgot, it has to
        // put the view back where it was, rather than leaving it on whichever
        // project the interim fallback happened to land on -- and moving
        // keystrokes with it, since a pane only gets focus while its project
        // is selected.
        let (mut app, alpha, beta, first_daemon, _second_daemon, _, _) = two_daemons();

        let _ = app.state.select_project(alpha);

        app.attachments()[0].client.handle().reconnect_for_test();
        app.poll_daemon();

        assert_eq!(
            app.state.selected_project(),
            Some(beta),
            "while the reconnect is in flight the fallback lands on whatever is left"
        );

        // The replay: the same daemon, so the same id.
        let mut replayed = Project::new("/tmp/alpha", ProjectSource::LocalDir);
        replayed.id = alpha;
        first_daemon
            .send(ServerMessage::ProjectOpened { project: replayed })
            .expect("the app is listening");
        app.poll_daemon();

        assert_eq!(
            app.state.selected_project(),
            Some(alpha),
            "the view returns to the project the user was looking at"
        );
    }

    #[test]
    fn a_selection_made_during_the_outage_is_not_undone_by_the_replay() {
        // The other side of restoring the selection: a reconnect's replay is
        // not the only thing that can happen while a machine is away, and the
        // user looking somewhere else on purpose has to outrank a promise
        // this client made to itself before the outage started.
        let (mut app, alpha, beta, first_daemon, _second_daemon, _, _) = two_daemons();

        let _ = app.state.select_project(alpha);

        app.attachments()[0].client.handle().reconnect_for_test();
        app.poll_daemon();

        // The user looks at the other project during the outage -- through
        // `App::select_project`, the same path a click on the sidebar or the
        // project picker takes, which is what is supposed to let go of the
        // reconnect's own intention.
        app.select_project(beta);

        let mut replayed = Project::new("/tmp/alpha", ProjectSource::LocalDir);
        replayed.id = alpha;
        first_daemon
            .send(ServerMessage::ProjectOpened { project: replayed })
            .expect("the app is listening");
        app.poll_daemon();

        assert_eq!(
            app.state.selected_project(),
            Some(beta),
            "the replay must not yank the view back to a project the user left on purpose"
        );
    }

    #[test]
    fn a_reattached_daemon_that_renamed_itself_updates_its_row() {
        // sync_attachment reads client.device() every poll and used it only
        // for the "reattached to {name}" status -- the row in state kept
        // whatever name `attach` first saw, so a daemon that comes back under
        // a different name left the sidebar naming the old one while the
        // status line already said the new one.
        let (mut app, alpha, _beta, ..) = two_daemons();
        let device = device_of(&app, alpha).expect("the project is registered");

        app.attachments()[0]
            .client
            .handle()
            .rename_for_test("renamed");
        app.attachments()[0].client.handle().reconnect_for_test();
        app.poll_daemon();

        assert_eq!(
            app.state.device(device).map(|d| d.name.as_str()),
            Some("renamed"),
            "the sidebar's row follows the rename"
        );
    }

    #[test]
    fn a_daemon_that_is_still_up_is_not_taken_for_a_restart() {
        // Each connection carries its own generation, so polling a fleet that
        // has not changed must rebuild nothing and ask for no frame — this
        // runs every tick.
        let (mut app, alpha, beta, _, _, _, _) = two_daemons();

        assert!(!app.poll_daemon(), "nothing changed, so nothing to draw");
        assert!(app.state.projects().iter().any(|p| p.id == alpha));
        assert!(app.state.projects().iter().any(|p| p.id == beta));
    }

    #[test]
    fn attaching_takes_down_the_agents_this_process_started() {
        // `attach` is public and a standalone client can have been running
        // agents of its own for an hour before it ever reaches a daemon.
        // Dropping that machine's rows while keeping its panes would leave a
        // real child process running with nothing on screen pointing at it,
        // nothing reading it and nobody left to stop it.
        let def = dispatch_config::HarnessDef {
            id: "shell".to_string(),
            display_name: "Shell".to_string(),
            launch: Launch {
                command: "sh".to_string(),
                args: Vec::new(),
                env: Default::default(),
            },
            ..Default::default()
        };

        let dir = scratch("attach-local");
        let mut app = App::new([def].into_iter().collect());
        app.add_project(dir.clone());
        let project = app.state.projects()[0].id;
        let _ = app.state.select_project(project);
        app.spawn_pane("shell", Size::new(80, 24))
            .expect("a shell starts");

        assert_eq!(app.panes.len(), 1, "the client is holding the child");

        let (client, _daemon, _sent) = Client::for_test();
        app.attach(client);

        // Taken down through the same `terminate` `^a x` uses, and then
        // dropped: what is left is a client holding nothing of its own.
        assert!(
            app.panes.is_empty(),
            "the child this process started is not left running unseen"
        );
        assert!(
            app.state.visible_panes().is_empty(),
            "and its row is gone with it"
        );
        assert_eq!(
            app.state.devices().len(),
            1,
            "the daemon's machine is the only one left"
        );
    }

    #[test]
    fn a_standalone_client_is_a_machine_too() {
        // One code path rather than "device or not" at every use.
        let app = App::new(HarnessRegistry::default());

        assert_eq!(app.state.devices().len(), 1);
    }

    #[test]
    fn a_keystroke_for_an_unreachable_machine_is_refused_out_loud() {
        let (mut app, _alpha, beta, _, _, _, second_sent) = two_daemons();
        let pane = PaneId::new();
        app.apply(spawned(pane, beta, "shell", None, false));
        let _ = app.state.select_project(beta);
        let _ = app.state.focus(pane);

        let device = device_of(&app, beta).expect("the pane is on a machine");
        app.state.set_device_reachable(device, false);

        press(&mut app, KeyCode::Char('x'));

        assert!(
            !std::iter::from_fn(|| second_sent.try_recv().ok())
                .any(|m| matches!(m, ClientMessage::WritePane { .. })),
            "nothing is sent into the void"
        );
        assert!(
            app.status.contains("unreachable"),
            "and the user is told: {:?}",
            app.status
        );
    }
}
