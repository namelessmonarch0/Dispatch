//! Local transport between a Dispatch client and `dispatchd`.
//!
//! A Unix domain socket on POSIX, a named pipe on Windows. Both are
//! local-only and carry the operating system's own access control, which is
//! what keeps another user off a daemon that can run arbitrary commands.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod pairing;

/// Failures opening or accepting a connection.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The endpoint could not be determined.
    #[error(transparent)]
    Path(#[from] crate::paths::PathError),

    /// The endpoint is already in use by a running daemon.
    #[error("a daemon is already listening on {0}")]
    AlreadyRunning(String),

    /// No daemon is listening.
    #[error("no daemon is listening on {0}")]
    NotRunning(String),

    /// The underlying transport failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being attempted.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },

    /// A command that was supposed to speak for a daemon could not be started.
    #[error("cannot run {command}: {source}")]
    Spawn {
        /// The command line, for a message that says what was looked for.
        command: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl IpcError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Where the daemon listens.
///
/// Under the configuration directory, so `DISPATCH_CONFIG_DIR` gives a
/// separate daemon its own endpoint and two configurations cannot collide.
pub fn endpoint() -> Result<PathBuf, IpcError> {
    Ok(crate::paths::config_dir()?.join("dispatchd.sock"))
}

/// How long a connection may take to say which half it is.
///
/// A client writes its thirteen bytes the moment it connects. One that has
/// not in two seconds is not a Dispatch client, or not a working one.
const PREAMBLE_PATIENCE: Duration = Duration::from_secs(2);

/// How many connections may be announcing themselves at once.
///
/// Each holds a thread until it has said which half it is or run out of
/// patience. Past this a new connection is closed at once, so a flood of
/// silent connections costs this many threads for two seconds rather than
/// one thread each.
const MAX_ANNOUNCING: usize = 32;

/// Ends a connection from outside the threads using it.
///
/// A thread parked in a read or a write on a peer that has stopped answering
/// holds its half until the peer lets go -- for a socket under a dead SSH
/// session, the kernel's quarter of an hour. Nothing outside that thread can
/// drop the half, so this is the way in: it fails what is in flight on both
/// halves, the parked threads return with an error, and each lets its half
/// go.
///
/// On Unix both sockets are shut down, which also fails everything after.
/// On Windows the pipe operations in flight are cancelled, and the pipe ends
/// once the threads parked in them drop their halves -- which they do when
/// their operation fails. A command transport's process tree is killed,
/// which ends its pipes from the far side.
///
/// Cheap to clone; every clone ends the same connection, and closing twice
/// does nothing. `Closer::default()` closes nothing: it is what a
/// connection built from halves the caller already owns hands out.
#[derive(Clone, Default)]
pub struct Closer(Arc<Mutex<Option<Ending>>>);

/// What ending one connection takes.
enum Ending {
    /// A second handle onto each of the connection's two streams.
    Streams(Vec<imp::Stream>),
    /// The process behind a command transport.
    Process(u32),
}

impl std::fmt::Debug for Closer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Closer")
    }
}

impl Closer {
    /// A closer for the connection paired from `reader` and `writer`.
    fn streams(reader: &imp::Stream, writer: &imp::Stream) -> Result<Self, IpcError> {
        let second = |stream: &imp::Stream| {
            imp::try_clone(stream)
                .map_err(|e| IpcError::io("preparing a connection to be closed", e))
        };
        Ok(Self::ending(Ending::Streams(vec![
            second(reader)?,
            second(writer)?,
        ])))
    }

    /// A closer for the command transport running as `pid`.
    fn process(pid: u32) -> Self {
        Self::ending(Ending::Process(pid))
    }

    fn ending(ending: Ending) -> Self {
        Self(Arc::new(Mutex::new(Some(ending))))
    }

    /// Makes both halves of the connection fail, whoever holds them.
    pub fn close(&self) {
        let ending = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();

        match ending {
            None => {}
            Some(Ending::Streams(streams)) => {
                for stream in &streams {
                    imp::interrupt(stream);
                }
            }
            Some(Ending::Process(pid)) => {
                if let Err(error) = crate::process::terminate_tree(pid, TEARDOWN_GRACE) {
                    tracing::debug!(%error, pid, "failed to end a command transport");
                }
            }
        }
    }
}

/// A connected client or server end.
///
/// Two transport connections, one per direction, paired by [`pairing`] when
/// the transport is a socket. They are separate so that a thread parked
/// reading cannot hold up a thread writing -- which one connection cannot
/// promise on Windows.
///
/// Boxed rather than naming `imp::Stream`: a later transport builds a
/// connection from a child process's stdout and stdin, which are a different
/// concrete type on every platform this already varies by, and the two must
/// still fit in one field each.
pub struct Connection {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    /// The process behind a command transport, kept so it can be reaped.
    child: Option<std::process::Child>,
    hint: StderrHint,
    /// Ends this connection from outside; see [`Closer`].
    closer: Closer,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Connection")
    }
}

impl Connection {
    /// Connects to the daemon for the current configuration.
    pub fn connect() -> Result<Self, IpcError> {
        Self::connect_to(&endpoint()?)
    }

    /// Connects to the daemon listening on `endpoint`.
    ///
    /// A client that reconnects uses this with the endpoint it first reached, so
    /// a configuration change mid-session cannot silently move it to a different
    /// daemon than the one its panes are on.
    pub fn connect_to(endpoint: &Path) -> Result<Self, IpcError> {
        let (reader, writer) = pairing::dial(|| imp::connect(endpoint))?;
        Self::over_streams(reader, writer)
    }

