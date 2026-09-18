//! The running application: state, panes, and the event loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dispatch_config::{HarnessRegistry, Launch};
use dispatch_core::{AppState, HarnessId, PaneId, PaneStatus, Project, ProjectSource};
use dispatch_layout::{tile, tile_zoomed};
use dispatch_pty::{KeyEncoder, PtySession, RunState, Screen, ScreenReader, Size};
use dispatch_tui::input::{Action, Direction, Event, InputRouter, KeyCode, KeyEventKind};
use dispatch_tui::{Item, PaneWidget, Picker, Sidebar, sidebar};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Paragraph, Widget};

/// Which overlay is open, decoupled from the picker so a selection can be
/// read out before the overlay is closed.
#[derive(Debug, Clone, Copy)]
enum OverlayKind {
    Harness,
    Project,
    Register,
}

fn kind_of(overlay: &Overlay) -> OverlayKind {
    match overlay {
        Overlay::Harness => OverlayKind::Harness,
        Overlay::Project => OverlayKind::Project,
        Overlay::Register => OverlayKind::Register,
    }
}

/// Frame budget. A chatty agent can produce output faster than any terminal
/// can draw it, so redraws are coalesced rather than done per byte.
const FRAME: Duration = Duration::from_millis(16);

/// Everything one pane owns.
struct Pane {
    session: PtySession,
    encoder: KeyEncoder,
    reader: ScreenReader,
    /// Last screen read, redrawn each frame without re-reading when nothing
    /// changed.
    screen: Screen,
}

/// What the picker on screen is choosing, which decides what a selection
/// does.
enum Overlay {
    /// A harness to spawn.
    Harness,
    /// A project to switch to.
    Project,
    /// A harness to register, found on PATH.
    Register,
}

/// The application.
pub struct App {
    overlay: Option<(Overlay, Picker)>,
    state: AppState,
    panes: HashMap<PaneId, Pane>,
    harnesses: HarnessRegistry,
    router: InputRouter,
    /// Where each pane was drawn last frame, for resolving the pointer.
    layout: Vec<(PaneId, Rect)>,
    status: String,
    quit: bool,
}

impl App {
    /// Creates an application with `harnesses` registered.
    pub fn new(harnesses: HarnessRegistry) -> Self {
        Self {
            overlay: None,
            state: AppState::new(),
            panes: HashMap::new(),
            harnesses,
            router: InputRouter::new(),
            layout: Vec::new(),
            status: String::new(),
            quit: false,
        }
    }

    /// Registers a project.
    pub fn add_project(&mut self, root: PathBuf) {
        let source = if root.join(".git").exists() {
            ProjectSource::GitRepo { remote: None }
        } else {
            ProjectSource::LocalDir
        };

        self.state.add_project(Project::new(root, source));
    }

    /// Whether the loop should stop.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Starts a pane running `harness` in the selected project.
    pub fn spawn_pane(&mut self, harness: &str, area: Size) -> Result<()> {
        let Some(project_id) = self.state.selected_project() else {
            self.status = "no project selected".into();
            return Ok(());
        };

        let Some(def) = self.harnesses.get(harness) else {
            self.status = format!("unknown harness {harness:?}");
            return Ok(());
        };

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

        let mut reader = ScreenReader::new().context("failed to create a screen reader")?;
        let screen = reader
            .read(session.terminal())
            .context("failed to read the new pane")?;

        let id = self
            .state
            .spawn_pane(project_id, HarnessId::new(harness))
            .context("the selected project is registered")?;

        // The sidebar should read "Claude Code", not "claude". The harness id
        // is a filename; the display name is what the user chose to call it.
        // A title sequence from the child replaces this later.
        let _ = self.state.set_pane_title(id, &def.display_name);

        self.panes.insert(
            id,
            Pane {
                session,
                encoder: KeyEncoder::new().context("failed to create a key encoder")?,
                reader,
                screen,
            },
        );

        Ok(())
    }

