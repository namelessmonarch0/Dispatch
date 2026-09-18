//! Tests for the client half.
//!
//! Each runs against a hand-written server on a real socket rather than the
//! daemon: what is under test is how the client behaves when the other side
//! welcomes it, refuses it, goes away, or comes back, and a fake server can be
//! made to do all four on demand.

use super::*;

use std::time::Instant;

use dispatch_os::ipc::Listener;
use dispatch_proto::{PaneUpdate, Version};

/// How long to wait for something to happen before failing.
///
/// Generous because reconnection backs off, and a loaded machine should not
/// turn a working client into a failing test.
const PATIENCE: Duration = Duration::from_secs(10);

/// Points the endpoint at a directory of this test's own.
///
/// The variable is process-wide, so these tests run one at a time.
struct Endpoint {
    dir: PathBuf,
    previous: Option<std::ffi::OsString>,
}

impl Endpoint {
    fn new(label: &str) -> Self {
        // Under the system temp directory rather than a deeper path: a Unix
        // socket address is limited to about a hundred bytes.
        let dir =
            std::env::temp_dir().join(format!("dispatch-client-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir is writable");

        let previous = std::env::var_os(dispatch_os::paths::CONFIG_DIR_ENV);

        // SAFETY: these tests are serialised by the mutex below, and nothing
        // else in this crate reads the variable concurrently.
        unsafe { std::env::set_var(dispatch_os::paths::CONFIG_DIR_ENV, &dir) };

        Self { dir, previous }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(dispatch_os::paths::CONFIG_DIR_ENV, value),
                None => std::env::remove_var(dispatch_os::paths::CONFIG_DIR_ENV),
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Serialises the tests that move the process-wide endpoint.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A server writer: boxed, so a test's closure does not have to be generic.
type Writer = Box<dyn Write + Send>;

/// What the fake server does once it has welcomed a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// Closes the connection, which is what a daemon going away looks like.
    HangUp,
    /// Keeps it open and keeps listening.
    Listen,
}

/// What a fake server has been told, readable while it is still running.
///
/// Shared rather than returned from a join: with the connection held open, a
/// join would wait for the client to close it, and the client is what the test
/// is still using.
#[derive(Clone)]
struct Heard(Arc<Mutex<Vec<ClientMessage>>>);

impl Heard {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn push(&self, message: ClientMessage) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(message);
    }

    fn snapshot(&self) -> Vec<ClientMessage> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn contains(&self, message: &ClientMessage) -> bool {
        self.snapshot().contains(message)
    }
}

/// Accepts one client, answers its handshake with `answer`, then runs `serve`.
///
/// Binding happens before the thread starts, so a client connecting immediately
/// finds the endpoint already there.
fn serve_one(
    answer: ServerMessage,
    serve: impl FnOnce(&mut Writer) + Send + 'static,
    after: After,
) -> Heard {
    let listener = Listener::bind().expect("binding succeeds");
    let heard = Heard::new();
    let recording = heard.clone();

    std::thread::spawn(move || {
        let connection = listener.accept().expect("a client connects");
        let (mut reader, writer) = connection.split().expect("splitting succeeds");
        let mut writer: Writer = Box::new(writer);

        let hello = Frame::read::<_, ClientMessage>(&mut reader).expect("a hello arrives");
        recording.push(hello);

        Frame::write(&mut writer, &answer).expect("writing succeeds");
        serve(&mut writer);

        if after == After::HangUp {
            return;
        }

        while let Ok(message) = Frame::read::<_, ClientMessage>(&mut reader) {
            recording.push(message);
        }
    });

    heard
}

/// Waits for `condition`, returning whether it held before the deadline.
fn wait_until(patience: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + patience;

    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    condition()
}

/// Waits for `predicate` to hold over everything polled so far.
fn wait_for(
    client: &Client,
    what: &str,
    predicate: impl Fn(&[ServerMessage]) -> bool,
) -> Vec<ServerMessage> {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();

    while Instant::now() < deadline {
        seen.extend(client.poll());
        if predicate(&seen) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    panic!("timed out waiting for {what}; saw {seen:#?}");
}

/// The welcome a daemon of this version sends.
fn welcome() -> ServerMessage {
    ServerMessage::Welcome {
        version: dispatch_proto::VERSION,
        device: "desktop".into(),
    }
}

#[test]
fn attaching_reports_the_daemon_it_reached() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("welcome");

    let heard = serve_one(welcome(), |_| {}, After::Listen);

    let client = Client::attach("test").expect("attaching succeeds");
    assert_eq!(client.device(), "desktop");
    assert!(client.is_connected());
    assert_eq!(client.generation(), 1, "this is the first connection");

    assert!(client.subscribe(), "the connection is up");

    assert!(
        wait_until(PATIENCE, || heard.contains(&ClientMessage::Subscribe)),
        "subscribing is explicit, sent {:#?}",
        heard.snapshot()
    );
    assert!(
        matches!(heard.snapshot().first(), Some(ClientMessage::Hello { .. })),
        "the client introduces itself first, sent {:#?}",
        heard.snapshot()
    );
}

#[test]
fn a_refusal_fails_the_attach_rather_than_arriving_later() {
    // A client that treated a refusal as an ordinary message would draw an
    // empty interface and wait forever for panes that are never coming.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("refused");

    let refusal = ProtocolError::IncompatibleVersion {
        peer: Version { major: 1, minor: 0 },
        ours: Version {
            major: 99,
            minor: 0,
        },
    };
    let _heard = serve_one(
        ServerMessage::Error {
            error: refusal.clone(),
        },
        |_| {},
        After::HangUp,
    );

    let error = Client::attach("test").expect_err("attaching fails");
    let reported = format!("{error}");
    assert!(
        matches!(error, ClientError::Refused(actual) if actual == refusal),
        "expected the daemon's reason, got {reported}"
    );
}

#[test]
fn messages_arrive_without_blocking_the_caller() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("stream");

