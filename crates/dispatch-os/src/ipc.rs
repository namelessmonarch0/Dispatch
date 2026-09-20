//! Local transport between a Dispatch client and `dispatchd`.
//!
//! A Unix domain socket on POSIX, a named pipe on Windows. Both are
//! local-only and carry the operating system's own access control, which is
//! what keeps another user off a daemon that can run arbitrary commands.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;

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

/// A connected client or server end.
///
/// Two transport connections, one per direction, paired by [`pairing`]. They
/// are separate so that a thread parked reading cannot hold up a thread
/// writing -- which one connection cannot promise on Windows.
pub struct Connection {
    reader: imp::Stream,
    writer: imp::Stream,
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
    pub fn connect_to(endpoint: &std::path::Path) -> Result<Self, IpcError> {
        let (reader, writer) = pairing::dial(|| imp::connect(endpoint))?;
        Ok(Self { reader, writer })
    }

    /// Splits into a reader and a writer.
    ///
    /// The loop reads on one thread and writes from another, so neither
    /// blocks the other.
    pub fn split(self) -> (impl Read + Send, impl Write + Send) {
        (self.reader, self.writer)
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
pub struct Listener {
    inner: imp::Listener,
    /// Connections whose partner has not arrived yet.
    halves: Mutex<pairing::Halves<imp::Stream>>,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Listener")
    }
}

impl Listener {
    /// Starts listening, refusing to start beside a running daemon.
    pub fn bind() -> Result<Self, IpcError> {
        let path = endpoint()?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| IpcError::io(format!("creating {}", parent.display()), e))?;
        }

        Ok(Self {
            inner: imp::bind(&path)?,
            halves: Mutex::new(pairing::Halves::new()),
        })
    }

    /// Waits for the next client, meaning both halves of one.
    ///
    /// Connections arrive one at a time and a client sends two, so this
    /// accepts until some client's pair is complete. The preamble is read here
    /// rather than on a thread of its own: a client that connects and then
    /// neither writes nor exits would hold up this loop, but it is a process of
    /// the same user -- the transport admits no one else -- and the simplicity
    /// is worth more than a defence against the user's own wedged build. A
    /// client that dies mid-handshake closes its connection instead, which
    /// fails the read at once. Unix bounds the wait as well; a synchronous
    /// named pipe read cannot be given a timeout.
    ///
    /// A client that dies *between* its two connections leaves the half it did
    /// announce waiting for a partner that will never come, and nothing reaps
    /// it. The window is the microseconds between two connects, so this costs
    /// one idle entry per client that died inside it -- not a budget worth a
    /// reaper on a local, single-user daemon.
    pub fn accept(&self) -> Result<Connection, IpcError> {
        loop {
            let mut stream = imp::accept(&self.inner)?;

            imp::bound_preamble_wait(&stream);
            let half = pairing::listen_for(&mut stream);
            imp::unbounded_reads(&stream);

            // A client that vanished, stalled, or was speaking to something
            // else. Nothing is owed to it, and the next client is still owed a
            // listener.
            let Ok((token, role)) = half else { continue };

            let paired = self
                .halves
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .offer(token, role, stream);

            if let Some((reader, writer)) = paired {
                return Ok(Connection { reader, writer });
            }
        }
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

    /// Bounds how long a client may take over its preamble.
    ///
    /// The accept loop reads the preamble itself, so an indefinite wait here
    /// would be a wait every other client shares. A failure to set the timeout
    /// only loses that bound, which is why it is ignored.
    pub(super) fn bound_preamble_wait(stream: &Stream) {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    }

    /// Restores the blocking reads the frame loop expects.
    pub(super) fn unbounded_reads(stream: &Stream) {
        let _ = stream.set_read_timeout(None);
    }
}

#[cfg(windows)]
mod imp {
    use std::io::{Read, Write};
    use std::os::windows::io::FromRawHandle;
    use std::path::Path;
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
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

        // SAFETY: `handle` is a live pipe instance held by this listener.
        let connected = unsafe { ConnectNamedPipe(handle as HANDLE, std::ptr::null_mut()) };

        if connected == 0 {
            let error = std::io::Error::last_os_error();
            // A client that connected between creation and this call has
            // already succeeded; that is not a failure.
            if error.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) {
                return Err(IpcError::io(
                    format!("accepting on {}", listener.name),
                    error,
                ));
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

    /// A synchronous named pipe read cannot be given a timeout, so there is no
    /// bound to set. The accept loop's comment says what that costs.
    pub(super) fn bound_preamble_wait(_stream: &Stream) {}

    /// Nothing was bounded, so nothing is restored.
    pub(super) fn unbounded_reads(_stream: &Stream) {}
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
