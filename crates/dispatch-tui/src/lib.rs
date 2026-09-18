//! `dispatch-tui`: rendering, input routing and keymap.

pub mod input;
pub mod pane;
pub mod sidebar;

pub use input::{Action, Direction, InputRouter, Prefix};
pub use pane::PaneWidget;
pub use sidebar::Sidebar;
