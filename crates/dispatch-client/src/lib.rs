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

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dispatch_os::ipc::{Closer, Connection, IpcError, StderrHint};
use dispatch_proto::{ClientMessage, Frame, ProtocolError, Role, ServerMessage};

/// How a client reaches a daemon, and reaches it again after a drop.
///
/// Remembered rather than resolved once: a reconnection has to repeat the
/// original dial, and for a command transport that means respawning the
/// process. Without this a dropped connection over SSH could never come back.
#[derive(Debug, Clone)]
pub enum Dial {
    /// A socket on this machine.
    Endpoint(PathBuf),
    /// A command that speaks for a daemon on its own machine.
    Command {
        /// The program to run.
        program: OsString,
        /// Its arguments.
        args: Vec<OsString>,
    },
}

impl std::fmt::Display for Dial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Dial::Endpoint(path) => write!(f, "{}", path.display()),
            Dial::Command { program, args } => {
                write!(f, "{}", program.to_string_lossy())?;
                for arg in args {
                    write!(f, " {}", arg.to_string_lossy())?;
                }
                Ok(())
            }
        }
    }
}

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

/// How long to wait for a daemon on a socket to answer the handshake.
///
/// A peer that accepts a connection and then says nothing would otherwise stop
/// the client for good: the handshake is read before anything else happens, so
/// without a limit one unanswering socket ends both attaching and reconnecting.
///
/// Short because the peer is a daemon on this machine: it answers in
/// microseconds, or something is genuinely wrong with it.
const SOCKET_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long to wait for a daemon a command speaks for to answer the handshake.
///
/// Far longer than [`SOCKET_HANDSHAKE_TIMEOUT`], because it is not the same
/// question. That one is a socket on this machine. This one may be
/// `ssh host dispatchd --stdio`, and the budget has to cover a TCP handshake,
/// an authentication, a remote exec, and `--stdio`'s own connect -- which
/// cold-starts a daemon on the far side and waits up to ten seconds for it to
/// listen. A local socket's patience loses that race on every cold host and
/// every `ProxyJump`, and the reconnect would then repeat the loss forever.
const COMMAND_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long this dial is given to answer.
fn patience_for(dial: &Dial) -> Duration {
    match dial {
        Dial::Endpoint(_) => SOCKET_HANDSHAKE_TIMEOUT,
        Dial::Command { .. } => COMMAND_HANDSHAKE_TIMEOUT,
    }
}

/// The longest gap between reconnection attempts.
///
/// A daemon being restarted is back within a second or two, and a daemon that
/// is gone for good should not cost more than a connect attempt every couple of
/// seconds.
const MAX_RETRY: Duration = Duration::from_secs(2);

/// How long to wait before redialling a command the first time.
///
/// A second rather than [`FIRST_RETRY`]: every attempt is a new `ssh`, with a
/// TCP handshake and an authentication behind it, and a host that refused one
/// a tenth of a second ago will refuse the next.
const COMMAND_FIRST_RETRY: Duration = Duration::from_secs(1);

/// The longest gap between attempts to dial a command.
///
/// [`MAX_RETRY`] would respawn `ssh` against an asleep host thirty times a
/// minute for as long as it sleeps. With `ConnectTimeout=10` in front of it,
/// thirty seconds still has a machine that wakes joining within about forty.
const COMMAND_MAX_RETRY: Duration = Duration::from_secs(30);

/// The first gap between attempts and the longest, for this dial.
fn retry_for(dial: &Dial) -> (Duration, Duration) {
    match dial {
        Dial::Endpoint(_) => (FIRST_RETRY, MAX_RETRY),
        Dial::Command { .. } => (COMMAND_FIRST_RETRY, COMMAND_MAX_RETRY),
    }
}

/// The gap after `current`: doubled, up to this dial's ceiling.
fn next_backoff(current: Duration, dial: &Dial) -> Duration {
    (current * 2).min(retry_for(dial).1)
}

/// Where a dial leaves the means to end what it started, for whoever stops
/// waiting on it.
///
/// The handshake runs on a thread of its own, and a peer that accepts and
/// then never speaks leaves that thread parked in a read. Whoever gives up
/// on the handshake closes the connection through this: the parked thread
/// returns and, for a command, the process is ended -- rather than an `ssh`
/// left running, or a socket left open, for as long as the peer takes to
/// let go.
///
/// Recorded before the handshake begins, because the handshake is the part
/// that may never finish.
#[derive(Clone, Default)]
struct Dialling(Arc<Mutex<Option<Closer>>>);

