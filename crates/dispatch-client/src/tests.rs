//! Tests for the client half.
//!
//! Each runs against a hand-written server on a real socket rather than the
//! daemon: what is under test is how the client behaves when the other side
//! welcomes it, refuses it, or goes away, and a fake server can be made to do
//! all three on demand.

use super::*;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dispatch_os::ipc::Listener;
use dispatch_proto::{PaneUpdate, Version};

/// How long to wait for a message before failing.
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
type Writer = Box<dyn std::io::Write + Send>;

/// Accepts one client, answers the handshake with `answer`, then runs `serve`.
///
/// Reads `expect` further messages from the client and hangs up. It is a count
/// rather than a read-to-end because both sides would otherwise sit waiting for
/// the other to speak: the client's reader blocks until the server sends or
/// closes, and a server reading to end blocks until the client does.
///
/// Binding happens before the thread starts, so a client connecting immediately
/// finds the endpoint already there.
fn serve_one(
    answer: ServerMessage,
    serve: impl FnOnce(&mut Writer) + Send + 'static,
    expect: usize,
) -> std::thread::JoinHandle<Vec<ClientMessage>> {
    let listener = Listener::bind().expect("binding succeeds");

    std::thread::spawn(move || {
        let connection = listener.accept().expect("a client connects");
        let (mut reader, writer) = connection.split().expect("splitting succeeds");
        let mut writer: Writer = Box::new(writer);

        let mut heard = Vec::new();
        let hello = Frame::read::<_, ClientMessage>(&mut reader).expect("a hello arrives");
        heard.push(hello);

        Frame::write(&mut writer, &answer).expect("writing succeeds");
        serve(&mut writer);

        for _ in 0..expect {
            match Frame::read::<_, ClientMessage>(&mut reader) {
                Ok(message) => heard.push(message),
                Err(_) => break,
            }
        }

        heard
    })
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

#[test]
fn attaching_reports_the_daemon_it_reached() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("welcome");

    let server = serve_one(
        ServerMessage::Welcome {
            version: dispatch_proto::VERSION,
            device: "desktop".into(),
        },
        |_| {},
        1,
    );

    let client = Client::attach("test").expect("attaching succeeds");
    assert_eq!(client.device(), "desktop");
    assert!(client.is_connected());

    assert!(client.subscribe(), "the connection is still up");

    let heard = server.join().expect("the server does not panic");
    assert!(
        matches!(heard.first(), Some(ClientMessage::Hello { .. })),
        "the client introduces itself first, sent {heard:#?}"
    );
    assert!(
        heard.contains(&ClientMessage::Subscribe),
        "subscribing is explicit, sent {heard:#?}"
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
    let _server = serve_one(
        ServerMessage::Error {
            error: refusal.clone(),
        },
        |_| {},
        0,
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
    let _server = serve_one(
        ServerMessage::Welcome {
            version: dispatch_proto::VERSION,
            device: "desktop".into(),
        },
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
        0,
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

    // The server hangs up as soon as it has welcomed the client.
    let server = serve_one(
        ServerMessage::Welcome {
            version: dispatch_proto::VERSION,
            device: "desktop".into(),
        },
        |_| {},
        0,
    );

    let client = Client::attach("test").expect("attaching succeeds");
    let _ = server.join();

    let deadline = Instant::now() + PATIENCE;
    while client.is_connected() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!client.is_connected(), "a dropped connection is noticed");
}
