//! `dispatch-core`: domain types and state. No I/O.

pub mod device;
pub mod id;
pub mod pane;
pub mod project;
pub mod state;
pub mod tabs;

pub use device::Device;
pub use id::{DeviceId, PaneId, ProjectId, RequestId, TabId};
pub use pane::{HarnessId, Pane, PaneRole, PaneStatus};
pub use project::{Project, ProjectSource};
pub use state::{AppState, StateError};
pub use tabs::{NAME_LIMIT, Placement, ProjectTabs, TAB_CAPACITY, Tab, TabError};
