//! Tests for the client half.
//!
//! Each runs against a hand-written server on a real socket rather than the
//! daemon: what is under test is how the client behaves when the other side
//! welcomes it, refuses it, goes away, or comes back, and a fake server can be
//! made to do all four on demand.

use super::*;

use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Keeps listening, and answers a ping as a daemon would.
    Answer,
    /// Keeps the connection open but never says anything again, which is what a
    /// tunnel that died or a wedged peer looks like.
    Silence,
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

/// A running fake server, which stops listening when it is dropped.
///
/// Two of these run in one test, standing in for a daemon that restarted. A
/// server that stayed bound would make the next `Listener::bind` refuse, and a
/// listener parked in `accept` cannot be dropped from outside — so the loop is
/// asked to stand down and then woken by one connection of its own.
///
/// Before this existed the second bind only worked where the platform happened
/// to refuse a connection to the first listener, which macOS does and Linux does
/// not.
struct Server {
    heard: Heard,
    stopped: Arc<AtomicBool>,
    endpoint: PathBuf,
}

impl Server {
    fn snapshot(&self) -> Vec<ClientMessage> {
        self.heard.snapshot()
    }

    fn contains(&self, message: &ClientMessage) -> bool {
        self.heard.contains(message)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        // The loop is parked in `accept`; one connection wakes it, and it
        // checks the flag before serving anyone.
        let _ = dispatch_os::ipc::Connection::connect_to(&self.endpoint);
    }
}

/// Accepts clients, answers each handshake with `answer`, and runs `serve` for
/// the first one.
///
/// Accepts more than one because a client that has been dropped can still have a
/// connection attempt in flight, and a server that took exactly one would then
/// starve the client the test is actually watching.
///
/// Binding happens before the thread starts, so a client connecting immediately
/// finds the endpoint already there.
fn serve_one(
    answer: ServerMessage,
    serve: impl FnOnce(&mut Writer) + Send + 'static,
    after: After,
) -> Server {
    let listener = Listener::bind().expect("binding succeeds");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");
    let heard = Heard::new();
    let recording = heard.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let standing_down = Arc::clone(&stopped);

    type Once = Arc<Mutex<Option<Box<dyn FnOnce(&mut Writer) + Send>>>>;
    let serve: Once = Arc::new(Mutex::new(Some(Box::new(serve))));

    std::thread::spawn(move || {
        loop {
            let Ok(connection) = listener.accept() else {
                break;
            };

            if standing_down.load(Ordering::Relaxed) {
                break;
            }

            let recording = recording.clone();
            let answer = answer.clone();
            let serve = Arc::clone(&serve);

            let handler = std::thread::spawn(move || {
                let (mut reader, writer) = connection.split();
                let mut writer: Writer = Box::new(writer);

                let Ok(hello) = Frame::read::<_, ClientMessage>(&mut reader) else {
                    return;
                };
                recording.push(hello);

                if Frame::write(&mut writer, &answer).is_err() {
                    // A connection that went away before being welcomed: a
                    // client's reconnection attempt crossing with its own drop,
                    // or the liveness probe another bind makes.
                    return;
                }

                // Only the first client gets the scripted output: a test writes
                // its messages once, and a straggler must not consume them.
                let scripted = serve.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(scripted) = scripted {
                    scripted(&mut writer);
                }

                if after == After::HangUp {
                    return;
                }

                while let Ok(message) = Frame::read::<_, ClientMessage>(&mut reader) {
                    if let (After::Answer, ClientMessage::Ping { token }) = (after, &message) {
                        let pong = ServerMessage::Pong { token: *token };
                        if Frame::write(&mut writer, &pong).is_err() {
                            break;
                        }
                    }

                    recording.push(message);
                }
            });

            if after == After::HangUp {
                // The daemon is gone, endpoint included: a test that starts a
                // second one needs this listener out of the way, or binding
                // finds this one still answering.
                //
                // Released before the handler is waited on, not after: a
                // handler that never finishes must not hold the endpoint
                // hostage as well.
                drop(listener);
                let _ = handler.join();
                return;
            }
        }
    });

    Server {
        heard,
        stopped,
        endpoint,
    }
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
    wait_for_from(client, Vec::new(), what, predicate)
}

