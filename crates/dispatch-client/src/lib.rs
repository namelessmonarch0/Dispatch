//! The client half of the daemon protocol.
//!
//! Keeps the socket off the interface's thread. Reading, writing and
//! reconnecting each get one of their own, so a chatty agent cannot stall a
//! redraw and a redraw cannot stall the socket; what reaches the interface is a
//! queue of messages it drains whenever it likes.
//!
//! The connection is supervised rather than owned: a daemon that is restarted,
//! or a socket that breaks, is reconnected to on its own. That matters because
//! the agents are on the daemon's side — the work is still running, and a client
//! that gave up would leave the user with a dead window over live agents.
//!
//! Nothing here knows about panes or drawing. It connects, shakes hands, and
//! moves messages.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dispatch_os::ipc::{Connection, IpcError};
use dispatch_proto::{ClientMessage, Frame, ProtocolError, Role, ServerMessage};

/// How long to wait before the first reconnection attempt.
const FIRST_RETRY: Duration = Duration::from_millis(100);

/// How often to ask a quiet daemon whether it is still there, and how long to
/// wait for an answer before deciding it is not.
///
/// A socket can be up as far as this end is concerned while nothing can cross
/// it: a forwarded connection whose tunnel died, or a peer wedged mid-write.
/// Nothing arrives, nothing fails, and without a question being asked the client
/// would wait for output forever. Asking costs one frame every few seconds.
#[derive(Debug, Clone, Copy)]
pub struct Liveness {
    /// How long to wait, having heard nothing, before asking.
    pub interval: Duration,
    /// How long to go unanswered before treating the connection as lost.
    pub silence: Duration,
}

impl Default for Liveness {
    fn default() -> Self {
        // Four unanswered questions. Long enough that a daemon busy with a
        // hundred panes is not mistaken for a dead one.
        Self {
            interval: Duration::from_secs(5),
            silence: Duration::from_secs(20),
        }
    }
}

/// How long to wait for a daemon to answer the handshake.
///
/// A peer that accepts a connection and then says nothing would otherwise stop
/// the client for good: the handshake is read before anything else happens, so
/// without a limit one unanswering socket ends both attaching and reconnecting.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// The longest gap between reconnection attempts.
///
/// A daemon being restarted is back within a second or two, and a daemon that
/// is gone for good should not cost more than a connect attempt every couple of
/// seconds.
const MAX_RETRY: Duration = Duration::from_secs(2);

/// Failures attaching to a daemon.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No daemon is listening.
    ///
    /// Kept separate from the other transport failures because it is the one a
    /// user is expected to hit, and the answer to it is to start one.
    #[error("no daemon is listening on {0}")]
    NotRunning(String),

    /// The transport failed.
    #[error(transparent)]
    Ipc(#[from] IpcError),

    /// The handshake could not be sent or read.
    #[error("the handshake failed: {0}")]
    Handshake(String),

    /// The daemon refused the connection.
    #[error(transparent)]
    Refused(ProtocolError),

    /// The daemon answered the handshake with something else.
    #[error("expected a welcome, got {0}")]
    Unexpected(String),
}

/// The socket, and what is known about it.
///
/// Shared by the reader, the writer and the supervisor. The writer is behind a
/// lock and behind an `Option` because reconnecting replaces it: senders keep
/// the same [`Handle`] across a reconnection rather than being handed a new one.
struct Wire {
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    connected: AtomicBool,
    /// Incremented for each connection. A change tells a caller its view is of
    /// a connection that no longer exists and has to be rebuilt.
    generation: AtomicU64,
    /// Whether to resubscribe on reconnecting.
    subscribed: AtomicBool,
    /// Set when the client is dropped, so the supervisor stops.
    closed: AtomicBool,
    /// What the daemon calls itself.
    device: Mutex<String>,
    /// What this client calls itself, for the daemon's log.
    name: String,
    /// What the connection is for, re-announced on every reconnection so a
    /// delegate caller that reconnects does not come back as an interface —
    /// which would start the fleet's output flowing to a call that only waits
    /// on one request.
    role: Role,
    /// The endpoint first reached, which every reconnection uses.
    endpoint: PathBuf,
    /// When anything last arrived, for deciding a silent socket is dead.
    last_heard: Mutex<Instant>,
    /// When the last question was asked, so one goes out per interval rather
    /// than on every pass of the supervisor.
    last_asked: Mutex<Instant>,
    /// How patient to be with silence.
    liveness: Liveness,
}

