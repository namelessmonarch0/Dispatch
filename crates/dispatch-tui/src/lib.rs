//! `dispatch-tui`: rendering, input routing and keymap.

pub mod activity;
pub mod browser;
pub mod input;
pub mod motion;
pub mod pane;
pub mod picker;
pub mod prompt;
pub mod sidebar;
pub mod theme;

pub use input::{Action, Direction, InputRouter, KeyMode, Prefix};
pub use pane::PaneWidget;
pub use picker::{Item, Picker};
pub use prompt::{Note, Prompt};
pub use sidebar::{Sidebar, truncate};
pub use theme::Theme;
