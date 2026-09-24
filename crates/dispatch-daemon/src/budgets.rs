//! What one client may cost the daemon.

use std::time::Duration;

/// Limits on what one client can cost the daemon.
///
/// Every client is the same user, so these are not a defence against an
/// attacker: they keep one slow, stuck or broken client from impairing
/// everyone else's view of the fleet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    /// Bytes of the fleet's own traffic -- output, statuses, prompts -- that
    /// may wait for one client before it is hung up on.
    ///
    /// Judges only what the fleet does on its own, never what a client asked
    /// for: an answer, or the replay a `Subscribe` begins with, is bounded
    /// separately and delivered whole. Far above what a reading client ever
    /// has queued, far below what an agent printing for an afternoon
    /// produces.
    pub outbox_bytes: usize,
    /// How long a client may take to say `Hello`.
    ///
    /// A client sends it the moment it connects; ten seconds is one that is
    /// not going to.
    pub handshake: Duration,
    /// How long a client may take to finish a frame it has started.
    ///
    /// Between frames a quiet client is an idle one. Part-way through one it
    /// is a stalled one -- its reader thread is parked mid-read -- and
    /// thirty seconds covers a large paste over a slow link.
    pub frame: Duration,
    /// How many clients may be connected at once.
    ///
    /// Each costs two threads and a queue. A fleet is a handful of screens
    /// and a few delegate calls in flight; sixty-four is a leak.
    pub max_clients: usize,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            outbox_bytes: 32 * 1024 * 1024,
            handshake: Duration::from_secs(10),
            frame: Duration::from_secs(30),
            max_clients: 64,
        }
    }
}
