//! Deciding what a keystroke or mouse event means.
//!
//! A focused pane receives every key verbatim, so an agent's own full-screen
//! interface works unchanged. A prefix key escapes to Dispatch's commands,
//! which is the only way to have both without stealing bindings the agents
//! already use.

use dispatch_core::PaneId;
use dispatch_pty::{Key, Modifiers, MouseAction, MouseButton, MouseInput};
use ratatui::layout::Rect;

pub use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};

// Renamed to keep it distinct from the encoder's own button type, which this
// module converts into.
use crossterm::event::MouseButton as MouseButton_;

/// Which way to move focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Left.
    Left,
    /// Down.
    Down,
    /// Up.
    Up,
    /// Right.
    Right,
}

/// What an input event should cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing.
    None,
    /// Send a key to the focused pane.
    ///
    /// Carries the key rather than bytes: encoding depends on the receiving
    /// pane's current modes, which only the caller knows.
    SendKey(Key, Modifiers),
    /// Paste text into the focused pane.
    Paste(String),
    /// Focus a pane, from the mouse moving over it.
    FocusPane(PaneId),
    /// Forward a pointer event to a pane, in coordinates relative to it.
    SendMouse(PaneId, MouseInput),
    /// Scroll the focused pane's viewport by a signed number of rows.
    Scroll(isize),
    /// Move focus in a direction.
    FocusDirection(Direction),
    /// Open the pane picker.
    NewPane,
    /// Close the focused pane.
    ClosePane,
    /// Zoom the focused pane, or restore the grid.
    ToggleZoom,
    /// Open the project picker.
    ProjectPicker,
    /// Open the harness manager.
    HarnessManager,
    /// Show the tab holding this many panes in, counting from zero.
    SelectTab(usize),
    /// Move to the next tab, wrapping.
    NextTab,
    /// Enter scrollback mode.
    Scrollback,
    /// Reopen the approval prompt for whatever delegation requests are queued.
    Approvals,
    /// Focus the focused pane's next child, opening it into the tiled grid.
    ///
    /// Cycles through several children one at a time — the mouse is not the
    /// only way to reach a subagent's pane.
    ExpandChild,
    /// If the focused pane is a subagent, remove it from the tiled grid and
    /// return focus to its parent.
    CollapseChild,
    /// Quit.
    Quit,
}

/// The key that escapes to Dispatch's own commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    /// The character.
    pub code: char,
    /// The modifiers held with it.
    pub modifiers: KeyModifiers,
}

impl Default for Prefix {
    fn default() -> Self {
        // Ctrl-a, as tmux and screen use. Agents do not bind it, and users
        // coming from either already have the habit.
        Self {
            code: 'a',
            modifiers: KeyModifiers::CONTROL,
        }
    }
}

impl Prefix {
    fn matches(&self, event: &KeyEvent) -> bool {
        event.code == KeyCode::Char(self.code) && event.modifiers == self.modifiers
    }
}

/// Routes input to panes or to Dispatch.
#[derive(Debug, Default)]
pub struct InputRouter {
    prefix: Prefix,
    /// Whether the prefix was the previous key, so this one is a command.
    armed: bool,
}

impl InputRouter {
    /// Creates a router using the default prefix.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a router using `prefix`.
    #[must_use]
    pub fn with_prefix(prefix: Prefix) -> Self {
        Self {
            prefix,
            armed: false,
        }
    }

