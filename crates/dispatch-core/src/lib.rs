//! `dispatch-core`: domain types and state. No I/O.

pub mod id;
pub mod pane;
pub mod project;
pub mod state;

pub use id::{DeviceId, PaneId, ProjectId};
pub use pane::{HarnessId, Pane, PaneRole, PaneStatus};
pub use project::{Project, ProjectSource};
pub use state::{AppState, StateError};
