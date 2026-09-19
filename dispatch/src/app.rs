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
use dispatch_proto::{ClientMessage, PaneUpdate, ServerMessage};
use dispatch_pty::{
    KeyEncoder, MouseEncoder, MouseInput, PtySession, RunState, Screen, ScreenReader, ScrollTo,
    Size, TitleScanner,
};

use crate::approval::Approval;
use crate::backend::{Backend, RemotePane};
use dispatch_tui::input::{
    Action, Direction, Event, InputRouter, KeyCode, KeyEventKind, MouseEventKind,
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
    /// Subagents the user has opened, so they join the tiled grid.
    ///
    /// Which rows are open is a per-client choice, not a property of the
    /// pane itself — two clients on one fleet can disagree about it, so this
    /// never goes to the daemon.
    expanded: HashSet<PaneId>,
    /// Delegation requests waiting on a decision, oldest first.
    pending: VecDeque<PendingRequest>,
    status: String,
    quit: bool,
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
            expanded: HashSet::new(),
            pending: VecDeque::new(),
            status: String::new(),
            quit: false,
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
                parent: _,
            } => self.adopt_remote(pane, project, &harness),

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
                // keyboard when nothing else already has it.
                if self.overlay.is_none() {
                    self.overlay = Some(Overlay::Approval { scroll: 0 });
                }

                true
            }

            ServerMessage::DelegateResolved { request, .. } => {
                // Answered here, elsewhere, or by the daemon's own deadline —
                // any of the three means this client's queue no longer has a
                // reason to hold it. A decision this client made itself is
                // already gone from `pending` (see `decide`), so this mostly
                // withdraws a prompt someone else, or nobody, has just settled.
                let front_was = self.pending.front().map(|waiting| waiting.request);
                self.pending.retain(|waiting| waiting.request != request);

                if matches!(self.overlay, Some(Overlay::Approval { .. }))
                    && front_was != self.pending.front().map(|waiting| waiting.request)
                {
                    self.open_next_approval();
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
    fn adopt_remote(&mut self, id: PaneId, project: ProjectId, harness: &str) -> bool {
        if self.panes.contains_key(&id) {
            return false;
        }

        let Mode::Attached(client) = &self.mode else {
            return false;
        };
        let daemon = client.handle();

        let mut pane = CorePane::new(project, HarnessId::new(harness));
        pane.id = id;
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

        let display_name = self
            .harnesses
            .get(harness)
            .map_or_else(|| harness.to_string(), |def| def.display_name.clone());

        if let Err(error) = self.adopt(id, backend, &display_name) {
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

    /// Handles input while an overlay has the keyboard.
    fn handle_overlay(&mut self, event: &Event, area: Size) -> Result<()> {
        let Event::Key(key) = event else {
            return Ok(());
        };
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        if matches!(self.overlay, Some(Overlay::Approval { .. })) {
            self.handle_approval_key(key.code);
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

        Ok(())
    }

    /// Handles a key while the approval prompt has the keyboard.
    ///
    /// `Esc` defers rather than denies — a mistaken deny throws away work the
    /// agent has already reasoned about — so it is the only key here that
    /// leaves the request in `pending` rather than answering it.
    fn handle_approval_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.overlay = None;
                if !self.pending.is_empty() {
                    self.status = format!("{} delegation(s) waiting — ^a a", self.pending.len());
                }
            }
            KeyCode::Char('a') => self.decide(true, false),
            KeyCode::Char('d') => self.decide(false, false),
            KeyCode::Char('A') => self.decide(true, true),
            KeyCode::Up => self.scroll_approval(false),
            KeyCode::Down => self.scroll_approval(true),
            _ => {}
        }
    }

    /// Answers the request at the front of the queue, and moves on to the
    /// next one if there is one waiting.
    fn decide(&mut self, approve: bool, blanket: bool) {
        let Some(waiting) = self.pending.pop_front() else {
            self.overlay = None;
            return;
        };

        if let Mode::Attached(client) = &self.mode {
            client.send(ClientMessage::DelegateDecision {
                request: waiting.request,
                approve,
                blanket,
            });
        }

        self.open_next_approval();
    }

    /// Scrolls the task text of the request currently shown.
    fn scroll_approval(&mut self, down: bool) {
        let Some(Overlay::Approval { scroll }) = &mut self.overlay else {
            return;
        };

        if down {
            let max = self.pending.front().map_or(0, |waiting| {
                u16::try_from(waiting.task.lines().count()).unwrap_or(u16::MAX)
            });
            *scroll = scroll.saturating_add(1).min(max);
        } else {
            *scroll = scroll.saturating_sub(1);
        }
    }

    /// Opens the approval prompt for whatever is at the front of the queue,
    /// or closes it when there is nothing left to ask about.
    ///
    /// This is `Action::Approvals`, reached with the queue as it stands
    /// whenever nothing new has arrived; it is also how the prompt advances
    /// to the next request after one is answered.
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
    fn draw_overlay(&self, frame: &mut Frame<'_>, panes_area: Rect) {
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
        // The queue can only be empty here for one frame, between the last
        // request being answered and `open_next_approval` closing the
        // overlay; nothing to draw is not a bug worth a fallback screen for.
        let Some(request) = self.pending.front() else {
            return;
        };

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

        let widget = Approval {
            asking,
            harness: &request.harness,
            project,
            depth: request.depth,
            task: &request.task,
            waiting: self.pending.len().saturating_sub(1),
            scroll: *scroll,
        };

        let rect = centred_approval(panes_area);
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

        let text = if self.router.is_armed() {
            // A prefix that armed invisibly is how a keystroke goes missing
            // with no explanation.
            "PREFIX".to_string()
        } else if !self.status.is_empty() {
            self.status.clone()
        } else {
            let panes = self.state.visible_panes().len();
            // Attached is worth saying: it is the difference between closing
            // Dispatch and killing the agents.
            let where_ = self
                .device()
                .map_or_else(String::new, |device| format!("  {device}"));
            format!("{panes} pane(s){where_}  ^a n new  ^a x close  ^a z zoom  ^a q quit")
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
    use dispatch_core::{Project, ProjectSource};

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
}
