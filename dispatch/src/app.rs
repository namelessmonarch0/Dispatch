//! The running application: state, panes, and the event loop.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dispatch_client::Client;
use dispatch_config::{HarnessRegistry, Launch};
use dispatch_core::{
    AppState, HarnessId, Pane as CorePane, PaneId, PaneStatus, Project, ProjectId, ProjectSource,
    RequestId,
};
use dispatch_layout::{tile, tile_zoomed};
use dispatch_proto::{ClientMessage, DelegateOutcome, PaneUpdate, ServerMessage};
use dispatch_pty::{
    KeyEncoder, MouseEncoder, MouseInput, PtySession, RunState, Screen, ScreenReader, ScrollTo,
    Size, TitleScanner,
};

use crate::approval::Approval;
use crate::backend::{Backend, RemotePane};
use dispatch_tui::input::{
    Action, Direction, Event, InputRouter, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use dispatch_tui::{Item, PaneWidget, Picker, Sidebar, sidebar};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Clear, Paragraph, Widget};

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
            Overlay::Approval { .. } => None,
        }
    }

    /// The picker inside, mutably, for the variants that have one.
    fn picker_mut(&mut self) -> Option<&mut Picker> {
        match self {
            Overlay::Harness(picker) | Overlay::Project(picker) | Overlay::Register(picker) => {
                Some(picker)
            }
            Overlay::Approval { .. } => None,
        }
    }

    /// What kind of choice a picker overlay is making, for the variants that
    /// are one.
    fn kind(&self) -> Option<OverlayKind> {
        match self {
            Overlay::Harness(_) => Some(OverlayKind::Harness),
            Overlay::Project(_) => Some(OverlayKind::Project),
            Overlay::Register(_) => Some(OverlayKind::Register),
            Overlay::Approval { .. } => None,
        }
    }
}

/// Where this Dispatch's agents run.
///
/// Attached is what lets the work outlive the interface; standalone is what
/// makes Dispatch usable with nothing else running, so both are kept.
enum Mode {
    /// The agents are this process's children.
    Standalone,
    /// The agents belong to a daemon.
    Attached(Client),
}

/// The application.
pub struct App {
    mode: Mode,
    /// Which daemon connection the state on screen was built from. Zero when
    /// the agents are this process's own.
    generation: u64,
    /// The project roots this client asked for, so a reconnection can ask again.
    opened: Vec<PathBuf>,
    overlay: Option<Overlay>,
    state: AppState,
    panes: HashMap<PaneId, Pane>,
    harnesses: HarnessRegistry,
    router: InputRouter,
    /// Where each pane was drawn last frame, for resolving the pointer.
    layout: Vec<(PaneId, Rect)>,
    /// Where the sidebar was drawn last frame.
    ///
    /// The sidebar has no keyboard focus of its own, so a click is the only
    /// way to pick one of its rows out of the list — this is what a click is
    /// matched against.
    sidebar_area: Rect,
    /// Where the approval prompt was last drawn, so its scroll can be
    /// clamped against the box's actual size at draw time.
    approval_area: Rect,
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
    /// Every `ClientMessage` a decision would have sent, kept only so tests
    /// can tell `a` and `A` apart without a real daemon to send it to.
    #[cfg(test)]
    sent: Vec<ClientMessage>,
}

impl App {
    /// Creates an application that owns its own agents.
    pub fn new(harnesses: HarnessRegistry) -> Self {
        Self::with_mode(harnesses, Mode::Standalone)
    }

    /// Creates an application whose agents belong to `client`'s daemon.
    pub fn attached(harnesses: HarnessRegistry, client: Client) -> Self {
        Self::with_mode(harnesses, Mode::Attached(client))
    }

    fn with_mode(harnesses: HarnessRegistry, mode: Mode) -> Self {
        let generation = match &mode {
            Mode::Standalone => 0,
            Mode::Attached(client) => client.generation(),
        };

        Self {
            mode,
            generation,
            opened: Vec::new(),
            overlay: None,
            state: AppState::new(),
            panes: HashMap::new(),
            harnesses,
            router: InputRouter::new(),
            layout: Vec::new(),
            sidebar_area: Rect::default(),
            approval_area: Rect::default(),
            expanded: HashSet::new(),
            pending: VecDeque::new(),
            answered: HashMap::new(),
            child_titles: HashMap::new(),
            status: String::new(),
            quit: false,
            #[cfg(test)]
            sent: Vec::new(),
        }
    }

