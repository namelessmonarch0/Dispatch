//! `dispatch-tui`: rendering, input routing and keymap.

pub mod input;
pub mod pane;
pub mod picker;
pub mod sidebar;

pub use input::{Action, Direction, InputRouter, Prefix};
pub use pane::PaneWidget;
pub use picker::{Item, Picker};
pub use sidebar::Sidebar;
