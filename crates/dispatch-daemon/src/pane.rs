//! A pane as the daemon sees it.

use dispatch_core::{PaneId, PaneStatus};
use dispatch_pty::PtySession;

/// How much of each pane's output the daemon keeps for a client attaching later.
///
/// A client that reattaches is told what a pane printed rather than being given
/// a blank screen, which is the difference between reattaching and starting
/// again. The daemon holds bytes rather than a screen because it has no
/// emulator: it replays what it forwarded.
pub const HISTORY_BYTES: usize = 256 * 1024;

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
    /// What the pane has printed, up to [`HISTORY_BYTES`].
    pub history: Vec<u8>,
    /// What the pane is doing, as last reported to clients.
    pub status: PaneStatus,
}

impl DaemonPane {
    /// Records output for a client attaching later.
    ///
    /// The oldest bytes go first once the limit is reached. That can cut an
    /// escape sequence in half, which a terminal parser resynchronises from
    /// within a few bytes; the alternative — keeping everything an agent ever
    /// printed — is unbounded memory.
    pub fn remember(&mut self, output: &[u8]) {
        if output.len() >= HISTORY_BYTES {
            self.history.clear();
            self.history
                .extend_from_slice(&output[output.len() - HISTORY_BYTES..]);
            return;
        }

        self.history.extend_from_slice(output);

        let excess = self.history.len().saturating_sub(HISTORY_BYTES);
        if excess > 0 {
            self.history.drain(..excess);
        }
    }
}
