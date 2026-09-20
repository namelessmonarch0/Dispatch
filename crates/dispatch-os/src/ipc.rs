//! Local transport between a Dispatch client and `dispatchd`.
//!
//! A Unix domain socket on POSIX, a named pipe on Windows. Both are
//! local-only and carry the operating system's own access control, which is
//! what keeps another user off a daemon that can run arbitrary commands.

use std::io::{Read, Write};
use std::path::PathBuf;

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
/// Both directions of one connection; the protocol layer reads and writes
/// frames over it.
pub struct Connection(imp::Stream);

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
        imp::connect(endpoint).map(Self)
    }

    /// Splits into a reader and a writer.
    ///
    /// The loop reads on one thread and writes from another, so neither
    /// blocks the other.
    pub fn split(self) -> Result<(impl Read + Send, impl Write + Send), IpcError> {
        imp::split(self.0)
    }
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Accepts client connections.
pub struct Listener(imp::Listener);

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

        imp::bind(&path).map(Self)
    }

    /// Waits for the next client.
    pub fn accept(&self) -> Result<Connection, IpcError> {
        imp::accept(&self.0).map(Connection)
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{Read, Write};
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

    pub(super) fn split(stream: Stream) -> Result<(impl Read + Send, impl Write + Send), IpcError> {
        let writer = stream
            .try_clone()
            .map_err(|e| IpcError::io("splitting the connection", e))?;
        Ok((stream, writer))
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

    pub(super) fn connect(path: &Path) -> Result<Stream, IpcError> {
        let name = pipe_name(path);

        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .map(Stream)
            .map_err(|e| match e.raw_os_error() {
                // Every instance is busy serving someone, which still means a
                // daemon is there.
                Some(code) if code == ERROR_PIPE_BUSY as i32 => {
                    IpcError::io(format!("connecting to {name}"), e)
                }
                _ if e.kind() == std::io::ErrorKind::NotFound => IpcError::NotRunning(name.clone()),
                _ => IpcError::io(format!("connecting to {name}"), e),
            })
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

    pub(super) fn split(stream: Stream) -> Result<(impl Read + Send, impl Write + Send), IpcError> {
        let writer = stream
            .0
            .try_clone()
            .map_err(|e| IpcError::io("splitting the connection", e))?;
        Ok((stream, Stream(writer)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points the endpoint at a directory of this test's own.
    ///
    /// The variable is process-wide, so these tests run one at a time; they
    /// are marked accordingly and kept few.
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

    /// Serialises the tests that move the process-wide endpoint.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_client_and_server_exchange_bytes() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn connecting_with_no_daemon_says_so() {
        // The message a user sees when they start a client first, so it must
        // name the situation rather than an errno.
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