    /// Whether the next key will be read as a command.
    ///
    /// Shown in the status bar: a prefix that armed invisibly is how a
    /// keystroke goes missing with no explanation.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Decides what an event means.
    ///
    /// `panes` gives the on-screen rectangle of each pane, used to resolve
    /// which one the pointer is over.
    pub fn handle(&mut self, event: &Event, panes: &[(PaneId, Rect)]) -> Action {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse, panes),
            Event::Paste(text) => Action::Paste(text.clone()),
            _ => Action::None,
        }
    }

    fn handle_key(&mut self, event: &KeyEvent) -> Action {
        // Key releases and repeats reach us on some platforms. Only presses
        // should act, or every keystroke would fire twice.
        if event.kind != KeyEventKind::Press {
            return Action::None;
        }

        if self.armed {
            self.armed = false;

            // The prefix twice sends the prefix itself, which is the only way
            // to type it into a pane.
            if self.prefix.matches(event) {
                return Action::SendKey(
                    Key::Char(self.prefix.code),
                    modifiers_of(self.prefix.modifiers),
                );
            }

            return command_for(event);
        }

        if self.prefix.matches(event) {
            self.armed = true;
            return Action::None;
        }

        match translate(event) {
            Some((key, mods)) => Action::SendKey(key, mods),
            None => Action::None,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent, panes: &[(PaneId, Rect)]) -> Action {
        let Some((id, rect)) = panes
            .iter()
            .find(|(_, rect)| contains(*rect, event.column, event.row))
        else {
            // The sidebar and status row are not panes. Moving across them
            // must not drop focus or send anything anywhere.
            return Action::None;
        };

        // Coordinates are relative to the pane, because that is the only frame
        // of reference the child has.
        let col = event.column - rect.x;
        let row = event.row - rect.y;
        let modifiers = modifiers_of(event.modifiers);

        let (action, button) = match event.kind {
            // Focus follows the pointer, so moving the mouse over a pane is
            // enough to type into it.
            MouseEventKind::Moved => return Action::FocusPane(*id),
            MouseEventKind::Down(button) => (MouseAction::Press, translate_button(button)),
            MouseEventKind::Up(button) => (MouseAction::Release, translate_button(button)),
            MouseEventKind::Drag(button) => (MouseAction::Motion, translate_button(button)),
            // Scrolling is handled by Dispatch when the pane is not tracking
            // the mouse, which the caller decides; sending it as a wheel
            // button lets a pane that does track it receive it instead.
            MouseEventKind::ScrollUp => (MouseAction::Press, MouseButton::WheelUp),
            MouseEventKind::ScrollDown => (MouseAction::Press, MouseButton::WheelDown),
            MouseEventKind::ScrollLeft => (MouseAction::Press, MouseButton::WheelLeft),
            MouseEventKind::ScrollRight => (MouseAction::Press, MouseButton::WheelRight),
        };

        Action::SendMouse(
            *id,
            MouseInput {
                action,
                button,
                col,
                row,
                modifiers,
            },
        )
    }
}

/// Whether `rect` covers the cell at `(x, y)`.
fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// The command a key means once the prefix has armed.
fn command_for(event: &KeyEvent) -> Action {
    match event.code {
        KeyCode::Char('n') => Action::NewPane,
        KeyCode::Char('x') => Action::ClosePane,
        KeyCode::Char('z') => Action::ToggleZoom,
        KeyCode::Char('h') => Action::FocusDirection(Direction::Left),
        KeyCode::Char('j') => Action::FocusDirection(Direction::Down),
        KeyCode::Char('k') => Action::FocusDirection(Direction::Up),
        KeyCode::Char('l') => Action::FocusDirection(Direction::Right),
        KeyCode::Char('p') => Action::ProjectPicker,
        KeyCode::Char('H') => Action::HarnessManager,
        // `p` is already the project picker, so approvals go on `a` instead —
        // mnemonic with the `a` that approves one once the prompt is open.
        KeyCode::Char('a') => Action::Approvals,
        // A subagent otherwise has no keyboard way in: `FocusDirection`
        // searches the tiled grid, which excludes an unopened child by
        // construction, and a click is not available over SSH without mouse
        // reporting.
        KeyCode::Char('s') => Action::ExpandChild,
        KeyCode::Char('c') => Action::CollapseChild,
        KeyCode::Char('[') => Action::Scrollback,
        // A grid holds four panes at most, so a fifth opens a tab rather than
        // shrinking the other four into unreadability. Digits pick one
        // directly; Tab walks them for anyone who would rather not count.
        KeyCode::Char(digit @ '1'..='9') => {
            Action::SelectTab(digit.to_digit(10).unwrap_or(1) as usize - 1)
        }
        KeyCode::Tab => Action::NextTab,
        KeyCode::Char('q') => Action::Quit,
        // An unbound key after the prefix does nothing rather than reaching
        // the pane, so a mistyped command cannot run something in an agent.
        _ => Action::None,
    }
}

/// Converts a crossterm button into the encoder's.
fn translate_button(button: MouseButton_) -> MouseButton {
    match button {
        MouseButton_::Left => MouseButton::Left,
        MouseButton_::Middle => MouseButton::Middle,
        MouseButton_::Right => MouseButton::Right,
    }
}

/// Converts crossterm modifiers into the encoder's.
fn modifiers_of(m: KeyModifiers) -> Modifiers {
    Modifiers {
        shift: m.contains(KeyModifiers::SHIFT),
        ctrl: m.contains(KeyModifiers::CONTROL),
        alt: m.contains(KeyModifiers::ALT),
        super_: m.contains(KeyModifiers::SUPER),
    }
}

/// Converts a crossterm key into the encoder's.
///
/// Returns nothing for keys with no meaning to a child, such as a bare
/// modifier press.
fn translate(event: &KeyEvent) -> Option<(Key, Modifiers)> {
    let key = match event.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Escape,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::F(n) => Key::Function(n),
        _ => return None,
    };

    let mut mods = modifiers_of(event.modifiers);

    // Shift-Tab arrives as its own code with the modifier already folded in.
    if event.code == KeyCode::BackTab {
        mods.shift = true;
    }

    Some((key, mods))
}

#[cfg(test)]
mod tests;