/// Waits for `predicate`, counting `seen` as already having arrived.
///
/// A test that polled before calling this has to hand what it got back in, or
/// the messages it is waiting for are the ones it threw away: polling never
/// blocks, so whether anything has arrived by then is a race with the reader
/// thread.
fn wait_for_from(
    client: &Client,
    mut seen: Vec<ServerMessage>,
    what: &str,
    predicate: impl Fn(&[ServerMessage]) -> bool,
) -> Vec<ServerMessage> {
    let deadline = Instant::now() + PATIENCE;

    if predicate(&seen) {
        return seen;
    }

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

    let heard = serve_one(welcome(), |_| {}, After::Answer);

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
        After::Answer,
    );

    let client = Client::attach("test").expect("attaching succeeds");

    // Polling never blocks, so this returns whatever has arrived so far —
    // possibly everything the server wrote, since the reader thread is already
    // running. Kept rather than discarded: thrown away, the two messages this
    // test is waiting for could be in it, and the wait below would then only
    // ever see the keepalive traffic.
    let already = client.poll();

    // Keepalive answers are not what this is about, and a slow machine can slip
    // one in between the two messages that are.
    let interesting = |m: &[ServerMessage]| -> Vec<ServerMessage> {
        m.iter()
            .filter(|m| !matches!(m, ServerMessage::Pong { .. }))
            .cloned()
            .collect()
    };

    let seen = wait_for_from(&client, already, "the pane's output and title", |m| {
        interesting(m).len() >= 2
    });
    let seen = interesting(&seen);

    assert!(
        matches!(seen.first(), Some(ServerMessage::PaneOutput { .. })),
        "expected the pane's output first, saw {seen:#?}"
    );
    assert!(
        matches!(seen.get(1), Some(ServerMessage::PaneChanged { .. })),
        "expected the title after it, saw {seen:#?}"
    );
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
    let first = serve_one(welcome(), |_| {}, After::HangUp);
    let client = Client::attach("test").expect("attaching succeeds");
    client.subscribe();

    assert!(
        wait_until(PATIENCE, || !client.is_connected()),
        "the client should notice the daemon going away"
    );

    // A second daemon takes the endpoint, as a restarted one would. The first
    // has to let go of it first, and saying so beats relying on it.
    drop(first);
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
        After::Answer,
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

    let first = serve_one(welcome(), |_| {}, After::HangUp);
    let client = Client::attach("test").expect("attaching succeeds");
    let handle = client.handle();

    assert!(
        wait_until(PATIENCE, || !handle.is_connected()),
        "the handle should report the connection going away"
    );

    let pane = dispatch_core::PaneId::new();
    drop(first);
    let heard = serve_one(welcome(), |_| {}, After::Answer);

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

#[test]
fn a_daemon_that_stops_answering_is_treated_as_gone() {
    // The socket is up as far as this end can tell, and nothing will ever cross
    // it again. Without asking, the client would wait for output forever.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("silent");

    let heard = serve_one(welcome(), |_| {}, After::Silence);

    let client = Client::attach_with(
        "test",
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(400),
        },
    )
    .expect("attaching succeeds");
    assert!(client.is_connected());

    assert!(
        wait_until(PATIENCE, || heard
            .snapshot()
            .iter()
            .any(|m| matches!(m, ClientMessage::Ping { .. }))),
        "a quiet daemon should be asked whether it is there, sent {:#?}",
        heard.snapshot()
    );

    assert!(
        wait_until(PATIENCE, || !client.is_connected()),
        "a daemon that never answers should be given up on"
    );
}

#[test]
fn a_daemon_that_answers_is_left_alone() {
    // The other half of the same rule: silence from the client's side is not
    // evidence of anything, so a daemon with nothing to say must not be dropped.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("answering");

    let heard = serve_one(welcome(), |_| {}, After::Answer);

    let client = Client::attach_with(
        "test",
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(400),
        },
    )
    .expect("attaching succeeds");

    // Several silence windows, so a client that was going to give up has had
    // every chance to.
    std::thread::sleep(Duration::from_millis(1200));

    assert!(
        client.is_connected(),
        "an answering daemon should still be attached"
    );
    assert_eq!(
        client.generation(),
        1,
        "and should not have been reconnected"
    );
    assert!(
        heard
            .snapshot()
            .iter()
            .any(|m| matches!(m, ClientMessage::Ping { .. })),
        "the client should have asked at least once"
    );
}