impl Dialling {
    /// Remembers how to end what this dial opened.
    fn record(&self, closer: Closer) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(closer);
    }

    /// Ends whatever the dial started, if it started anything.
    fn abandon(&self) {
        let closer = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(closer) = closer {
            closer.close();
        }
    }
}

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

/// One connection: where to write, how to end it, and whether a write on it
/// is under way.
///
/// Held by `Arc`, so a write already under way on a connection that has
/// been replaced carries on against the old one -- and fails, once that is
/// closed -- without anything having to wait for it.
struct Line {
    /// Which connection this is.
    generation: u64,
    /// Where everything sent is written, one frame at a time.
    writer: Mutex<Box<dyn Write + Send>>,
    /// Ends both halves, and the process behind a command dial.
    closer: Closer,
    /// When the write under way began, while one is.
    ///
    /// A peer that talks but never reads is never silent, so this is what
    /// notices it: a write not finished within the silence the connection
    /// is allowed is not going to finish.
    writing_since: Mutex<Option<Instant>>,
}

impl Line {
    /// A connection with no write under way on it yet.
    fn new(generation: u64, writer: Box<dyn Write + Send>, closer: Closer) -> Self {
        Self {
            generation,
            writer: Mutex::new(writer),
            closer,
            writing_since: Mutex::new(None),
        }
    }

    /// How long the write under way has been going, if one is.
    fn stuck_for(&self) -> Option<Duration> {
        self.writing_since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|since| since.elapsed())
    }
}

/// The socket, and what is known about it.
///
/// Shared by the reader, the writer and the supervisor. The connection is
/// behind a lock and an `Option` because reconnecting replaces it: senders
/// keep the same [`Handle`] across a reconnection rather than being handed a
/// new one.
struct Wire {
    /// The connection now, if there is one.
    ///
    /// Only ever held for a moment, and never across a read or a write: so
    /// declaring a connection dead, or putting a new one in its place, never
    /// waits on a write that is stuck.
    line: Mutex<Option<Arc<Line>>>,
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
    /// How the daemon was first reached, which every reconnection repeats.
    dial: Dial,
    /// When anything last arrived, for deciding a silent socket is dead.
    last_heard: Mutex<Instant>,
    /// When the last question was asked, so one goes out per interval rather
    /// than on every pass of the supervisor.
    last_asked: Mutex<Instant>,
    /// How patient to be with silence.
    liveness: Liveness,
    /// Why the last dial failed, until one succeeds.
    ///
    /// Kept for the interface rather than only logged: a machine that stays
    /// down has to be able to say `Permission denied (publickey)` somewhere
    /// the user is looking.
    last_error: Mutex<Option<String>>,
}

impl Wire {
    /// A wire with nothing on it yet: down, at generation 0, and nameless.
    fn new(role: Role, name: &str, liveness: Liveness, dial: Dial) -> Self {
        Self {
            line: Mutex::new(None),
            connected: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            subscribed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            device: Mutex::new(String::new()),
            name: name.to_string(),
            role,
            dial,
            last_heard: Mutex::new(Instant::now()),
            last_asked: Mutex::new(Instant::now()),
            liveness,
            last_error: Mutex::new(None),
        }
    }

