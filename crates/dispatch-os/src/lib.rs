//! `dispatch-os`.
//!
//! All platform-specific behaviour lives here. No other crate in the
//! workspace carries a `#[cfg(windows)]`.

pub mod dll;
pub mod ipc;
pub mod paths;
pub mod process;
pub mod signal;
