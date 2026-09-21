//! `dispatch-core`: domain types and state. No I/O.

pub mod device;
pub mod id;
pub mod pane;
pub mod project;
pub mod state;

pub use device::Device;
pub use id::{DeviceId, PaneId, ProjectId, RequestId};
pub use pane::{HarnessId, Pane, PaneRole, PaneStatus};
pub use project::{Project, ProjectSource};
pub use state::{AppState, StateError};