    /// A connection over the two streams a dial or a listener paired.
    fn over_streams(reader: imp::Stream, writer: imp::Stream) -> Result<Self, IpcError> {
        let closer = Closer::streams(&reader, &writer)?;
        Ok(Self {
            reader: Box::new(reader),
            writer: Box::new(writer),
            child: None,
            hint: StderrHint::default(),
            closer,
        })
    }

    /// A connection over halves the caller already holds.
    ///
    /// The pairing dance that [`Self::connect_to`] runs exists to turn one
    /// dialable address into two one-way streams, which is what Windows needs
    /// and what a child process's pipes already are. A caller holding both
    /// halves has nothing left to pair.
    #[must_use]
    pub fn from_halves(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        Self {
            reader,
            writer,
            child: None,
            hint: StderrHint::default(),
            closer: Closer::default(),
        }
    }

    /// A connection to the daemon a command speaks for.
    ///
    /// The command's stdout is the reader and its stdin is the writer. Its
    /// stderr is drained on a thread of its own: every line goes to the log,
    /// and the first is kept for the error that a failed handshake will
    /// otherwise report as mere silence.
    pub fn over_command(
        program: &std::ffi::OsStr,
        args: &[std::ffi::OsString],
    ) -> Result<Self, IpcError> {
        use std::process::{Command, Stdio};

        let mut described = program.to_string_lossy().into_owned();
        for arg in args {
            described.push(' ');
            described.push_str(&arg.to_string_lossy());
        }

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        put_in_its_own_group(&mut command);

        let mut child = command.spawn().map_err(|source| IpcError::Spawn {
            command: described.clone(),
            source,
        })?;

        let reader = child.stdout.take().expect("stdout was piped");
        let writer = child.stdin.take().expect("stdin was piped");
        let hint = StderrHint::default();

        if let Some(stderr) = child.stderr.take() {
            let hint = hint.clone();
            let described = described.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                {
                    tracing::warn!(command = %described, "{line}");
                    hint.remember(&line);
                }
            });
        }

        let closer = Closer::process(child.id());

        Ok(Self {
            reader: Box::new(reader),
            writer: Box::new(writer),
            child: Some(child),
            hint,
            closer,
        })
    }

    /// What the command said on stderr, for an error that needs it.
    #[must_use]
    pub fn hint(&self) -> StderrHint {
        self.hint.clone()
    }

    /// Ends this connection from outside, whoever ends up holding its halves.
    ///
    /// Taken before [`Connection::split`], which hands the halves to threads
    /// that may park in them.
    #[must_use]
    pub fn closer(&self) -> Closer {
        self.closer.clone()
    }

    /// The child's process id, when the transport is a command.
    #[must_use]
    pub fn child_id(&self) -> Option<u32> {
        self.child.as_ref().map(std::process::Child::id)
    }

    /// Splits into a reader and a writer.
    ///
    /// The loop reads on one thread and writes from another, so neither
    /// blocks the other. Boxed rather than `impl Trait`: a connection's
    /// transport is chosen at runtime, and the type cannot be named at the
    /// boundary.
    ///
    /// The child, when there is one, rides with the reader: the halves
    /// outlive this `Connection`, and killing the process when it goes would
    /// close the transport the caller just took.
    pub fn split(mut self) -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
        let child = self.child.take();
        let reader = std::mem::replace(&mut self.reader, Box::new(std::io::empty()));
        let writer = std::mem::replace(&mut self.writer, Box::new(std::io::sink()));

        match child {
            Some(child) => (Box::new(ChildReader { reader, child }), writer),
            None => (reader, writer),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Asked to stop and then waited for: a child left unreaped is a
        // zombie, and one left running is an `ssh` nobody can see.
        if let Some(child) = &mut self.child {
            reap(child);
        }
    }
}

/// How long a command transport's process tree is given to exit on its own.
///
/// Shorter than [`process::DEFAULT_GRACE`](crate::process::DEFAULT_GRACE):
/// that one waits for an agent closing a pane, this one for a transport
/// tearing down, which should not make a reconnect wait on it.
const TEARDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(50);

/// Terminates a command transport's whole process tree, then reaps its
/// immediate child.
///
/// `ssh` and `sh -c` both fork; killing only the process this crate spawned
/// would leave those orphaned and holding the pipes this `Connection` reads
/// and writes, which is what [`put_in_its_own_group`] and
/// [`process::terminate_tree`](crate::process::terminate_tree) are for.
fn reap(child: &mut std::process::Child) {
    let _ = crate::process::terminate_tree(child.id(), TEARDOWN_GRACE);
    let _ = child.wait();
}

/// Puts `command`'s child in a process group or job of its own, so
/// [`process::terminate_tree`](crate::process::terminate_tree) can reach
/// everything it forks rather than only the child itself.
///
/// The same treatment [`process::spawn_detached`](crate::process::spawn_detached)
/// gives the daemon it starts, for the same reason: a command transport's
/// child is not necessarily a leaf either.
#[cfg(unix)]
fn put_in_its_own_group(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: setsid is async-signal-safe and is the documented way to leave
    // the parent's session and become a process group leader, which is what
    // lets `killpg` reach every descendant later. The closure allocates
    // nothing and touches no shared state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn put_in_its_own_group(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    /// Starts the child as the root of its own process group, so a signal
    /// meant for it does not also reach this process, and so it can be
    /// addressed as a group later.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// A reader that reaps its process when it is dropped.
///
/// `split` hands the halves to a client that may hold them for the life of a
/// connection; the child has to be owned by one of them or it would be killed
/// the moment the `Connection` went out of scope.
struct ChildReader {
    reader: Box<dyn Read + Send>,
    child: std::process::Child,
}

impl Read for ChildReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Drop for ChildReader {
    fn drop(&mut self) {
        reap(&mut self.child);
    }
}

/// The first line a command wrote to stderr, if it wrote one.
///
/// A command transport fails in ways only its stderr explains — a refused SSH
/// key, a missing binary on the far side — and by the time the handshake times
/// out, that line is the only evidence of which happened.
#[derive(Clone, Default)]
pub struct StderrHint(std::sync::Arc<Mutex<Option<String>>>);

impl std::fmt::Debug for StderrHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StderrHint")
    }
}

