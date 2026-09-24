//! What the daemon has queued for each client, counted in bytes.
//!
//! A client that stops reading leaves its writer thread parked on the
//! socket, and everything broadcast after that waits in its queue. Counted,
//! the queue can be given a limit: past it the daemon stops queueing and
//! hangs up, and the client's reconnection replays what it missed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::Duration;

use dispatch_proto::ServerMessage;

/// The daemon's end of one client's queue.
#[derive(Debug)]
pub struct Outbox {
    sender: Sender<ServerMessage>,
    queued: Arc<AtomicUsize>,
}

/// The other end, drained by whatever writes to the client.
///
/// Public because a test holds a client's end directly:
/// `Daemon::attach_for_test` hands one back.
#[derive(Debug)]
pub struct Inbox {
    receiver: Receiver<ServerMessage>,
    queued: Arc<AtomicUsize>,
}

/// Why a message was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing drains the queue any more: the client has gone.
    Gone,
    /// The client has let this many bytes pile up.
    Behind {
        /// Bytes already waiting.
        queued: usize,
    },
}

/// A new, empty queue.
pub fn pair() -> (Outbox, Inbox) {
    let (sender, receiver) = channel();
    let queued = Arc::new(AtomicUsize::new(0));
    (
        Outbox {
            sender,
            queued: Arc::clone(&queued),
        },
        Inbox { receiver, queued },
    )
}

impl Outbox {
    /// Queues `message` whatever is already waiting.
    ///
    /// For what a client asked for: an answer, or the replay a subscription
    /// begins with. A client is not hung up on for the size of the fleet it
    /// asked to be shown.
    pub fn send(&self, message: ServerMessage) -> Result<(), Refused> {
        let weight = weight(&message);
        self.queued.fetch_add(weight, Ordering::AcqRel);
        self.sender.send(message).map_err(|_| {
            self.queued.fetch_sub(weight, Ordering::AcqRel);
            Refused::Gone
        })
    }

    /// Queues `message` unless `budget` bytes are already waiting.
    ///
    /// For what the fleet does on its own -- output, statuses, prompts --
    /// which is what piles up behind a client that has stopped reading.
    pub fn send_within(&self, message: ServerMessage, budget: usize) -> Result<(), Refused> {
        let queued = self.queued.load(Ordering::Acquire);
        if queued > budget {
            return Err(Refused::Behind { queued });
        }
        self.send(message)
    }
}

impl Inbox {
    /// Waits for the next message; `None` once the daemon has let go of this
    /// client and everything queued has been taken.
    pub fn recv(&self) -> Option<ServerMessage> {
        let message = self.receiver.recv().ok()?;
        self.took(&message);
        Some(message)
    }

    /// Waits up to `timeout` for the next message.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<ServerMessage, RecvTimeoutError> {
        let message = self.receiver.recv_timeout(timeout)?;
        self.took(&message);
        Ok(message)
    }

    /// Takes the next message if there is one.
    pub fn try_recv(&self) -> Result<ServerMessage, TryRecvError> {
        let message = self.receiver.try_recv()?;
        self.took(&message);
        Ok(message)
    }

    /// Takes every message queued now.
    pub fn try_iter(&self) -> impl Iterator<Item = ServerMessage> + '_ {
        std::iter::from_fn(move || self.try_recv().ok())
    }

    fn took(&self, message: &ServerMessage) {
        self.queued.fetch_sub(weight(message), Ordering::AcqRel);
    }
}

/// Roughly what holding a message costs: its bulk, plus an allowance for
/// the rest. Only output, a subagent's tail and a task are ever large.
fn weight(message: &ServerMessage) -> usize {
    const ENVELOPE: usize = 64;

    ENVELOPE
        + match message {
            ServerMessage::PaneOutput { bytes, .. } => bytes.len(),
            ServerMessage::DelegateFinished { tail, .. } => tail.len(),
            ServerMessage::DelegatePending { task, .. } => task.len(),
            _ => 0,
        }
}