    let pane = dispatch_core::PaneId::new();
    let _heard = serve_one(
        welcome(),
        move |writer| {
            Frame::write(
                writer,
                &ServerMessage::PaneOutput {
                    pane,
                    bytes: b"hello".to_vec(),
                },
            )
            .expect("writing succeeds");
            Frame::write(
                writer,
                &ServerMessage::PaneChanged {
                    pane,
                    update: PaneUpdate::Title {
                        title: "building".into(),
                    },
                },
            )
            .expect("writing succeeds");
        },
        After::Listen,
    );

    let client = Client::attach("test").expect("attaching succeeds");

    // Polling an empty queue returns nothing rather than waiting.
    let _ = client.poll();

    let seen = wait_for(&client, "the pane's output and title", |m| m.len() >= 2);
    assert!(matches!(
        seen.first(),
        Some(ServerMessage::PaneOutput { .. })
    ));
    assert!(matches!(
        seen.get(1),
        Some(ServerMessage::PaneChanged { .. })
    ));
}

#[test]
fn a_daemon_that_goes_away_is_noticed() {
    // The interface has to be able to say so, rather than silently accepting
    // keystrokes that reach nothing.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("gone");

    let _heard = serve_one(welcome(), |_| {}, After::HangUp);

    let client = Client::attach("test").expect("attaching succeeds");

    assert!(
        wait_until(PATIENCE, || !client.is_connected()),
        "a dropped connection is noticed"
    );
}

#[test]
fn a_daemon_that_comes_back_is_reconnected_to() {
    // The agents are on the daemon's side, so a client that gave up would leave
    // a dead window over live work. It waits instead.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("again");

    // The first daemon welcomes the client and hangs up, as a restart would.
    let _first = serve_one(welcome(), |_| {}, After::HangUp);
    let client = Client::attach("test").expect("attaching succeeds");
    client.subscribe();

    assert!(
        wait_until(PATIENCE, || !client.is_connected()),
        "the client should notice the daemon going away"
    );

    // A second daemon takes the endpoint, as a restarted one would.
    let pane = dispatch_core::PaneId::new();
    let heard = serve_one(
        welcome(),
        move |writer| {
            Frame::write(
                writer,
                &ServerMessage::PaneOutput {
                    pane,
                    bytes: b"still here".to_vec(),
                },
            )
            .expect("writing succeeds");
        },
        After::Listen,
    );

    assert!(
        wait_until(PATIENCE, || client.is_connected()),
        "the client should reconnect on its own"
    );
    assert_eq!(
        client.generation(),
        2,
        "a caller can tell its view is of a connection that is gone"
    );

    wait_for(&client, "output from the new connection", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. }))
    });

    assert!(
        wait_until(PATIENCE, || heard.contains(&ClientMessage::Subscribe)),
        "reconnecting resubscribes, or the panes never come back; sent {:#?}",
        heard.snapshot()
    );
}

#[test]
fn a_handle_taken_before_a_reconnection_still_works_after_one() {
    // Panes keep a handle. Handing out a new one on every reconnection would
    // mean every pane in the interface had to be told.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("handle");

    let _first = serve_one(welcome(), |_| {}, After::HangUp);
    let client = Client::attach("test").expect("attaching succeeds");
    let handle = client.handle();

    assert!(
        wait_until(PATIENCE, || !handle.is_connected()),
        "the handle should report the connection going away"
    );

    let pane = dispatch_core::PaneId::new();
    let heard = serve_one(welcome(), |_| {}, After::Listen);

    assert!(
        wait_until(PATIENCE, || handle.is_connected()),
        "the handle should follow the reconnection"
    );
    assert!(
        handle.send(ClientMessage::ClosePane { pane }),
        "the handle sends"
    );

    assert!(
        wait_until(PATIENCE, || heard
            .contains(&ClientMessage::ClosePane { pane })),
        "the message reached the new connection, sent {:#?}",
        heard.snapshot()
    );
}
