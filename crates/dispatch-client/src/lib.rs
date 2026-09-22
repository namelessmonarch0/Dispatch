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

use dispatch_os::ipc::{Connection, IpcError, StderrHint};
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

/// How long a dialled process tree is given to exit before it is killed
/// outright.
///
/// Shorter than [`process::DEFAULT_GRACE`](dispatch_os::process::DEFAULT_GRACE):
/// that grace waits on an agent being asked to close a pane, this one on a
/// transport that has already been replaced or given up on, and nothing --
/// least of all a reconnection -- should wait on it.
const DIAL_TEARDOWN_GRACE: Duration = Duration::from_millis(50);

/// The longest gap between reconnection attempts.
///
/// A daemon being restarted is back within a second or two, and a daemon that
/// is gone for good should not cost more than a connect attempt every couple of
/// seconds.
const MAX_RETRY: Duration = Duration::from_secs(2);

/// Where a dial leaves its child's pid, for whoever may have to kill it.
///
/// [`Connection::split`] hands the process to the reader half, so nothing but
/// that reader being dropped reaps it -- and the reader is exactly what stays
/// blocked when a peer accepts and then never speaks. Two callers walk away
/// from a reader in that state: a handshake that times out, and a connection
/// liveness has declared dead. Both would leave an `ssh` running for as long
/// as the kernel takes to give up on it, while the supervisor dials another.
///
/// Recorded before the handshake begins rather than after it succeeds: the
/// handshake is the part that may never finish.
#[derive(Clone, Default)]
struct DialledChild(Arc<Mutex<Option<u32>>>);

impl DialledChild {
    /// Remembers the process this dial started, if it started one.
    fn record(&self, pid: Option<u32>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = pid;
    }

    /// Takes the pid, leaving nothing behind.
    ///
    /// Taking rather than reading: whoever takes it owns the killing, and a
    /// pid killed twice could by then belong to somebody else's process.
    fn take(&self) -> Option<u32> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Kills what a command dial started, if it started anything.
///
/// A socket dial has no process of its own, which is why this takes an
/// `Option` rather than making every caller ask first.
fn reap_dialled(pid: Option<u32>) {
    let Some(pid) = pid else { return };

    if let Err(error) = dispatch_os::process::terminate_tree(pid, DIAL_TEARDOWN_GRACE) {
        tracing::warn!(%error, pid, "failed to stop the process behind a dial");
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
    /// How the daemon was first reached, which every reconnection repeats.
    dial: Dial,
    /// When anything last arrived, for deciding a silent socket is dead.
    last_heard: Mutex<Instant>,
    /// When the last question was asked, so one goes out per interval rather
    /// than on every pass of the supervisor.
    last_asked: Mutex<Instant>,
    /// How patient to be with silence.
    liveness: Liveness,
    /// The process behind the current connection, when the dial is a command.
    child: DialledChild,
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
            let child = self.child.take();
            *guard = None;
            self.connected.store(false, Ordering::Relaxed);
            drop(guard);
            // The writer going is only half of it: a command whose stdin
            // closes need not exit, and `ssh` does not.
            reap_dialled(child);
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

    /// Records that the connection has broken, and ends the process behind it.
    ///
    /// Dropping the writer closes a command's stdin, which is not enough:
    /// `ssh` does not exit on it, and the reader that owns the process is
    /// parked on a peer that has stopped speaking -- the very case liveness
    /// declares dead. Left alone it would sit there for the kernel's own TCP
    /// timeout, a quarter of an hour, while the supervisor dialled a second
    /// one beside it.
    ///
    /// The pid is taken *before* the connection is marked down, because down
    /// is what lets the supervisor dial again: taking second could hand this
    /// call the fresh connection's child and kill that instead.
    fn lost(&self) {
        let child = self.child.take();
        *self.writer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.connected.store(false, Ordering::Relaxed);
        reap_dialled(child);
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

        let child = DialledChild::default();
        child.record(connected.child);

        let wire = Arc::new(Wire {
            writer: Mutex::new(Some(connected.writer)),
            connected: AtomicBool::new(true),
            generation: AtomicU64::new(1),
            subscribed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            device: Mutex::new(connected.device),
            name: name.to_string(),
            role,
            dial,
            last_heard: Mutex::new(Instant::now()),
            last_asked: Mutex::new(Instant::now()),
            liveness,
            child,
        });

        let (outbox, outgoing) = channel::<ClientMessage>();
        let (incoming, inbox) = channel::<ServerMessage>();

        read_from(connected.reader, &incoming, &wire);
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
struct Connected {
    /// The half everything arriving is read from.
    reader: Box<dyn Read + Send>,
    /// The half everything sent is written to.
    writer: Box<dyn Write + Send>,
    /// What the daemon calls itself.
    device: String,
    /// The process behind a command dial, so whatever supervises this
    /// connection can end it without waiting on the reader that owns it.
    child: Option<u32>,
}

/// Connects and shakes hands, giving up if the peer does not answer in time.
///
/// The handshake runs on a thread of its own so a peer that accepts and then
/// says nothing costs one abandoned thread — which ends when that peer finally
/// closes — rather than the client's ability to connect at all.
///
/// Abandoning the thread is not abandoning what it started: a command dial
/// records its process before shaking hands, and giving up on the handshake
/// kills it. Without that, the thread stays blocked in the read forever
/// holding both halves, so the command's stdin is never even closed — and the
/// supervisor, which repeats the dial every couple of seconds, would start a
/// fresh one each time.
fn connect_within(
    name: &str,
    role: Role,
    dial: &Dial,
    patience: Duration,
) -> Result<Connected, ClientError> {
    let (done, answer) = channel();
    let name = name.to_string();
    let for_thread = dial.clone();
    let started = DialledChild::default();
    let recording = started.clone();

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
            reap_dialled(started.take());
            Err(ClientError::Handshake(format!(
                "{dial} did not answer within {patience:?}"
            )))
        }
    }
}

/// Connects and shakes hands, returning the two halves and the daemon's name.
///
/// `started` is filled in before the handshake, so a caller that stops waiting
/// still has something to kill; see [`DialledChild`].
fn connect(
    name: &str,
    role: Role,
    dial: &Dial,
    started: &DialledChild,
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

    let child = connection.child_id();
    started.record(child);

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
        Ok(ServerMessage::Welcome { device, .. }) => device,
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
        child,
    })
}

/// How long to wait for a hint that has not arrived yet.
///
/// Bounded because a hint is worth a moment and never a hang: the caller is
/// already holding a failure to report, and a command that says nothing more
/// must not turn that failure into a wait.
const HINT_PATIENCE: Duration = Duration::from_millis(50);

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

            let patience = patience_for(&wire.dial);
            match connect_within(&wire.name, wire.role, &wire.dial, patience) {
                Ok(connected) => {
                    *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = connected.device;
                    *wire.writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(connected.writer);
                    wire.child.record(connected.child);
                    wire.generation.fetch_add(1, Ordering::Relaxed);
                    wire.heard();
                    wire.connected.store(true, Ordering::Relaxed);
                    read_from(connected.reader, &incoming, &wire);

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
            dial: Dial::Endpoint(PathBuf::new()),
            last_heard: Mutex::new(Instant::now()),
            last_asked: Mutex::new(Instant::now()),
            liveness: Liveness::default(),
            child: DialledChild::default(),
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

        // And the command the last dial started would outlive the client that
        // wanted it: the reader that owns it is parked, and nothing else is
        // ever going to wake it.
        reap_dialled(self.wire.child.take());
    }
}

#[cfg(test)]
mod tests;