    /// Registers a project.
    ///
    /// Attached, the daemon is asked to open it and the project appears when it
    /// answers: it is the daemon that names projects, and both clients on a
    /// fleet have to use the same id for the same checkout.
    pub fn add_project(&mut self, root: PathBuf) {
        if let Mode::Attached(client) = &self.mode {
            client.send(ClientMessage::OpenProject { root: root.clone() });
            // Remembered so a reconnection asks again: a daemon that was
            // restarted is serving whatever its own command line said, which
            // need not include what this client was opened with.
            if !self.opened.contains(&root) {
                self.opened.push(root);
            }
            return;
        }

        let source = if root.join(".git").exists() {
            ProjectSource::GitRepo { remote: None }
        } else {
            ProjectSource::LocalDir
        };

        self.state.add_project(Project::new(root, source));
    }

    /// What the daemon calls itself, when attached to one.
    #[must_use]
    pub fn device(&self) -> Option<String> {
        match &self.mode {
            Mode::Standalone => None,
            Mode::Attached(client) => Some(client.device()),
        }
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

        let Some(def) = self.harnesses.get(harness) else {
            self.status = format!("unknown harness {harness:?}");
            return Ok(());
        };

        if let Mode::Attached(client) = &self.mode {
            client.send(ClientMessage::SpawnPane {
                project: project_id,
                harness: harness.to_string(),
                size: (area.cols, area.rows),
            });
            self.status = format!("starting {}…", def.display_name);
            return Ok(());
        }

        let launch: Launch = def.launch_for_current_platform().clone();
        let cwd = self
            .state
            .projects()
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.root.clone())
            .context("the selected project is registered")?;

        let session = PtySession::spawn(&launch, &cwd, area)
            .with_context(|| format!("failed to start {}", def.display_name))?;

        let id = self
            .state
            .spawn_pane(project_id, HarnessId::new(harness))
            .context("the selected project is registered")?;

        let display_name = def.display_name.clone();
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
        let Mode::Attached(client) = &self.mode else {
            return false;
        };

        let generation = client.generation();
        let connected = client.is_connected();
        let device = client.device();
        let messages = client.poll();
        let mut changed = false;

        if generation != self.generation {
            // Everything on screen was described by a connection that is gone.
            // The new one announces its projects and panes on subscribing, so
            // the view is rebuilt from what it says rather than kept and
            // patched.
            self.generation = generation;
            self.forget_the_fleet();
            self.reopen_projects();
            self.status = format!("reattached to {device}");
            changed = true;
        }

        for message in messages {
            changed |= self.apply(message);
        }

        // Said once, and only once: a status line rewritten every frame would
        // bury whatever the user was reading. The agents are the daemon's, so
        // this is a lost view rather than lost work.
        if !connected && !self.status.starts_with("waiting") {
            self.status = "waiting for the daemon — the agents are still running".into();
            changed = true;
        }