impl StderrHint {
    /// The first line, once there is one.
    #[must_use]
    pub fn first_line(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn remember(&self, line: &str) {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if held.is_none() {
            *held = Some(line.to_string());
        }
    }
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

/// Accepts client connections.
///
/// Accepting runs on a thread of its own from the moment the endpoint is
/// bound, and each connection's preamble is read on a thread of its own
/// again, against [`PREAMBLE_PATIENCE`]. A client that connects and says
/// nothing -- a wedged build, or on Windows a handle opened read-only --
/// costs that one thread for two seconds and holds up nobody.
pub struct Listener {
    /// Connections whose two halves have both arrived and announced themselves.
    paired: Mutex<Receiver<Result<Connection, IpcError>>>,
    /// Asks the accepting thread to stop, once something wakes it.
    stopping: Arc<AtomicBool>,
    /// Where it listens, so dropping it can wake the accepting thread.
    endpoint: PathBuf,
    accepting: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Listener")
    }
}

impl Listener {
    /// Starts listening, refusing to start beside a running daemon.
    pub fn bind() -> Result<Self, IpcError> {
        Self::bind_to(&endpoint()?)
    }

    /// Starts listening on `path`, refusing to start beside a running daemon.
    ///
    /// For a daemon whose endpoint is not this configuration's own: a test
    /// that stands one up, without steering the process-wide configuration
    /// directory to put it there.
    pub fn bind_to(path: &Path) -> Result<Self, IpcError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| IpcError::io(format!("creating {}", parent.display()), e))?;
        }

        let inner = imp::bind(path)?;
        let (sender, paired) = channel();
        let stopping = Arc::new(AtomicBool::new(false));

        let accepting = {
            let stopping = Arc::clone(&stopping);
            std::thread::spawn(move || accept_all(&inner, &sender, &stopping))
        };

        Ok(Self {
            paired: Mutex::new(paired),
            stopping,
            endpoint: path.to_path_buf(),
            accepting: Some(accepting),
        })
    }

    /// Waits for the next client, meaning both halves of one.
    pub fn accept(&self) -> Result<Connection, IpcError> {
        let paired = self.paired.lock().unwrap_or_else(|e| e.into_inner());
        match paired.recv() {
            Ok(result) => result,
            // The accepting thread reports why before it stops, so an empty,
            // closed queue means it stopped without a reason to give.
            Err(_) => Err(IpcError::io(
                "accepting a connection",
                std::io::Error::from(std::io::ErrorKind::BrokenPipe),
            )),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);

        // The accepting thread is parked in the platform's accept; one
        // connection of our own wakes it to see the flag. Joined, so the
        // endpoint is free once this returns: a daemon restarted in the same
        // process, or a test binding the same path again, must not find the
        // old listener still answering.
        //
        // Joined only when the wake got through. One that cannot reach the
        // endpoint means no one else can either -- its socket file was
        // removed under it, or the accepting thread has already stopped --
        // and a join that nothing will wake would hang this drop for good.
        let woken = imp::connect(&self.endpoint).is_ok();
        if let Some(accepting) = self.accepting.take()
            && woken
        {
            let _ = accepting.join();
        }
    }
}