impl Wire {
    /// Writes one message, or reports that the connection is gone.
    fn write(&self, message: &ClientMessage) -> bool {
        let mut guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());

        let Some(writer) = guard.as_mut() else {
            // Disconnected: dropped rather than queued. A keystroke that
            // arrives at an agent minutes later, out of order with the rest,
            // is worse than one that never arrives.
            return false;
        };

        if Frame::write(writer, message).is_err() {
            *guard = None;
            self.connected.store(false, Ordering::Relaxed);
            return false;
        }

        true
    }

    /// Records that something arrived.
    fn heard(&self) {
        *self.last_heard.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        *self.last_asked.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// How long nothing has arrived for.
    fn quiet_for(&self) -> Duration {
        self.last_heard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed()
    }

    /// Records that the connection has broken.
    fn lost(&self) {
        *self.writer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.connected.store(false, Ordering::Relaxed);
    }
}

/// Sends to a daemon.
///
/// Cheap to clone, so whatever owns a pane can keep one rather than reaching
/// back through the application for every keystroke. Survives a reconnection:
/// the handle addresses the connection, not one socket.
#[derive(Clone)]
pub struct Handle {
    outbox: Sender<ClientMessage>,
    wire: Arc<Wire>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handle")
    }
}

impl Handle {
    /// Queues a message. Returns whether the connection is up.
    ///
    /// A dropped connection is not an error here: the interface has already
    /// been told, and failing a keystroke it can do nothing about would only
    /// add noise.
    pub fn send(&self, message: ClientMessage) -> bool {
        if self.outbox.send(message).is_err() {
            self.wire.lost();
            return false;
        }

        self.is_connected()
    }

    /// Whether the connection is up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.wire.connected.load(Ordering::Relaxed)
    }
}

/// An attached daemon connection.
pub struct Client {
    handle: Handle,
    inbox: Receiver<ServerMessage>,
    wire: Arc<Wire>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client")
    }
}

impl Client {
    /// Connects and shakes hands as an interface client.
    ///
    /// `name` is what the daemon logs this client as. Attaching does not ask
    /// for pane events; call [`Client::subscribe`] for those.
    ///
    /// Failing here means no daemon answered now. Once attached, a connection
    /// that breaks is reconnected to rather than reported as an error.
    pub fn attach(name: &str) -> Result<Self, ClientError> {
        Self::attach_as(Role::Interface, name)
    }

    /// Connects and shakes hands as an interface client, with something other
    /// than the usual patience for silence.
    ///
    /// Exists for tests, which cannot wait the tens of seconds a real client
    /// should wait before declaring a quiet daemon dead.
    pub fn attach_with(name: &str, liveness: Liveness) -> Result<Self, ClientError> {
        Self::attach_with_as(Role::Interface, name, liveness)
    }

    /// Connects and shakes hands as the given role.
    ///
    /// A `dispatch delegate` call attaches as [`Role::Delegate`] so it is
    /// spared the fleet's output: an interface draws every pane, but a call
    /// waiting on one request would only have to filter that firehose back
    /// out.
    pub fn attach_as(role: Role, name: &str) -> Result<Self, ClientError> {
        Self::attach_with_as(role, name, Liveness::default())
    }

    /// Connects and shakes hands as the given role, with something other than
    /// the usual patience for silence.
    pub fn attach_with_as(role: Role, name: &str, liveness: Liveness) -> Result<Self, ClientError> {
        let endpoint = dispatch_os::ipc::endpoint()?;
        let (reader, writer, device) = connect_within(name, role, &endpoint, HANDSHAKE_TIMEOUT)?;

        let wire = Arc::new(Wire {
            writer: Mutex::new(Some(writer)),
            connected: AtomicBool::new(true),
            generation: AtomicU64::new(1),
            subscribed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            device: Mutex::new(device),
            name: name.to_string(),
            role,
            endpoint,
            last_heard: Mutex::new(Instant::now()),
            last_asked: Mutex::new(Instant::now()),
            liveness,
        });

        let (outbox, outgoing) = channel::<ClientMessage>();
        let (incoming, inbox) = channel::<ServerMessage>();

        read_from(reader, &incoming, &wire);
        write_to(outgoing, &wire);
        supervise(incoming, &wire);

        Ok(Self {
            handle: Handle {
                outbox,
                wire: Arc::clone(&wire),
            },
            inbox,
            wire,
        })
    }