    /// The connection now, if there is one.
    fn current(&self) -> Option<Arc<Line>> {
        self.line.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Writes one message, or reports that the connection is gone.
    fn write(&self, message: &ClientMessage) -> bool {
        let Some(line) = self.current() else {
            // Disconnected: dropped rather than queued. A keystroke that
            // arrives at an agent minutes later, out of order with the rest,
            // is worse than one that never arrives.
            return false;
        };

        let mut writer = line.writer.lock().unwrap_or_else(|e| e.into_inner());
        self.write_on(&line, &mut writer, message)
    }

    /// Writes one message, unless a write is already under way.
    ///
    /// For the supervisor's ping, which must never wait behind a write that
    /// may be stuck: the write deadline deals with that one.
    fn try_write(&self, message: &ClientMessage) {
        let Some(line) = self.current() else { return };

        let mut writer = match line.writer.try_lock() {
            Ok(writer) => writer,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        self.write_on(&line, &mut writer, message);
    }

    /// Writes `message` to `line`, timing the write.
    fn write_on(
        &self,
        line: &Line,
        writer: &mut Box<dyn Write + Send>,
        message: &ClientMessage,
    ) -> bool {
        *line.writing_since.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let written = Frame::write(writer, message);
        *line.writing_since.lock().unwrap_or_else(|e| e.into_inner()) = None;

        if written.is_err() {
            // The writer going is only half of it: a command whose stdin
            // closes need not exit, and `ssh` does not. `lost` ends the lot.
            self.lost(line.generation);
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

    /// Records that connection `generation` has broken, and ends it.
    ///
    /// A connection already replaced is left alone: a reader parked on a
    /// peer liveness gave up on wakes only when that peer finally closes, and
    /// a write stuck on it fails only once it is closed -- by which time the
    /// supervisor may have dialled a replacement, which "whatever is current"
    /// would tear down. Compared under the lock the supervisor installs a
    /// connection under, so the check cannot fall between a replacement being
    /// put in place and its generation being counted.
    ///
    /// Marked down under that lock too, so `connected` never disagrees with
    /// whether there is a line. Closed after, outside it: ending a command's
    /// process tree can take a moment, and nothing else should wait for it.
    fn lost(&self, generation: u64) {
        let line = {
            let mut slot = self.line.lock().unwrap_or_else(|e| e.into_inner());
            if slot
                .as_ref()
                .is_none_or(|line| line.generation != generation)
            {
                return;
            }
            self.connected.store(false, Ordering::Relaxed);
            slot.take()
        };

        if let Some(line) = line {
            line.closer.close();
        }
    }

    /// Records that the current connection has broken.
    ///
    /// For callers that are about the connection as it stands rather than
    /// one they were handed: the supervisor, and a queue whose writer thread
    /// has gone.
    fn lost_current(&self) {
        if let Some(line) = self.current() {
            self.lost(line.generation);
        }
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
            self.wire.lost_current();
            return false;
        }

        self.is_connected()
    }

    /// Whether the connection is up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.wire.connected.load(Ordering::Relaxed)
    }

    /// Counts a reconnection that no socket performed.
    ///
    /// The companion to [`Client::for_test`]: a real reconnection is a
    /// supervisor thread and a daemon restarting, so what an interface *does*
    /// with one — rebuilding that machine's rows, asking again for its roots —
    /// is otherwise only reachable by standing a real daemon up around it and
    /// killing it.
    #[doc(hidden)]
    pub fn reconnect_for_test(&self) {
        self.wire.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Changes what `Client::device` reports, as if the daemon on the other
    /// end had come back under a different name.
    ///
    /// The companion to [`Handle::reconnect_for_test`]: a real reconnection's
    /// handshake can rename the device in the same beat that bumps the
    /// generation (see `supervise`), and covering an interface's reaction to
    /// that rename is otherwise only reachable by restarting a real daemon
    /// under a different `--device`.
    #[doc(hidden)]
    pub fn rename_for_test(&self, name: &str) {
        *self.wire.device.lock().unwrap_or_else(|e| e.into_inner()) = name.to_string();
    }

    /// Connects as a dial would, with no dial behind it.
    ///
    /// The companion to [`Client::pending_for_test`]: renames the device,
    /// bumps the generation and clears any failure, in the order the
    /// supervisor does.
    #[doc(hidden)]
    pub fn connect_for_test(&self, device: &str) {
        self.rename_for_test(device);
        *self
            .wire
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.wire.generation.fetch_add(1, Ordering::Relaxed);
        self.wire.connected.store(true, Ordering::Relaxed);
    }

    /// Fails as a dial would, with no dial behind it.
    #[doc(hidden)]
    pub fn fail_for_test(&self, error: &str) {
        self.wire.connected.store(false, Ordering::Relaxed);
        *self
            .wire
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
    }
}

/// A connection to one daemon: attached, or — made by [`Client::dial`] — still
/// dialling one that has not answered yet.
///
/// One type for both because a caller treats them alike: a client that has
/// never connected is one whose connection is down, and its first connection
/// is a generation change like any reconnection.
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
    ///
    /// Resolves the endpoint from this process's own configuration and
    /// delegates: everyone who does not need to name a daemon explicitly gets
    /// the behaviour they always had.
    pub fn attach_with_as(role: Role, name: &str, liveness: Liveness) -> Result<Self, ClientError> {
        let endpoint = dispatch_os::ipc::endpoint()?;
        Self::attach_at(role, name, liveness, endpoint)
    }

    /// Connects and shakes hands with the daemon listening on `endpoint`,
    /// rather than the one this configuration would otherwise resolve to.
    ///
    /// Federation holds one connection per machine, so the endpoint has to be
    /// the caller's to name: a client attaching to three daemons cannot take
    /// all three from the one endpoint this process's own configuration
    /// resolves to.
    pub fn attach_at(
        role: Role,
        name: &str,
        liveness: Liveness,
        endpoint: PathBuf,
    ) -> Result<Self, ClientError> {
        Self::attach_dialling(role, name, liveness, Dial::Endpoint(endpoint))
    }

    /// Connects and shakes hands with the daemon a command speaks for.
    ///
    /// The command is respawned on every reconnection, so it has to be one
    /// that can be run again — `ssh host dispatchd --stdio` is the case this
    /// exists for.
    pub fn attach_over(
        role: Role,
        name: &str,
        liveness: Liveness,
        program: OsString,
        args: Vec<OsString>,
    ) -> Result<Self, ClientError> {
        Self::attach_dialling(role, name, liveness, Dial::Command { program, args })
    }

    /// Connects and shakes hands by whichever dial was given.
    ///
    /// The one place that turns a [`Dial`] into a live connection and the
    /// state that supervises it: `attach_at` and `attach_over` differ only in
    /// which [`Dial`] they build.
    fn attach_dialling(
        role: Role,
        name: &str,
        liveness: Liveness,
        dial: Dial,
    ) -> Result<Self, ClientError> {
        let connected = connect_within(name, role, &dial, patience_for(&dial))?;

        let wire = Wire::new(role, name, liveness, dial);
        *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = connected.device;
        *wire.line.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Arc::new(Line::new(1, connected.writer, connected.closer)));
        wire.generation.store(1, Ordering::Relaxed);
        wire.connected.store(true, Ordering::Relaxed);

        Ok(Self::start(Arc::new(wire), Some(connected.reader)))
    }

    /// Dials in the background, and keeps dialling until it connects.
    ///
    /// Returns at once, down, at generation 0 and with an empty
    /// [`Client::device`]. The first connection is generation 1, so a caller
    /// that already rebuilds on a generation change handles a first connect
    /// with no code of its own. Messages sent before then are dropped, as
    /// they are during any outage; [`Client::subscribe`] is remembered and
    /// sent on connecting.
    ///
    /// For a machine that may be asleep: [`Client::attach_over`] would have
    /// nothing to hand back, and so nothing that could try again.
    #[must_use]
    pub fn dial(role: Role, name: &str, liveness: Liveness, dial: Dial) -> Self {
        Self::start(Arc::new(Wire::new(role, name, liveness, dial)), None)
    }

    /// Starts the threads that serve a wire, reading from `reader` when a
    /// connection is already up.
    fn start(wire: Arc<Wire>, reader: Option<Box<dyn Read + Send>>) -> Self {
        let (outbox, outgoing) = channel::<ClientMessage>();
        let (incoming, inbox) = channel::<ServerMessage>();

        if let Some(reader) = reader {
            let generation = wire.generation.load(Ordering::Relaxed);
            read_from(reader, generation, &incoming, &wire);
        }
        write_to(outgoing, &wire);
        supervise(incoming, &wire);

        Self {
            handle: Handle {
                outbox,
                wire: Arc::clone(&wire),
            },
            inbox,
            wire,
        }
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

    /// Which connection this is, counting from one; 0 means it has never
    /// connected.
    ///
    /// A caller that has built state from the daemon's messages compares this
    /// against what it built: a higher number means a different connection, and
    /// everything it was told belongs to a socket that no longer exists. A
    /// client from [`Client::dial`] starts at 0, so its first connection is
    /// the same kind of change as any later one.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.wire.generation.load(Ordering::Relaxed)
    }

    /// Why the last attempt to connect failed, while none has succeeded
    /// since.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.wire
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// How this client reaches its daemon, as one line.
    ///
    /// What a machine with no other name is called until its daemon answers.
    #[must_use]
    pub fn dialled(&self) -> String {
        self.wire.dial.to_string()
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
struct Connected {
    /// The half everything arriving is read from.
    reader: Box<dyn Read + Send>,
    /// The half everything sent is written to.
    writer: Box<dyn Write + Send>,
    /// What the daemon calls itself.
    device: String,
    /// Ends this connection, both halves and any process behind it.
    closer: Closer,
}

/// Connects and shakes hands, giving up if the peer does not answer in time.
///
/// The handshake runs on a thread of its own so a peer that accepts and then
/// says nothing costs one thread for as long as the handshake is given --
/// rather than the client's ability to connect at all.
///
/// Abandoning the thread is not abandoning what it started: a dial records
/// how to end its connection before shaking hands, and giving up on the
/// handshake closes what the dial opened -- the socket, or the command's
/// whole process tree. Without that, the thread stays blocked in the read
/// holding both halves until the peer lets go, so a command's stdin is never
/// even closed -- and the supervisor, which repeats the dial every couple of
/// seconds, would leave one more behind each time.
fn connect_within(
    name: &str,
    role: Role,
    dial: &Dial,
    patience: Duration,
) -> Result<Connected, ClientError> {
    let (done, answer) = channel();
    let name = name.to_string();
    let for_thread = dial.clone();
    let dialling = Dialling::default();
    let recording = dialling.clone();

    std::thread::spawn(move || {
        let _ = done.send(connect(&name, role, &for_thread, &recording));
    });

    match answer.recv_timeout(patience) {
        Ok(result) => result,
        // Raised here rather than by the thread: the thread may still be
        // waiting on a peer that never answers, so the timeout has to come
        // from the caller's side and cannot carry a stderr hint that only the
        // thread holds.
        Err(_) => {
            dialling.abandon();
            Err(ClientError::Handshake(format!(
                "{dial} did not answer within {patience:?}"
            )))
        }
    }
}

/// Connects and shakes hands, returning the two halves and the daemon's name.
///
/// `started` is filled in before the handshake, so a caller that stops waiting
/// still has something to close; see [`Dialling`].
fn connect(
    name: &str,
    role: Role,
    dial: &Dial,
    started: &Dialling,
) -> Result<Connected, ClientError> {
    let (connection, hint) = match dial {
        Dial::Endpoint(endpoint) => match Connection::connect_to(endpoint) {
            Ok(connection) => (connection, None),
            Err(IpcError::NotRunning(_)) => {
                return Err(ClientError::NotRunning(endpoint.display().to_string()));
            }
            Err(error) => return Err(error.into()),
        },
        Dial::Command { program, args } => {
            let connection = Connection::over_command(program, args)?;
            let hint = connection.hint();
            (connection, Some(hint))
        }
    };

    let closer = connection.closer();
    started.record(closer.clone());

    let (mut reader, mut writer) = connection.split();

    Frame::write(
        &mut writer,
        &ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: name.to_string(),
            role,
        },
    )
    .map_err(|e| with_hint(ClientError::Handshake(e.to_string()), hint.as_ref()))?;

    // Read the answer before starting any thread: a refused connection should
    // fail attaching rather than arrive later as a message the caller has to
    // know to look for.
    let device = match Frame::read::<_, ServerMessage>(&mut reader) {
        Ok(ServerMessage::Welcome { version, device }) => {
            // Checked here as the daemon checks ours: a major version apart,
            // every frame after this one could mean something else.
            if !dispatch_proto::VERSION.is_compatible_with(version) {
                return Err(ClientError::Refused(ProtocolError::IncompatibleVersion {
                    peer: version,
                    ours: dispatch_proto::VERSION,
                }));
            }
            device
        }
        Ok(ServerMessage::Error { error }) => {
            return Err(with_hint(ClientError::Refused(error), hint.as_ref()));
        }
        Ok(other) => return Err(ClientError::Unexpected(format!("{other:?}"))),
        Err(error) => {
            return Err(with_hint(
                ClientError::Handshake(error.to_string()),
                hint.as_ref(),
            ));
        }
    };

    Ok(Connected {
        reader,
        writer,
        device,
        closer,
    })
}

/// How long to wait for a hint that has not arrived yet.
///
/// Bounded because a hint is worth a moment and never a hang: the caller is
/// already holding a failure to report, and a command that says nothing more
/// must not turn that failure into a wait.
///
/// A quarter of a second rather than the fifty milliseconds it once was. A
/// dying command's last words cross a pipe and a drain thread of their own,
/// and on a loaded machine that took longer than fifty often enough to lose
/// them -- `Permission denied (publickey)` reported as bare silence. A
/// failing attach now reports a quarter second later, and loses its
/// explanation far less often: of the two, the explanation is what the user
/// cannot get back.
const HINT_PATIENCE: Duration = Duration::from_millis(250);

/// How often to look while waiting for one.
const HINT_POLL: Duration = Duration::from_millis(5);

/// The command's own words, folded into a failure that would otherwise read
/// as silence.
///
/// A command transport's real reason lives on its stderr: an SSH key refused,
/// a binary missing on the far side. Neither reaches the protocol, so neither
/// reaches the caller unless it is carried here.
///
/// Waited for, briefly, rather than read once: stderr is drained on a thread
/// of its own, so a command that dies the instant it starts can fail the
/// handshake while its own explanation is still in flight. Reading
/// immediately makes reporting `ssh: Permission denied (publickey)` instead
/// of nothing at all a race — one the caller loses exactly when the command
/// failed fastest.
fn with_hint(error: ClientError, hint: Option<&StderrHint>) -> ClientError {
    let Some(hint) = hint else { return error };

    let deadline = Instant::now() + HINT_PATIENCE;
    loop {
        if let Some(line) = hint.first_line() {
            return ClientError::Handshake(format!("{error}: {line}"));
        }
        if Instant::now() >= deadline {
            return error;
        }
        std::thread::sleep(HINT_POLL);
    }
}

/// Moves messages from the socket into the queue, until the socket ends.
///
/// `generation` is the connection this reader belongs to, so that its
/// ending is reported against that connection and not whichever one has
/// replaced it by then.
fn read_from(
    mut reader: impl Read + Send + 'static,
    generation: u64,
    incoming: &Sender<ServerMessage>,
    wire: &Arc<Wire>,
) {
    let incoming = incoming.clone();
    let wire = Arc::clone(wire);

    std::thread::spawn(move || {
        loop {
            match Frame::read::<_, ServerMessage>(&mut reader) {
                Ok(message) => {
                    // A connection that has been replaced says nothing more:
                    // its peer was given up on, and whatever it says now
                    // describes a connection the interface has already
                    // rebuilt from its successor's replay. Nor does it count
                    // as hearing from the daemon, which would keep a dead
                    // successor looking alive. Returning drops the reader,
                    // and with it anything it owns.
                    if wire.generation.load(Ordering::Relaxed) != generation {
                        return;
                    }

                    wire.heard();
                    if incoming.send(message).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    tracing::info!(%error, generation, "the daemon connection ended");
                    wire.lost(generation);
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

/// Reconnects whenever the connection is down — and, for a client made by
/// [`Client::dial`], connects in the first place.
fn supervise(incoming: Sender<ServerMessage>, wire: &Arc<Wire>) {
    let wire = Arc::clone(wire);

    std::thread::spawn(move || {
        let first_retry = retry_for(&wire.dial).0;
        let mut backoff = first_retry;
        // A client that has never connected dials at once: there is no
        // failure yet to back off from.
        let mut dial_now = wire.generation.load(Ordering::Relaxed) == 0;

        loop {
            if wire.closed.load(Ordering::Relaxed) {
                return;
            }

            if wire.connected.load(Ordering::Relaxed) {
                check_liveness(&wire);
                std::thread::sleep(FIRST_RETRY);
                backoff = first_retry;
                continue;
            }

            if !std::mem::take(&mut dial_now) {
                std::thread::sleep(backoff);
                backoff = next_backoff(backoff, &wire.dial);

                // Checked again after the sleep, not only at the top: a
                // client dropped during a thirty-second backoff would
                // otherwise run ssh once more for nobody.
                if wire.closed.load(Ordering::Relaxed) {
                    return;
                }
            }

            let patience = patience_for(&wire.dial);
            match connect_within(&wire.name, wire.role, &wire.dial, patience) {
                Ok(mut connected) => {
                    // Asked for before the line is put in place, rather than
                    // through the queue or once it is up: the queue's writer
                    // may be mid-message, and a subscribe that arrives after
                    // the first keystroke would lose the panes -- and once
                    // the line is up that writer may be stuck on it, and the
                    // supervisor must never wait behind a write. Until then
                    // nothing else can reach this writer, and a few bytes
                    // into a fresh connection do not block. One that cannot
                    // take them is already broken, and its next write or its
                    // reader will say so.
                    if wire.subscribed.load(Ordering::Relaxed) {
                        let _ = Frame::write(&mut connected.writer, &ClientMessage::Subscribe);
                    }

                    // Checked under the lock `Client::drop` takes to clear the
                    // line, so whichever of the two gets there first, the
                    // connection is closed exactly once. Device, line,
                    // generation and `connected` all change together under
                    // it: an interface that sees the generation move sees
                    // the device that came with it, and `lost` never finds a
                    // line without its generation counted.
                    let generation = {
                        let mut slot = wire.line.lock().unwrap_or_else(|e| e.into_inner());
                        if wire.closed.load(Ordering::Relaxed) {
                            drop(slot);
                            connected.closer.close();
                            return;
                        }

                        *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = connected.device;
                        let generation = wire.generation.fetch_add(1, Ordering::Relaxed) + 1;
                        *slot = Some(Arc::new(Line::new(
                            generation,
                            connected.writer,
                            connected.closer,
                        )));
                        wire.heard();
                        wire.connected.store(true, Ordering::Relaxed);
                        generation
                    };

                    *wire.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    read_from(connected.reader, generation, &incoming, &wire);

                    tracing::info!(generation, dial = %wire.dial, "connected to the daemon");
                    backoff = first_retry;
                }
                Err(error) => {
                    tracing::debug!(%error, dial = %wire.dial, "the daemon is not answering yet");
                    *wire.last_error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(error.to_string());
                }
            }
        }
    });
}

/// Asks a quiet daemon whether it is there, and gives up on one that never
/// says -- or on one a write has been stuck on for as long as silence is
/// allowed.
fn check_liveness(wire: &Wire) {
    // First, because a peer that talks but never reads is never quiet: its
    // chatter would pass every check below while nothing sent reaches it.
    if let Some(line) = wire.current()
        && line
            .stuck_for()
            .is_some_and(|stuck| stuck >= wire.liveness.silence)
    {
        tracing::info!("a write to the daemon never finished");
        wire.lost(line.generation);
        return;
    }

    let quiet = wire.quiet_for();

    if quiet >= wire.liveness.silence {
        tracing::info!(?quiet, "the daemon stopped answering");
        wire.lost_current();
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

    // Written straight to the socket rather than queued -- the queue carries
    // the interface's traffic -- but only if no write is under way: a ping
    // that waited behind a stuck write would stall the supervisor with it.
    wire.try_write(&ClientMessage::Ping {
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
        let wire = Wire::new(
            Role::Interface,
            "test",
            Liveness::default(),
            Dial::Endpoint(PathBuf::new()),
        );
        wire.connected.store(true, Ordering::Relaxed);
        wire.generation.store(1, Ordering::Relaxed);
        *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = "test-device".to_string();
        Self::without_threads(wire)
    }

    /// Creates a client with no socket behind it that has never connected.
    ///
    /// The companion to [`Client::for_test`] for what [`Client::dial`] hands
    /// back: down, at generation 0, nameless, and dialled as `test-dial`.
    /// Drive it with [`Handle::connect_for_test`] and
    /// [`Handle::fail_for_test`].
    #[doc(hidden)]
    #[must_use]
    pub fn pending_for_test() -> (Self, Sender<ServerMessage>, Receiver<ClientMessage>) {
        let dial = Dial::Command {
            program: "test-dial".into(),
            args: Vec::new(),
        };
        Self::without_threads(Wire::new(
            Role::Interface,
            "test",
            Liveness::default(),
            dial,
        ))
    }

    /// Wraps a wire in a client whose traffic the test holds both ends of.
    fn without_threads(wire: Wire) -> (Self, Sender<ServerMessage>, Receiver<ClientMessage>) {
        // No supervisor runs, so nothing may try to: a dropped test client
        // must not start dialling an empty endpoint.
        wire.closed.store(true, Ordering::Relaxed);
        let wire = Arc::new(wire);

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

        // And the connection -- a socket, or a command's whole process tree
        // -- would outlive the client that wanted it: the reader that owns it
        // is parked, and nothing else is ever going to wake it.
        let line = self
            .wire
            .line
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(line) = line {
            self.wire.connected.store(false, Ordering::Relaxed);
            line.closer.close();
        }
    }
}

#[cfg(test)]
mod tests;