    /// Feeds pending output into every pane and refreshes what changed.
    ///
    /// Returns whether anything needs redrawing.
    pub fn poll_panes(&mut self) -> bool {
        let mut changed = false;
        let mut exited = Vec::new();

        for (id, pane) in &mut self.panes {
            if !pane.session.drain() {
                continue;
            }

            changed = true;

            if let Ok(screen) = pane.reader.read(pane.session.terminal()) {
                pane.screen = screen;
            }

            if let RunState::Exited(code) = pane.session.state() {
                exited.push((*id, code));
            }
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
        // A picker takes the keyboard while it is open, so arrow keys choose
        // rather than reaching an agent.
        if self.overlay.is_some() {
            return self.handle_overlay(event, area);
        }

        let layout = std::mem::take(&mut self.layout);
        let action = self.router.handle(event, &layout);
        self.layout = layout;

        match action {
            Action::None => {}
            Action::Quit => self.quit = true,
            Action::SendKey(key, mods) => self.send_key(key, mods),
            Action::Paste(text) => self.paste(&text),
            Action::FocusPane(id) => {
                let _ = self.state.focus(id);
            }
            Action::FocusDirection(direction) => self.focus_direction(direction),
            Action::ToggleZoom => self.state.toggle_zoom(),
            Action::ClosePane => self.close_focused(),
            Action::NewPane => self.open_harness_picker(),
            Action::ProjectPicker => self.open_project_picker(),
            Action::HarnessManager => self.open_harness_manager(),
            Action::Scrollback => {
                self.status = "scrollback is not implemented yet".into();
            }
        }

        Ok(())
    }

    /// Handles input while a picker is open.
    fn handle_overlay(&mut self, event: &Event, area: Size) -> Result<()> {
        let Event::Key(key) = event else {
            return Ok(());
        };
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some((_, picker)) = &mut self.overlay {
                    picker.next();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some((_, picker)) = &mut self.overlay {
                    picker.previous();
                }
            }
            KeyCode::Enter => {
                let chosen = self.overlay.as_ref().and_then(|(kind, picker)| {
                    picker.selected().map(|i| (kind_of(kind), i.id.clone()))
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

        self.overlay = Some((Overlay::Harness, Picker::new("New pane", items)));
    }

    fn open_project_picker(&mut self) {
        let items: Vec<Item> = self
            .state
            .projects()
            .iter()
            .map(|p| Item::new(p.id.to_string(), &p.name).with_detail(p.root.display().to_string()))
            .collect();

        self.overlay = Some((Overlay::Project, Picker::new("Project", items)));
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

        self.overlay = Some((Overlay::Register, Picker::new("Add harness", items)));
    }

    fn send_key(&mut self, key: dispatch_pty::Key, mods: dispatch_pty::Modifiers) {
        let Some(id) = self.state.focused_pane() else {
            return;
        };
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };

        // Typing into a pane whose process has exited would go nowhere, and
        // the write would fail every keystroke.
        if !matches!(pane.session.state(), RunState::Running) {
            return;
        }

        match pane.encoder.encode(pane.session.terminal(), key, mods) {
            Ok(bytes) if !bytes.is_empty() => {
                if let Err(error) = pane.session.write(&bytes) {
                    tracing::warn!(%error, "failed to write to a pane");
                }
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to encode a key"),
        }
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

        if let Err(error) = pane.session.write(&bytes) {
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
            pane.session.terminate();
        }

        let _ = self.state.close_pane(id);
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

        self.layout = self.compute_layout(panes_area);
        self.draw_panes(frame);
        self.draw_status(frame, area);

        if let Some((_, picker)) = &self.overlay {
            frame.render_widget(picker, panes_area);
        }
    }

    /// Where each visible pane goes this frame.
    fn compute_layout(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let visible: Vec<PaneId> = self.state.visible_panes().iter().map(|p| p.id).collect();

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
            format!("{panes} pane(s)  ^a n new  ^a x close  ^a z zoom  ^a q quit")
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
            if size == pane.session.size() {
                continue;
            }

            if let Err(error) = pane.session.resize(size) {
                tracing::warn!(%error, "failed to resize a pane");
                continue;
            }

            if let Ok(screen) = pane.reader.read(pane.session.terminal()) {
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
