//! A pane as the daemon sees it.

use dispatch_core::PaneId;
use dispatch_pty::PtySession;

/// One agent owned by the daemon.
///
/// The daemon holds no emulator: clients run their own, because they are the
/// ones drawing. Keeping one here would mean rendering a screen per client per
/// frame for no gain.
pub struct DaemonPane {
    /// Stable identifier.
    pub id: PaneId,
    /// The running process.
    pub session: PtySession,
    /// Which harness is running, so a client attaching later can be told.
    pub harness: String,
    /// The project it belongs to.
    pub project: dispatch_core::ProjectId,
}
