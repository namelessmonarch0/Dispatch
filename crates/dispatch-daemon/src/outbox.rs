//! What the daemon has queued for each client, counted in bytes.
//!
//! A client that stops reading leaves its writer thread parked on the
//! socket, and everything broadcast after that waits in its queue. Two
//! counts are kept apart: live traffic the fleet produces on its own --
//! output, statuses, prompts -- and what the client itself asked for, such
//! as an answer or the replay a subscription begins with. Only the live
//! count is judged against a client's budget for the fleet it is watching;
//! the asked-for count is bounded too, so a client cannot grow its queue
//! forever by asking over and over, but a single reply or replay that clears
//! that check is queued whole and never split or refused partway through by
//! its own bulk.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::Duration;

use dispatch_proto::ServerMessage;

/// Which of an outbox's two counts a queued message was charged to.
#[derive(Debug, Clone, Copy)]
enum Charge {
    /// The fleet's own traffic, judged against a client's budget.
    Live,
    /// What the client itself asked for: bounded separately so it cannot
    /// grow the queue forever by asking, but never re-checked once a reply
    /// has cleared the check and is on its way in.
    Asked,
}

/// One message waiting in a queue, with the count and weight it was charged
/// so the reader can credit the right one back once it is taken.
#[derive(Debug)]
struct Entry {
    message: ServerMessage,
    charge: Charge,
    weight: usize,
}

/// The daemon's end of one client's queue.
#[derive(Debug)]
pub struct Outbox {
    sender: Sender<Entry>,
    live: Arc<AtomicUsize>,
    asked: Arc<AtomicUsize>,
}

/// The other end, drained by whatever writes to the client.
///
/// Public because a test holds a client's end directly:
/// `Daemon::attach_for_test` hands one back.
#[derive(Debug)]
pub struct Inbox {
    receiver: Receiver<Entry>,
    live: Arc<AtomicUsize>,
    asked: Arc<AtomicUsize>,
}

/// Why a message was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing drains the queue any more: the client has gone.
    Gone,
    /// The client has let this many bytes pile up, in whichever count was
    /// judged.
    Behind {
        /// Bytes already waiting.
        queued: usize,
    },
}

/// A new, empty queue.
pub fn pair() -> (Outbox, Inbox) {
    let (sender, receiver) = channel();
    let live = Arc::new(AtomicUsize::new(0));
    let asked = Arc::new(AtomicUsize::new(0));
    (
        Outbox {
            sender,
            live: Arc::clone(&live),
            asked: Arc::clone(&asked),
        },
        Inbox {
            receiver,
            live,
            asked,
        },
    )
}

impl Outbox {
    /// Queues every message in `messages` as one reply the client asked
    /// for -- a `Subscribe` reply's whole catch-up, in one call.
    ///
    /// Checked once, against the asked-for backlog already waiting, rather
    /// than once per message: a reply that clears the check is delivered
    /// whole, never split or refused partway through by its own bulk. Not
    /// judged against a client's live-traffic budget at all -- that is what
    /// distinguishes this from `send_within` -- but a client cannot grow
    /// this queue forever by asking for things it never reads either, so
    /// asking again before the last answer was taken is refused as
    /// [`Refused::Behind`].
    pub fn send_all(
        &self,
        messages: impl IntoIterator<Item = ServerMessage>,
        budget: usize,
    ) -> Result<(), Refused> {
        let asked = self.asked.load(Ordering::Acquire);
        if asked > budget {
            return Err(Refused::Behind { queued: asked });
        }

        for message in messages {
            self.queue(message, Charge::Asked)?;
        }
        Ok(())
    }

    /// Queues `message` unless `budget` bytes of live traffic are already
    /// waiting.
    ///
    /// For what the fleet does on its own -- output, statuses, prompts --
    /// which is what piles up behind a client that has stopped reading.
    /// What the client asked for does not count here: see [`Self::send`].
    pub fn send_within(&self, message: ServerMessage, budget: usize) -> Result<(), Refused> {
        let live = self.live.load(Ordering::Acquire);
        if live > budget {
            return Err(Refused::Behind { queued: live });
        }
        self.queue(message, Charge::Live)
    }

    fn queue(&self, message: ServerMessage, charge: Charge) -> Result<(), Refused> {
        let weight = weight(&message);
        let counter = match charge {
            Charge::Live => &self.live,
            Charge::Asked => &self.asked,
        };

        counter.fetch_add(weight, Ordering::AcqRel);
        self.sender
            .send(Entry {
                message,
                charge,
                weight,
            })
            .map_err(|_| {
                counter.fetch_sub(weight, Ordering::AcqRel);
                Refused::Gone
            })
    }
}

impl Inbox {
    /// Waits for the next message; `None` once the daemon has let go of this
    /// client and everything queued has been taken.
    pub fn recv(&self) -> Option<ServerMessage> {
        let entry = self.receiver.recv().ok()?;
        self.took(&entry);
        Some(entry.message)
    }

    /// Waits up to `timeout` for the next message.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<ServerMessage, RecvTimeoutError> {
        let entry = self.receiver.recv_timeout(timeout)?;
        self.took(&entry);
        Ok(entry.message)
    }

    /// Takes the next message if there is one.
    pub fn try_recv(&self) -> Result<ServerMessage, TryRecvError> {
        let entry = self.receiver.try_recv()?;
        self.took(&entry);
        Ok(entry.message)
    }

    /// Takes every message queued now.
    pub fn try_iter(&self) -> impl Iterator<Item = ServerMessage> + '_ {
        std::iter::from_fn(move || self.try_recv().ok())
    }

    fn took(&self, entry: &Entry) {
        let counter = match entry.charge {
            Charge::Live => &self.live,
            Charge::Asked => &self.asked,
        };
        counter.fetch_sub(entry.weight, Ordering::AcqRel);
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
