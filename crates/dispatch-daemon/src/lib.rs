//! `dispatchd`: the process that owns the agents.
//!
//! Moving pane ownership out of the TUI is what makes agents survive the
//! client dying, which is the point of the split: close the laptop, reattach
//! later, and the work is still running.
//!
//! Several clients can attach at once and all see the same panes. That is what
//! lets a MacBook and a desktop show the same fleet, and it is why output is
//! broadcast rather than handed to one owner.

mod delegation;
mod pane;
mod session;

pub use session::{Daemon, DaemonError, Shutdown};
