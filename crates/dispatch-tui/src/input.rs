//! Deciding what a keystroke or mouse event means.
//!
//! A focused pane receives every key verbatim, so an agent's own full-screen
//! interface works unchanged. A prefix key escapes to Dispatch's commands,
//! which is the only way to have both without stealing bindings the agents
//! already use.

use dispatch_core::PaneId;
use dispatch_pty::{Key, Modifiers};
use ratatui::layout::Rect;

pub use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

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
    /// Enter scrollback mode.
    Scrollback,
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
        match event.kind {
            // Focus follows the pointer, so moving the mouse over a pane is
            // enough to type into it.
            MouseEventKind::Moved => panes
                .iter()
                .find(|(_, rect)| contains(*rect, event.column, event.row))
                .map_or(Action::None, |(id, _)| Action::FocusPane(*id)),
            _ => Action::None,
        }
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
        KeyCode::Char('[') => Action::Scrollback,
        KeyCode::Char('q') => Action::Quit,
        // An unbound key after the prefix does nothing rather than reaching
        // the pane, so a mistyped command cannot run something in an agent.
        _ => Action::None,
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