/// Accepts until the listener fails or is dropped, reading each preamble on
/// a thread of its own and handing on each connection once both of its
/// halves are in.
fn accept_all(
    inner: &imp::Listener,
    paired: &Sender<Result<Connection, IpcError>>,
    stopping: &AtomicBool,
) {
    let halves = Arc::new(Mutex::new(pairing::Halves::new()));
    let announcing = Arc::new(AtomicUsize::new(0));

    loop {
        let stream = match imp::accept(inner) {
            Ok(stream) => stream,
            Err(error) => {
                let _ = paired.send(Err(error));
                return;
            }
        };

        if stopping.load(Ordering::Relaxed) {
            return;
        }

        if announcing.load(Ordering::Relaxed) >= MAX_ANNOUNCING {
            tracing::warn!(
                "closing a connection: {MAX_ANNOUNCING} others have not yet said which half they are"
            );
            continue;
        }

        announcing.fetch_add(1, Ordering::Relaxed);
        let halves = Arc::clone(&halves);
        let announcing = Arc::clone(&announcing);
        let paired = paired.clone();

        std::thread::spawn(move || {
            let mut stream = stream;
            let half = imp::read_preamble(&mut stream, PREAMBLE_PATIENCE);
            announcing.fetch_sub(1, Ordering::Relaxed);

            // A client that vanished, stalled, or was speaking to something
            // else. Nothing is owed to it.
            let Ok((token, role)) = half else { return };

            let offered = halves.lock().unwrap_or_else(|e| e.into_inner()).offer(
                token,
                role,
                stream,
                Instant::now(),
            );

            if let Some((reader, writer)) = offered {
                let _ = paired.send(Connection::over_streams(reader, writer));
            }
        });
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;

    use super::IpcError;

    pub(super) type Stream = UnixStream;
    pub(super) type Listener = UnixListener;

    pub(super) fn connect(path: &Path) -> Result<Stream, IpcError> {
        UnixStream::connect(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                IpcError::NotRunning(path.display().to_string())
            }
            _ => IpcError::io(format!("connecting to {}", path.display()), e),
        })
    }

    pub(super) fn bind(path: &Path) -> Result<Listener, IpcError> {
        // A socket file outlives the process that made it, so a daemon that
        // was killed leaves one behind. Connecting tells the two cases apart:
        // a refused connection means nothing is listening and the file is
        // stale, while a successful one means a daemon really is running.
        if path.exists() {
            match UnixStream::connect(path) {
                Ok(_) => return Err(IpcError::AlreadyRunning(path.display().to_string())),
                Err(_) => {
                    std::fs::remove_file(path).map_err(|e| {
                        IpcError::io(format!("removing the stale socket {}", path.display()), e)
                    })?;
                }
            }
        }

        let listener = UnixListener::bind(path)
            .map_err(|e| IpcError::io(format!("listening on {}", path.display()), e))?;

        // The daemon can run arbitrary commands, so no one else may connect.
        restrict_to_owner(path)?;

        Ok(listener)
    }

    /// Makes the socket readable and writable only by its owner.
    fn restrict_to_owner(path: &Path) -> Result<(), IpcError> {
        use std::os::unix::fs::PermissionsExt;

        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, permissions)
            .map_err(|e| IpcError::io(format!("securing {}", path.display()), e))
    }

    pub(super) fn accept(listener: &Listener) -> Result<Stream, IpcError> {
        listener
            .accept()
            .map(|(stream, _)| stream)
            .map_err(|e| IpcError::io("accepting a connection", e))
    }

    /// A second handle onto the same socket, for a [`super::Closer`].
    pub(super) fn try_clone(stream: &Stream) -> std::io::Result<Stream> {
        stream.try_clone()
    }

    /// Shuts both directions down: what is parked on the socket fails now,
    /// and everything after fails too.
    pub(super) fn interrupt(stream: &Stream) {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    /// Reads a connection's preamble, giving up after `patience`.
    ///
    /// A failure to set the timeout only loses the bound, which is why it is
    /// ignored; the frame loop that follows expects blocking reads again.
    pub(super) fn read_preamble(
        stream: &mut Stream,
        patience: std::time::Duration,
    ) -> Result<(super::pairing::Token, u8), IpcError> {
        let _ = stream.set_read_timeout(Some(patience));
        let half = super::pairing::listen_for(stream);
        let _ = stream.set_read_timeout(None);
        half
    }
}

#[cfg(windows)]
mod imp {
    use std::io::{Read, Write};
    use std::os::windows::io::FromRawHandle;
    use std::path::Path;
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_BUSY,
        ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::IO::CancelIoEx;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    use super::IpcError;

