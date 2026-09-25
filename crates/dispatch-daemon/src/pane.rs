//! A pane as the daemon sees it.

use dispatch_core::{PaneId, PaneStatus};
use dispatch_pty::Pty;

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
/// ones drawing. One here would parse every byte a second time and hold a screen
/// nothing ever reads.
pub struct DaemonPane {
    /// Stable identifier.
    pub id: PaneId,
    /// The running process.
    pub session: Pty,
    /// Which harness is running, so a client attaching later can be told.
    pub harness: String,
    /// The project it belongs to.
    pub project: dispatch_core::ProjectId,
    /// What the pane has printed, up to [`HISTORY_BYTES`].
    pub history: Vec<u8>,
    /// What the pane is doing, as last reported to clients.
    pub status: PaneStatus,
    /// The pane that delegated this one's work.
    pub parent: Option<PaneId>,
    /// Whether it outlives the caller that asked for it.
    pub durable: bool,
    /// The request it answers, while one is waiting.
    pub request: Option<dispatch_core::RequestId>,
    /// Which client is waiting, so its disappearance can end a one-off pane.
    pub caller: Option<u64>,
    /// When the process was seen to exit, if it has.
    ///
    /// The exit and the last of the output are separate events, so a caller
    /// waiting on this pane's output cannot be answered at the exit. This is how
    /// long that wait has lasted.
    pub exited_at: Option<std::time::Instant>,
    /// The branch last reported to clients, so a look that finds the same
    /// one says nothing.
    pub branch: Option<String>,
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