    /// Asks for pane events, and for what already exists.
    ///
    /// Remembered: a reconnection subscribes again, so the panes come back
    /// without the caller having to notice the socket changed.
    pub fn subscribe(&self) -> bool {
        self.wire.subscribed.store(true, Ordering::Relaxed);
        self.send(ClientMessage::Subscribe)
    }

    /// What the daemon calls itself.
    #[must_use]
    pub fn device(&self) -> String {
        self.wire
            .device
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Which connection this is, counting from one.
    ///
    /// A caller that has built state from the daemon's messages compares this
    /// against what it built: a higher number means a different connection, and
    /// everything it was told belongs to a socket that no longer exists.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.wire.generation.load(Ordering::Relaxed)
    }

    /// A sender for whatever needs to talk to the daemon.
    #[must_use]
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// Queues a message. Returns whether the connection is up.
    pub fn send(&self, message: ClientMessage) -> bool {
        self.handle.send(message)
    }

    /// Whether the connection is up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.handle.is_connected()
    }

    /// Takes everything that has arrived since the last call.
    ///
    /// Never blocks: the interface calls this once a frame and draws whatever
    /// it got.
    pub fn poll(&self) -> Vec<ServerMessage> {
        let mut messages = Vec::new();
        while let Ok(message) = self.inbox.try_recv() {
            messages.push(message);
        }
        messages
    }
}

/// What one successful connection hands back.
type Connected = (Box<dyn Read + Send>, Box<dyn Write + Send>, String);

/// Connects and shakes hands, giving up if the peer does not answer in time.
///
/// The handshake runs on a thread of its own so a peer that accepts and then
/// says nothing costs one abandoned thread — which ends when that peer finally
/// closes — rather than the client's ability to connect at all.
fn connect_within(
    name: &str,
    role: Role,
    endpoint: &Path,
    patience: Duration,
) -> Result<Connected, ClientError> {
    let (done, answer) = channel();
    let name = name.to_string();
    let endpoint = endpoint.to_path_buf();

    std::thread::spawn(move || {
        let _ = done.send(connect(&name, role, &endpoint));
    });

    match answer.recv_timeout(patience) {
        Ok(result) => result,
        Err(_) => Err(ClientError::Handshake(format!(
            "the daemon did not answer within {patience:?}"
        ))),
    }
}

/// Connects and shakes hands, returning the two halves and the daemon's name.
fn connect(name: &str, role: Role, endpoint: &Path) -> Result<Connected, ClientError> {
    let connection = match Connection::connect_to(endpoint) {
        Ok(connection) => connection,
        Err(IpcError::NotRunning(_)) => {
            return Err(ClientError::NotRunning(endpoint.display().to_string()));
        }
        Err(error) => return Err(error.into()),
    };

    let (mut reader, mut writer) = connection.split();

    Frame::write(
        &mut writer,
        &ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: name.to_string(),
            role,
        },
    )
    .map_err(|e| ClientError::Handshake(e.to_string()))?;

    // Read the answer before starting any thread: a refused connection should
    // fail attaching rather than arrive later as a message the caller has to
    // know to look for.
    let device = match Frame::read::<_, ServerMessage>(&mut reader) {
        Ok(ServerMessage::Welcome { device, .. }) => device,
        Ok(ServerMessage::Error { error }) => return Err(ClientError::Refused(error)),
        Ok(other) => return Err(ClientError::Unexpected(format!("{other:?}"))),
        Err(error) => return Err(ClientError::Handshake(error.to_string())),
    };

    Ok((Box::new(reader), Box::new(writer), device))
}

/// Moves messages from the socket into the queue, until the socket ends.
fn read_from(
    mut reader: impl Read + Send + 'static,
    incoming: &Sender<ServerMessage>,
    wire: &Arc<Wire>,
) {
    let incoming = incoming.clone();
    let wire = Arc::clone(wire);

    std::thread::spawn(move || {
        loop {
            match Frame::read::<_, ServerMessage>(&mut reader) {
                Ok(message) => {
                    wire.heard();
                    if incoming.send(message).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    tracing::info!(%error, "the daemon connection ended");
                    wire.lost();
                    return;
                }
            }
        }
    });
}

/// Moves queued messages out to the socket, for the life of the client.
///
/// One thread across every connection: it writes through the wire, which the
/// supervisor swaps underneath it.
fn write_to(outgoing: Receiver<ClientMessage>, wire: &Arc<Wire>) {
    let wire = Arc::clone(wire);

    std::thread::spawn(move || {
        while let Ok(message) = outgoing.recv() {
            wire.write(&message);
        }
    });
}