        changed
    }

    /// Asks the daemon for the projects this client was opened with.
    fn reopen_projects(&mut self) {
        let Mode::Attached(client) = &self.mode else {
            return;
        };

        for root in &self.opened {
            client.send(ClientMessage::OpenProject { root: root.clone() });
        }
    }

    /// Forgets what a daemon told us over a connection that has ended.
    ///
    /// A pane the old connection described may not exist any more: a daemon that
    /// was restarted names its panes afresh. Anything still running is described
    /// again by the new connection.
    fn forget_the_fleet(&mut self) {
        self.panes.clear();
        self.state = AppState::new();
        self.layout.clear();
        // A picker offering projects that have just been forgotten would act on
        // an id nothing answers to, and a request from a pane that no longer
        // exists would be answered into a void. The new connection's `Subscribe`
        // catch-up replays whatever is still actually outstanding.
        self.overlay = None;
        self.pending.clear();
        // Both are about requests that belonged to the connection that ended:
        // the new one names its panes afresh, and a title kept for a pane id
        // that will never be announced again is just a leak.
        self.answered.clear();
        self.child_titles.clear();
    }

    /// Applies one message from the daemon. Returns whether to redraw.
    fn apply(&mut self, message: ServerMessage) -> bool {
        match message {
            ServerMessage::ProjectOpened { project } => {
                self.state.add_project(project);
                true
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
                    let _ = self.state.set_pane_title(pane, &title);
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
                PaneUpdate::Title { title } => self.state.set_pane_title(pane, &title).is_ok(),
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

        let Mode::Attached(client) = &self.mode else {
            return false;
        };
        let daemon = client.handle();

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
            let _ = self.state.set_pane_title(id, &title);
        }

        for (id, code) in exited {
            // An exited pane keeps its screen and stays selectable, so its
            // final output can be read before it is closed.
            let _ = self.state.set_pane_status(id, PaneStatus::Exited(code));
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
            && let Some(id) =
                sidebar::hit_test(&self.state, self.sidebar_area, mouse.column, mouse.row)
        {
            self.focus_pane(id);
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
            Action::Scrollback => self.scroll_focused(-10),
            Action::Approvals => self.open_next_approval(),
            Action::ExpandChild => self.expand_child(),
            Action::CollapseChild => self.collapse_child(),
        }

        Ok(())
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

        if let Mode::Attached(client) = &self.mode {
            client.send(message);
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
    /// that arrives before the prompt has ever been drawn (`approval_area`
    /// would still be a zero rect) and would leave a stale, too-large offset
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
                    let _ = self.state.select_project(project);
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

    fn open_project_picker(&mut self) {
        let items: Vec<Item> = self
            .state
            .projects()
            .iter()
            .map(|p| Item::new(p.id.to_string(), &p.name).with_detail(p.root.display().to_string()))
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
        use dispatch_pty::MouseButton;
        let rows = match input.button {
            MouseButton::WheelUp => -3,
            MouseButton::WheelDown => 3,
            _ => return,
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
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };

        // Wrap in bracketed paste markers so the child knows this is pasted
        // text rather than typing, and does not act on each line as it lands.
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");

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

        frame.render_widget(Sidebar::new(&self.state), sidebar_area);
        self.sidebar_area = sidebar_area;

        self.layout = self.compute_layout(panes_area);
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

        let Overlay::Approval { scroll } = overlay else {
            return;
        };
        let scroll = *scroll;

        let rect = centred_approval(panes_area);
        self.approval_area = rect;

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
    fn tileable(&self) -> Vec<PaneId> {
        self.state
            .visible_panes()
            .iter()
            .filter(|pane| pane.parent.is_none() || self.expanded.contains(&pane.id))
            .map(|pane| pane.id)
            .collect()
    }

    /// Where each visible pane goes this frame.
    fn compute_layout(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let visible = self.tileable();

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

    fn draw_panes(&mut self, frame: &mut Frame<'_>) {
        let focused = self.state.focused_pane();
        let mut cursor = None;

        for (id, rect) in &self.layout {
            let Some(pane) = self.panes.get(id) else {
                continue;
            };

            let widget = PaneWidget::new(&pane.screen).focused(focused == Some(*id));

            if let Some(position) = widget.cursor_position(*rect) {
                cursor = Some(position);
            }

            frame.render_widget(widget, *rect);
        }

        // Placing the real cursor is what makes typing feel native rather
        // than like editing a picture of a terminal.
        if let Some((x, y)) = cursor {
            frame.set_cursor_position((x, y));
        }
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

        let text = if self.router.is_armed() {
            // A prefix that armed invisibly is how a keystroke goes missing
            // with no explanation.
            "PREFIX".to_string()
        } else {
            let base = if !self.status.is_empty() {
                self.status.clone()
            } else {
                let panes = self.state.visible_panes().len();
                // Attached is worth saying: it is the difference between
                // closing Dispatch and killing the agents.
                let where_ = self
                    .device()
                    .map_or_else(String::new, |device| format!("  {device}"));
                format!(
                    "{panes} pane(s){where_}  ^a n new  ^a x close  ^a z zoom  ^a s child  ^a c collapse  ^a q quit"
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
    fn column_of(line: &str, needle: &str) -> usize {
        let byte = line
            .find(needle)
            .unwrap_or_else(|| panic!("expected {needle:?} in {line:?}"));
        line[..byte].chars().count()
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

        let parent_row = lines
            .iter()
            .position(|line| line.contains("claude"))
            .expect("the parent has a row");
        let child_row = lines
            .iter()
            .position(|line| line.contains("codex"))
            .expect("the child has a row");
        assert_eq!(
            child_row,
            parent_row + 1,
            "the child belongs under its parent:\n{drawn}"
        );
        assert_eq!(
            column_of(lines[child_row], "codex"),
            column_of(lines[parent_row], "claude") + 2,
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
}