    /// Named pipes are addressed by name rather than by a filesystem path, so
    /// the endpoint is hashed into one. Two configurations therefore get two
    /// pipes, matching how the Unix socket lives under the config directory.
    fn pipe_name(path: &Path) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in path.display().to_string().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!(r"\\.\pipe\dispatchd-{hash:016x}")
    }

    pub(super) struct Stream(std::fs::File);

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }

    pub(super) struct Listener {
        name: String,
        /// The instance waiting for the next client.
        ///
        /// An instance has to exist before a client can connect, so one is
        /// always kept open: creating it inside `accept` would leave a window
        /// where a client that connected first found nothing.
        ///
        /// Stored as a raw handle because `HANDLE` is a pointer and therefore
        /// not `Send`; the pipe is owned solely by this listener.
        pending: Mutex<isize>,
    }

    // SAFETY: the handle is owned exclusively by this listener and is only
    // touched under the mutex.
    unsafe impl Send for Listener {}
    // SAFETY: as above — every access to the handle goes through the mutex, so
    // sharing the listener between threads cannot race on it.
    unsafe impl Sync for Listener {}

    impl Drop for Listener {
        fn drop(&mut self) {
            use windows_sys::Win32::Foundation::CloseHandle;

            let handle = *self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if handle != INVALID_HANDLE_VALUE as isize {
                // SAFETY: the handle came from CreateNamedPipeW and is closed
                // exactly once, here.
                unsafe { CloseHandle(handle as HANDLE) };
            }
        }
    }

    /// Creates one pipe instance.
    ///
    /// `first` asks the kernel to fail if an instance already exists, which is
    /// how a second daemon is detected without a lock file of its own.
    fn create_instance(name: &str, first: bool) -> Result<isize, std::io::Error> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

        let mut flags = PIPE_ACCESS_DUPLEX;
        if first {
            flags |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }

        // SAFETY: `wide` is a NUL-terminated wide string that outlives the
        // call. A null security descriptor gives the pipe the default, which
        // grants access to the creating user only -- the same boundary the
        // Unix socket's 0600 mode provides.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                std::ptr::null(),
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }

        Ok(handle as isize)
    }

    /// How long a client waits for a busy pipe before giving up.
    ///
    /// Matches the client's own handshake timeout: past it the caller has
    /// stopped waiting anyway.
    const BUSY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

    pub(super) fn connect(path: &Path) -> Result<Stream, IpcError> {
        let name = pipe_name(path);
        let deadline = std::time::Instant::now() + BUSY_PATIENCE;

        loop {
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&name)
                .map(Stream);

            let error = match opened {
                Ok(stream) => return Ok(stream),
                Err(error) => error,
            };

            // Every instance is already serving someone. A listener holds one
            // unconnected instance and opens the next only once a client has
            // taken it, so two clients -- or one client's two connections --
            // will land in that gap. A busy pipe means the daemon is there;
            // waiting is the only correct answer.
            if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
                if std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                return Err(IpcError::io(format!("connecting to {name}"), error));
            }

            return Err(match error.kind() {
                std::io::ErrorKind::NotFound => IpcError::NotRunning(name),
                _ => IpcError::io(format!("connecting to {name}"), error),
            });
        }
    }

    pub(super) fn bind(path: &Path) -> Result<Listener, IpcError> {
        let name = pipe_name(path);

        // Unlike a Unix socket there is no file to go stale: a pipe exists
        // only while its server holds it, so a refusal here means a daemon is
        // genuinely running.
        let pending = create_instance(&name, true).map_err(|e| match e.raw_os_error() {
            Some(code) if code == ERROR_ACCESS_DENIED as i32 => {
                IpcError::AlreadyRunning(name.clone())
            }
            _ => IpcError::io(format!("listening on {name}"), e),
        })?;

        Ok(Listener {
            name,
            pending: Mutex::new(pending),
        })
    }

    pub(super) fn accept(listener: &Listener) -> Result<Stream, IpcError> {
        let mut pending = listener.pending.lock().unwrap_or_else(|e| e.into_inner());

        let handle = *pending;

        loop {
            // SAFETY: `handle` is a live pipe instance held by this listener.
            let connected = unsafe { ConnectNamedPipe(handle as HANDLE, std::ptr::null_mut()) };

            if connected != 0 {
                break;
            }

            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                // A client that connected between creation and this call has
                // already succeeded; that is not a failure.
                Some(code) if code == ERROR_PIPE_CONNECTED as i32 => break,

                // A client that connected and closed again before this call.
                // Dispatch does this on purpose: every "is a daemon there?"
                // probe connects and drops. The instance has to be
                // disconnected before it will serve anyone else, and then it is
                // as good as new -- so this is not an error to report, it is
                // the next client's turn.
                Some(code) if code == ERROR_NO_DATA as i32 || code == ERROR_BROKEN_PIPE as i32 => {
                    // SAFETY: as above; the instance is this listener's and is
                    // being returned to the unconnected state, not closed.
                    unsafe { DisconnectNamedPipe(handle as HANDLE) };
                }

                _ => {
                    return Err(IpcError::io(
                        format!("accepting on {}", listener.name),
                        error,
                    ));
                }
            }
        }

        // Open the next instance before handing this one over, so the pipe is
        // never absent between clients.
        *pending = create_instance(&listener.name, false)
            .map_err(|e| IpcError::io(format!("reopening {}", listener.name), e))?;

        // SAFETY: the handle is a connected instance and ownership moves into
        // the File, which closes it exactly once.
        Ok(Stream(unsafe {
            std::fs::File::from_raw_handle(handle as _)
        }))
    }

    /// A second handle onto the same pipe, for a [`super::Closer`].
    pub(super) fn try_clone(stream: &Stream) -> std::io::Result<Stream> {
        stream.0.try_clone().map(Stream)
    }

    /// Cancels what is in flight on the pipe, from whichever thread issued it.
    pub(super) fn interrupt(stream: &Stream) {
        use std::os::windows::io::AsRawHandle;

        // SAFETY: the handle is a live duplicate the caller holds for the
        // length of the call, and cancelling reads or writes no memory of
        // ours.
        unsafe { CancelIoEx(stream.0.as_raw_handle() as HANDLE, std::ptr::null()) };
    }

    /// Reads a connection's preamble, giving up after `patience`.
    ///
    /// A synchronous pipe read has no timeout of its own, so a watchdog
    /// cancels it. The watchdog holds its own handle, so the one it cancels
    /// cannot have been closed and reused under it; it is joined before this
    /// returns, so it cannot cancel anything the paired connection does
    /// later; and a cancel that lands just after the read finished finds
    /// nothing in flight.
    pub(super) fn read_preamble(
        stream: &mut Stream,
        patience: std::time::Duration,
    ) -> Result<(super::pairing::Token, u8), IpcError> {
        let watched =
            try_clone(stream).map_err(|e| IpcError::io("watching a connection's preamble", e))?;
        let (finished, wait) = std::sync::mpsc::channel::<()>();

        let watchdog = std::thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = wait.recv_timeout(patience) {
                interrupt(&watched);
            }
        });

        let half = super::pairing::listen_for(stream);
        drop(finished);
        let _ = watchdog.join();
        half
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Points the endpoint at a directory of this test's own.
    ///
    /// The variable is process-wide, so every test that touches it holds
    /// `crate::env_lock()` for as long as its redirection lasts.
    struct Endpoint {
        dir: PathBuf,
        previous: Option<std::ffi::OsString>,
    }

    impl Endpoint {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("dispatch-ipc-{}-{label}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir is writable");

            let previous = std::env::var_os(crate::paths::CONFIG_DIR_ENV);

            // SAFETY: these tests are serialised by the mutex below, and no
            // other test in this crate reads the variable concurrently.
            unsafe { std::env::set_var(crate::paths::CONFIG_DIR_ENV, &dir) };

            Self { dir, previous }
        }
    }

    impl Drop for Endpoint {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(crate::paths::CONFIG_DIR_ENV, value),
                    None => std::env::remove_var(crate::paths::CONFIG_DIR_ENV),
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_client_and_server_exchange_bytes() {
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("exchange");

        let listener = Listener::bind().expect("binding succeeds");

        let server = std::thread::spawn(move || {
            let mut connection = listener.accept().expect("accepting succeeds");
            let mut buf = [0u8; 5];
            connection.read_exact(&mut buf).expect("reading succeeds");
            connection.write_all(b"world").expect("writing succeeds");
            connection.flush().expect("flushing succeeds");
            buf
        });

        let mut client = Connection::connect().expect("connecting succeeds");
        client.write_all(b"hello").expect("writing succeeds");
        client.flush().expect("flushing succeeds");

        let mut reply = [0u8; 5];
        client.read_exact(&mut reply).expect("reading succeeds");

        assert_eq!(
            &server.join().expect("the server thread finishes"),
            b"hello"
        );
        assert_eq!(&reply, b"world");
    }

    /// Reads four bytes on a thread of its own and reports them back.
    ///
    /// A deadlock is the failure these tests look for, so nothing waits on a
    /// thread without a bound: a hung `join` would burn a CI job's whole
    /// timeout instead of failing.
    fn reading(mut reader: impl Read + Send + 'static) -> std::sync::mpsc::Receiver<[u8; 4]> {
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4];
            if reader.read_exact(&mut buf).is_ok() {
                let _ = sent.send(buf);
            }
        });
        received
    }

    /// How long a test waits before calling something deadlocked.
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

    #[test]
    fn a_parked_reader_does_not_hold_up_a_writer() {
        // The defect this transport exists to fix. Both ends park a reader
        // thread and then write from another -- which is what every Dispatch
        // connection does -- and on Windows a single connection's duplicated
        // handles serialise those operations into a deadlock.
        //
        // It passes on Unix either way: a socket's cloned descriptors were
        // never the problem. It is here so the transport carries its own
        // regression test on the platform that needs one.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("parked");

        let listener = Listener::bind().expect("binding succeeds");

        let (served, from_server) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (reader, mut writer) = listener.accept().expect("accepting succeeds").split();
            let heard = reading(reader);
            writer.write_all(b"pong").expect("writing succeeds");
            writer.flush().expect("flushing succeeds");
            let _ = served.send(heard.recv_timeout(PATIENCE));
        });

        let (reader, mut writer) = Connection::connect().expect("connecting succeeds").split();
        let heard = reading(reader);
        writer.write_all(b"ping").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        assert_eq!(
            heard.recv_timeout(PATIENCE),
            Ok(*b"pong"),
            "the client's parked reader never saw the daemon's write"
        );
        assert_eq!(
            from_server.recv_timeout(PATIENCE),
            Ok(Ok(*b"ping")),
            "the daemon's parked reader never saw the client's write"
        );
    }

    #[test]
    fn two_clients_racing_get_their_own_connections() {
        // Four connections race into one listener. Pairing them by arrival
        // order would hand each client half of the other's.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("concurrent");

        let listener = Listener::bind().expect("binding succeeds");

        let (echoed, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (reader, mut writer) = listener.accept().expect("accepting succeeds").split();
                let heard = reading(reader)
                    .recv_timeout(PATIENCE)
                    .expect("a client wrote");
                // Echo what this client said back down its own half. A crossed
                // pair sends it to the other client.
                writer.write_all(&heard).expect("writing succeeds");
                writer.flush().expect("flushing succeeds");
                let _ = echoed.send(heard);
            }
        });

        let clients: Vec<_> = [*b"aaaa", *b"bbbb"]
            .into_iter()
            .map(|name| {
                std::thread::spawn(move || {
                    let (reader, mut writer) =
                        Connection::connect().expect("connecting succeeds").split();
                    writer.write_all(&name).expect("writing succeeds");
                    writer.flush().expect("flushing succeeds");
                    (name, reading(reader).recv_timeout(PATIENCE))
                })
            })
            .collect();

        for client in clients {
            let (name, heard) = client.join().expect("the client thread finishes");
            assert_eq!(
                heard,
                Ok(name),
                "a client was handed the other client's connection"
            );
        }

        let mut served = [
            done.recv_timeout(PATIENCE)
                .expect("the first client is served"),
            done.recv_timeout(PATIENCE)
                .expect("the second client is served"),
        ];
        served.sort_unstable();
        assert_eq!(served, [*b"aaaa", *b"bbbb"]);
    }

    #[test]
    fn a_client_whose_partner_never_comes_does_not_shut_out_the_next_one() {
        // One connection with a valid preamble and no partner behind it. It
        // waits in the table forever; the listener owes the next client a
        // connection regardless.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("orphan");

        let listener = Listener::bind().expect("binding succeeds");
        let path = endpoint().expect("resolves");

        let (served, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (reader, _writer) = listener.accept().expect("accepting succeeds").split();
            let _ = served.send(reading(reader).recv_timeout(PATIENCE));
        });

        let mut orphan = imp::connect(&path).expect("connecting succeeds");
        pairing::half_for_test(&mut orphan).expect("announcing succeeds");

        let (_reader, mut writer) = Connection::connect().expect("connecting succeeds").split();
        writer.write_all(b"real").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        assert_eq!(
            done.recv_timeout(PATIENCE),
            Ok(Ok(*b"real")),
            "a half-connected client must not cost the next one its connection"
        );
    }

    #[test]
    fn a_client_that_died_mid_handshake_is_forgotten() {
        // Connected, then gone before saying anything. The read fails at once
        // and the listener carries on.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("vanished");

        let listener = Listener::bind().expect("binding succeeds");
        let path = endpoint().expect("resolves");

        let (served, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (reader, _writer) = listener.accept().expect("accepting succeeds").split();
            let _ = served.send(reading(reader).recv_timeout(PATIENCE));
        });

        drop(imp::connect(&path).expect("connecting succeeds"));

        let (_reader, mut writer) = Connection::connect().expect("connecting succeeds").split();
        writer.write_all(b"real").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        assert_eq!(
            done.recv_timeout(PATIENCE),
            Ok(Ok(*b"real")),
            "a client that vanished must not cost the next one its connection"
        );
    }

    #[test]
    fn connecting_with_no_daemon_says_so() {
        // The message a user sees when they start a client first, so it must
        // name the situation rather than an errno.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("absent");

        let error = Connection::connect().expect_err("nothing is listening");
        assert!(
            matches!(error, IpcError::NotRunning(_)),
            "expected NotRunning, got {error:?}"
        );
    }

    #[test]
    fn a_second_daemon_refuses_to_start() {
        // Two daemons on one endpoint would each own half the panes.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("double");

        let _first = Listener::bind().expect("the first binds");
        let error = Listener::bind().expect_err("the second must refuse");

        assert!(
            matches!(error, IpcError::AlreadyRunning(_)),
            "expected AlreadyRunning, got {error:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_socket_left_by_a_dead_daemon_is_replaced() {
        // A socket file outlives the process that made it, so a crash would
        // otherwise block every future start.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("stale");

        let path = endpoint().expect("resolves");
        std::fs::create_dir_all(path.parent().expect("has a parent"))
            .expect("temp dir is writable");
        std::fs::write(&path, b"").expect("temp dir is writable");

        let _listener = Listener::bind().expect("a stale socket must not block a start");
    }

    #[test]
    #[cfg(unix)]
    fn the_socket_is_not_readable_by_other_users() {
        // The daemon runs arbitrary commands, so the transport is the
        // boundary that keeps another account off it.
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("permissions");

        let _listener = Listener::bind().expect("binding succeeds");
        let path = endpoint().expect("resolves");

        let mode = std::fs::metadata(&path)
            .expect("the socket exists")
            .permissions()
            .mode();

        assert_eq!(mode & 0o077, 0, "group and others must have no access");
    }

    #[test]
    #[cfg(unix)]
    fn a_connection_can_be_built_from_halves_the_caller_already_has() {
        // Federation's transports are not all sockets: one of them is a child
        // process's two pipes. A connection has to be assemblable from
        // whatever the caller has.
        use std::os::unix::net::UnixStream;

        let (mine, theirs) = UnixStream::pair().expect("a socket pair");
        let reader = theirs.try_clone().expect("a reader half");

        let connection = Connection::from_halves(Box::new(reader), Box::new(mine));
        let (mut reader, mut writer) = connection.split();

        writer.write_all(b"ping").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).expect("reading succeeds");
        assert_eq!(&buf, b"ping");

        drop(theirs);
    }

    #[test]
    #[cfg(unix)]
    fn a_command_carries_bytes_both_ways() {
        // `cat` is a byte-for-byte loopback, so this proves the pipes are
        // wired the right way round and that nothing in between reframes.
        let connection =
            Connection::over_command(std::ffi::OsStr::new("cat"), &[]).expect("cat exists");

        let (mut reader, mut writer) = connection.split();
        writer.write_all(b"ping\n").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).expect("reading succeeds");
        assert_eq!(&buf, b"ping\n");
    }

    #[test]
    fn a_command_that_is_not_there_names_itself() {
        // Over SSH this is the common failure — the far side has no dispatchd
        // — and a bare "not found" would not say what was looked for.
        let error = Connection::over_command(std::ffi::OsStr::new("dispatch-no-such-program"), &[])
            .expect_err("it does not exist");

        assert!(
            error.to_string().contains("dispatch-no-such-program"),
            "{error}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn dropping_a_command_connection_reaps_its_child() {
        // A client reconnects by dialling again. A transport that left its
        // child running would leak one per attempt, invisibly.
        //
        // `cat` exits the moment its stdin pipe closes -- which happens by
        // ordinary field drop regardless of what `Connection::drop` does --
        // so it would pass this test even with the reap deleted. `sleep`
        // does not exit on EOF; only an actual kill ends it. Its
        // grandchild -- a second `sleep` the shell backgrounds and records
        // the pid of -- is what proves the whole tree is reached and not
        // just the immediate child: a fix that kills only `sh` would still
        // leave the grandchild running.
        fn pid_is_alive(pid: u32) -> bool {
            // SAFETY: signal 0 sends nothing and only probes whether the pid
            // exists, which is safe to ask about any pid, alive or not.
            unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
        }

        let dir =
            std::env::temp_dir().join(format!("dispatch-ipc-test-{}-reap", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        let pid_file = dir.join("grandchild.pid");

        let connection = Connection::over_command(
            std::ffi::OsStr::new("sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(format!(
                    "sleep 30 & echo $! > {}; wait",
                    pid_file.display()
                )),
            ],
        )
        .expect("sh exists");

        let pid = connection
            .child_id()
            .expect("a command transport has a child");

        // Wait for the shell to record its background child's pid.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let grandchild = loop {
            if let Ok(contents) = std::fs::read_to_string(&pid_file)
                && let Ok(grandchild) = contents.trim().parse::<u32>()
            {
                break grandchild;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the shell never wrote the grandchild's pid"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        drop(connection);

        // A reaped process's pid answers no signal; an unreaped one does.
        // Polled: the tree has been killed by the time `drop` returns, but
        // the grandchild answers `kill(pid, 0)` until whoever inherited it
        // reaps it, and on macOS that is launchd, on its own schedule.
        let deadline = std::time::Instant::now() + PATIENCE;
        while (pid_is_alive(pid) || pid_is_alive(grandchild))
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!pid_is_alive(pid), "the shell is gone");
        assert!(
            !pid_is_alive(grandchild),
            "the grandchild outlived the process group"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn a_commands_stderr_is_kept_for_the_error_that_needs_it() {
        // "Permission denied (publickey)" only exists on stderr, and without
        // it a failed attach is indistinguishable from a silent one.
        let connection = Connection::over_command(
            std::ffi::OsStr::new("sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("echo nope 1>&2; sleep 5"),
            ],
        )
        .expect("sh exists");

        let hint = connection.hint();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while hint.first_line().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(hint.first_line().as_deref(), Some("nope"));
    }

    #[test]
    fn the_endpoint_follows_the_configuration_directory() {
        // Two configurations must not share one daemon.
        let _guard = crate::env_lock();
        let endpoint_guard = Endpoint::new("scoped");

        let path = endpoint().expect("resolves");
        assert!(
            path.starts_with(&endpoint_guard.dir),
            "{} should be under {}",
            path.display(),
            endpoint_guard.dir.display()
        );
    }

    #[test]
    fn a_client_that_never_announces_itself_does_not_hold_up_the_next() {
        // One connection says nothing at all. The listener used to read its
        // preamble itself, so every client behind it waited out the whole
        // two seconds -- and on Windows, where the read has no timeout,
        // waited forever.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("silent-first");

        let listener = Listener::bind().expect("binding succeeds");
        let path = endpoint().expect("resolves");

        let (served, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (reader, _writer) = listener.accept().expect("accepting succeeds").split();
            let _ = served.send(reading(reader).recv_timeout(PATIENCE));
        });

        let _silent = imp::connect(&path).expect("connecting succeeds");
        // Let the listener take the silent one first, as it would in life.
        std::thread::sleep(Duration::from_millis(50));

        let started = std::time::Instant::now();
        let (_reader, mut writer) = Connection::connect().expect("connecting succeeds").split();
        writer.write_all(b"next").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        assert_eq!(done.recv_timeout(PATIENCE), Ok(Ok(*b"next")));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the next client waited {:?} behind a silent one",
            started.elapsed()
        );
    }

    #[test]
    fn a_client_that_never_announces_itself_is_let_go() {
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("silent-closed");

        let _listener = Listener::bind().expect("binding succeeds");
        let path = endpoint().expect("resolves");

        let mut silent = imp::connect(&path).expect("connecting succeeds");
        let (ended, end) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            // End of file on Unix, a broken pipe on Windows: either way the
            // listener has let the connection go.
            let _ = silent.read(&mut byte);
            let _ = ended.send(());
        });

        assert!(
            end.recv_timeout(PATIENCE).is_ok(),
            "a connection that never said which half it was is still open"
        );
    }

    #[test]
    fn dropping_a_listener_frees_its_endpoint() {
        // Accepting now runs on a thread of its own. A drop that did not wait
        // for it would leave the endpoint answering for a moment, and the
        // next bind would take the old listener for a running daemon.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("rebind");

        let first = Listener::bind().expect("binding succeeds");
        drop(first);

        let _second = Listener::bind().expect("the endpoint is free once the first is dropped");
    }

    #[test]
    #[cfg(unix)]
    fn dropping_a_listener_whose_socket_was_removed_returns() {
        // Nothing can wake an accept parked on a socket file that is gone, so
        // a drop that waited for one regardless would never return.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("removed");

        let listener = Listener::bind().expect("binding succeeds");
        std::fs::remove_file(endpoint().expect("resolves")).expect("the socket exists");

        let (dropped, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(listener);
            let _ = dropped.send(());
        });

        assert!(
            done.recv_timeout(PATIENCE).is_ok(),
            "dropping the listener hung on an endpoint nothing can reach"
        );
    }

    #[test]
    fn a_listener_can_be_bound_to_a_path_it_is_given() {
        let _guard = crate::env_lock();
        let endpoint_guard = Endpoint::new("bind-to");
        let path = endpoint_guard.dir.join("elsewhere.sock");

        let listener = Listener::bind_to(&path).expect("binding succeeds");
        std::thread::spawn(move || {
            let (reader, mut writer) = listener.accept().expect("accepting succeeds").split();
            let heard = reading(reader).recv_timeout(PATIENCE);
            if let Ok(heard) = heard {
                let _ = writer.write_all(&heard);
                let _ = writer.flush();
            }
        });

        let (reader, mut writer) = Connection::connect_to(&path)
            .expect("connecting succeeds")
            .split();
        writer.write_all(b"echo").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");
        assert_eq!(reading(reader).recv_timeout(PATIENCE), Ok(*b"echo"));
    }

    #[test]
    fn a_closer_ends_a_connection_whose_reader_is_parked() {
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("closer");

        let listener = Listener::bind().expect("binding succeeds");
        let server = std::thread::spawn(move || listener.accept().expect("accepting succeeds"));

        let client = Connection::connect().expect("connecting succeeds");
        let closer = client.closer();
        let _server_side = server.join().expect("the server thread finishes");

        let (reader, mut writer) = client.split();
        // `reading` sends only on a full read; when the read fails instead,
        // its sender is dropped and the receiver sees a disconnect.
        let parked = reading(reader);

        closer.close();

        assert_eq!(
            parked.recv_timeout(PATIENCE),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the parked read is still parked"
        );

        // Unix shuts the sockets down for good. Windows cancels what was in
        // flight; the pipe itself ends when the threads holding it let go.
        if cfg!(unix) {
            assert!(
                writer
                    .write_all(&[0u8; 64 * 1024])
                    .and_then(|()| writer.flush())
                    .is_err(),
                "a write after closing still went through"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_closer_ends_a_command_transport() {
        let connection = Connection::over_command(
            std::ffi::OsStr::new("sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("sleep 30"),
            ],
        )
        .expect("sh exists");
        let closer = connection.closer();
        let (reader, _writer) = connection.split();
        let parked = reading(reader);

        closer.close();

        assert_eq!(
            parked.recv_timeout(PATIENCE),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the read on a killed command's stdout is still parked"
        );
    }
}