/// Reconnects whenever the connection is down.
fn supervise(incoming: Sender<ServerMessage>, wire: &Arc<Wire>) {
    let wire = Arc::clone(wire);

    std::thread::spawn(move || {
        let mut backoff = FIRST_RETRY;

        loop {
            if wire.closed.load(Ordering::Relaxed) {
                return;
            }

            if wire.connected.load(Ordering::Relaxed) {
                check_liveness(&wire);
                std::thread::sleep(FIRST_RETRY);
                backoff = FIRST_RETRY;
                continue;
            }

            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(MAX_RETRY);

            match connect_within(&wire.name, wire.role, &wire.endpoint, HANDSHAKE_TIMEOUT) {
                Ok((reader, writer, device)) => {
                    *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = device;
                    *wire.writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(writer);
                    wire.generation.fetch_add(1, Ordering::Relaxed);
                    wire.heard();
                    wire.connected.store(true, Ordering::Relaxed);
                    read_from(reader, &incoming, &wire);

                    // Sent directly rather than through the queue: the queue's
                    // writer may be mid-message, and a subscribe that arrives
                    // after the first keystroke would lose the panes.
                    if wire.subscribed.load(Ordering::Relaxed) {
                        wire.write(&ClientMessage::Subscribe);
                    }

                    tracing::info!(
                        generation = wire.generation.load(Ordering::Relaxed),
                        "reconnected to the daemon"
                    );
                    backoff = FIRST_RETRY;
                }
                Err(error) => {
                    tracing::debug!(%error, "the daemon is not answering yet");
                }
            }
        }
    });
}

/// Asks a quiet daemon whether it is there, and gives up on one that never says.
fn check_liveness(wire: &Wire) {
    let quiet = wire.quiet_for();

    if quiet >= wire.liveness.silence {
        tracing::info!(?quiet, "the daemon stopped answering");
        wire.lost();
        return;
    }

    if quiet < wire.liveness.interval {
        return;
    }

    // One question per interval, not one per pass: the supervisor comes round
    // every hundred milliseconds, and a daemon that is merely busy should not be
    // buried in pings while it catches up.
    let mut asked = wire.last_asked.lock().unwrap_or_else(|e| e.into_inner());
    if asked.elapsed() < wire.liveness.interval {
        return;
    }
    *asked = Instant::now();
    drop(asked);

    // Written straight to the socket rather than queued: the queue carries the
    // interface's traffic, and a write that fails here is itself the answer,
    // because the connection is gone.
    wire.write(&ClientMessage::Ping {
        token: u64::try_from(quiet.as_millis()).unwrap_or(u64::MAX),
    });
}

/// Lets a test drive an interface's message path without a daemon.
impl Client {
    /// Creates a client with no socket behind it.
    ///
    /// The returned sender delivers what a daemon would have said, and the
    /// receiver collects what the client would have sent. No thread is started,
    /// so nothing reconnects and nothing is written anywhere: what is under
    /// test with one of these is what an interface *does* with the daemon's
    /// messages, which is otherwise only reachable by standing a real daemon on
    /// a real socket up around it.
    ///
    /// Reported as connected, because a client that says the daemon is gone
    /// would have every caller drawing a disconnect notice instead.
    #[doc(hidden)]
    #[must_use]
    pub fn for_test() -> (Self, Sender<ServerMessage>, Receiver<ClientMessage>) {
        let wire = Arc::new(Wire {
            writer: Mutex::new(None),
            connected: AtomicBool::new(true),
            generation: AtomicU64::new(1),
            subscribed: AtomicBool::new(false),
            closed: AtomicBool::new(true),
            device: Mutex::new("test-device".to_string()),
            name: "test".to_string(),
            role: Role::Interface,
            endpoint: PathBuf::new(),
            last_heard: Mutex::new(Instant::now()),
            last_asked: Mutex::new(Instant::now()),
            liveness: Liveness::default(),
        });

        let (outbox, outgoing) = channel::<ClientMessage>();
        let (incoming, inbox) = channel::<ServerMessage>();

        (
            Self {
                handle: Handle {
                    outbox,
                    wire: Arc::clone(&wire),
                },
                inbox,
                wire,
            },
            incoming,
            outgoing,
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Otherwise the supervisor would keep reconnecting to a daemon nobody
        // is listening to, for as long as the process lives.
        self.wire.closed.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
