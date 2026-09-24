//! Tests for the client half.
//!
//! Each runs against a hand-written server on a real socket rather than the
//! daemon: what is under test is how the client behaves when the other side
//! welcomes it, refuses it, goes away, or comes back, and a fake server can be
//! made to do all four on demand.

use super::*;

use std::path::Path;
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

/// Points the environment at a directory for as long as this is held,
/// without removing it on drop.
///
/// `Endpoint` deletes its directory when it goes, which is right for a guard
/// that owns a socket for the whole test but wrong here: this is used only to
/// get a daemon bound *somewhere other than* `Endpoint`'s directory, and that
/// daemon keeps serving — and the socket file keeps needing to exist — after
/// this guard has restored the variable and gone out of scope.
struct Elsewhere {
    previous: Option<std::ffi::OsString>,
}

impl Elsewhere {
    fn new(dir: &Path) -> Self {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).expect("temp dir is writable");

        let previous = std::env::var_os(dispatch_os::paths::CONFIG_DIR_ENV);

        // SAFETY: as on `Endpoint`.
        unsafe { std::env::set_var(dispatch_os::paths::CONFIG_DIR_ENV, dir) };

        Self { previous }
    }
}

impl Drop for Elsewhere {
    fn drop(&mut self) {
        // SAFETY: as on `Endpoint`.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(dispatch_os::paths::CONFIG_DIR_ENV, value),
                None => std::env::remove_var(dispatch_os::paths::CONFIG_DIR_ENV),
            }
        }
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
///
/// Dropping one also waits for its accepting thread, which closes the listener
/// on its way out. Waking the thread is not enough on its own: the next bind
/// can run before that thread gets round to letting go, find the old listener
/// still answering, and report a daemon already running.
struct Server {
    heard: Heard,
    stopped: Arc<AtomicBool>,
    endpoint: PathBuf,
    serving: Option<std::thread::JoinHandle<()>>,
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
        let wake = dispatch_os::ipc::Connection::connect_to(&self.endpoint);

        // Joined only when the wake got through, as `Listener`'s own drop
        // does. One that could not connect found nothing listening -- a
        // hung-up server has already dropped its listener -- so the endpoint
        // is free, and a join nothing will wake could hang. One that got
        // through always ends: an answering or silent loop breaks on it and
        // drops the listener, and a hung-up server's thread only ever waits
        // for its own listener's drop and its one handler, which runs no
        // scripted output that blocks.
        //
        // The wake is held open until then: Windows takes a connection that
        // closed before the accept reached it for a probe, and would not
        // deliver it.
        if let Some(serving) = self.serving.take()
            && wake.is_ok()
        {
            let _ = serving.join();
        }
        drop(wake);
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

    let serving = std::thread::spawn(move || {
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
                let mut writer: Writer = writer;

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
        serving: Some(serving),
    }
}

/// Stands a fake daemon up at `dir`'s endpoint, leaving `DISPATCH_CONFIG_DIR`
/// pointed elsewhere once it returns.
///
/// `serve_one` binds wherever the environment currently points, because that
/// is what `Listener::bind` and `dispatch_os::ipc::endpoint` both read. Proving
/// that `attach_at` honours the endpoint it is *given*, rather than quietly
/// falling back to the environment, means the daemon it reaches must be
/// listening somewhere that variable does not point at when the client
/// attaches — so this points it at `dir` only for the moment of binding, then
/// restores it.
fn serve_one_at(
    dir: &Path,
    answer: ServerMessage,
    serve: impl FnOnce(&mut Writer) + Send + 'static,
    after: After,
) -> Server {
    let _guard = Elsewhere::new(dir);
    serve_one(answer, serve, after)
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

/// `bytes` as a `printf` format string.
///
/// Three-digit octal escapes rather than `\xHH`: `printf` must understand
/// them under POSIX, and Ubuntu's `/bin/sh` is dash, whose `printf` has no
/// `\x` at all -- it printed the escapes as text and the handshake never
/// completed.
#[cfg(unix)]
fn octal(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:03o}")).collect()
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
fn a_dropped_fake_daemon_has_let_go_of_its_endpoint() {
    // The two tests above stand a second daemon up the moment the first is
    // dropped. A first one still closing its listener when `drop` returned
    // answered the second one's bind, which then reported AlreadyRunning.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("let-go");

    for after in [After::HangUp, After::Answer, After::Silence] {
        for _ in 0..20 {
            drop(serve_one(welcome(), |_| {}, after));
            let rebound = Listener::bind();
            assert!(
                rebound.is_ok(),
                "a dropped {after:?} server still held the endpoint: {:?}",
                rebound.err()
            );
        }
    }
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

#[test]
fn a_client_attaches_to_an_endpoint_it_is_given() {
    // Federation dials one socket per machine, so the endpoint cannot come
    // from this process's own configuration. Proving that means the daemon
    // has to be reachable only through the argument: `DISPATCH_CONFIG_DIR`
    // is left pointed at a directory with nothing listening in it, so a
    // client that ignored `attach_at`'s endpoint and fell back to the
    // environment, as `attach_with_as` does, would fail to connect.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("attach-at-wrong");
    let wrong = dispatch_os::ipc::endpoint().expect("the endpoint resolves");

    let elsewhere = std::env::temp_dir().join(format!(
        "dispatch-client-{}-attach-at-right",
        std::process::id()
    ));
    let heard = serve_one_at(&elsewhere, welcome(), |_| {}, After::Answer);
    let right = elsewhere.join("dispatchd.sock");
    assert_ne!(wrong, right, "the daemon must not be at the wrong endpoint");

    let client = Client::attach_at(Role::Interface, "test", Liveness::default(), right)
        .expect("the daemon is listening at the endpoint given, not the environment's");

    assert!(client.is_connected());

    drop(client);
    drop(heard);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
#[cfg(unix)]
fn a_client_dialling_a_command_reports_what_the_command_said() {
    // The dial plumbing, without a bridge: a command that refuses and exits
    // must fail the attach with its own words rather than a bare timeout.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let error = Client::attach_over(
        Role::Interface,
        "test",
        Liveness::default(),
        std::ffi::OsString::from("sh"),
        vec![
            std::ffi::OsString::from("-c"),
            std::ffi::OsString::from("echo 'dispatchd: command not found' 1>&2; exit 127"),
        ],
    )
    .expect_err("the command refuses");

    assert!(
        error.to_string().contains("command not found"),
        "the command's own words reach the caller: {error}"
    );
}

#[test]
#[cfg(unix)]
fn a_command_that_dies_is_respawned() {
    // The supervisor reconnects by repeating the dial. For a command that
    // means running it again, which is the whole reason `Dial` is remembered
    // rather than resolved once.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let dir = std::env::temp_dir().join(format!("dispatch-client-{}-respawn", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    let counter = dir.join("runs");

    // A canned Hello, written exactly as `Frame` writes one, so the client's
    // handshake completes without a daemon behind it. Then the command exits,
    // which is the event under test: the reader sees EOF, the connection is
    // lost, and the supervisor has to dial again.
    let mut encoded = Vec::new();
    Frame::write(&mut encoded, &welcome()).expect("writing succeeds");
    let escaped = octal(&encoded);

    let program = std::ffi::OsString::from("sh");
    let args = vec![
        std::ffi::OsString::from("-c"),
        std::ffi::OsString::from(format!(
            "echo ran >> {}; printf '{}'; sleep 0.2",
            counter.display(),
            escaped
        )),
    ];

    let client = Client::attach_over(Role::Interface, "test", Liveness::default(), program, args)
        .expect("the command answers the handshake");

    let deadline = Instant::now() + PATIENCE;
    while client.generation() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        client.generation() >= 2,
        "the supervisor dialled again after the command exited"
    );
    assert!(
        std::fs::read_to_string(&counter)
            .unwrap_or_default()
            .lines()
            .count()
            >= 2,
        "the command ran more than once"
    );

    drop(client);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reports whether `pid` names a live process.
///
/// The same probe `dispatch-os`'s reaping test uses: a signal of zero is
/// delivered to nobody and only asks whether the pid exists.
#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing and only performs the kernel's own
    // existence check, which is safe to ask about any pid.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// A directory of this test's own, emptied first.
#[cfg(unix)]
fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dispatch-client-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    dir
}

/// The first pid a shell recorded, once it has recorded one.
///
/// Polled rather than read once: the shell forks and writes on its own
/// schedule, and a test that read too early would prove nothing.
#[cfg(unix)]
fn first_recorded_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Some(first) = contents.lines().next()
            && let Ok(pid) = first.trim().parse::<u32>()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the shell never recorded a pid in {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A shell that forks a child, records its pid, and then never speaks.
///
/// The grandchild is what makes an assertion about it worth making: killing
/// the shell alone would leave the process it forked running and holding the
/// transport's pipes, which is exactly what `ssh` does.
#[cfg(unix)]
fn mute_command(pid_file: &Path, then: &str) -> (OsString, Vec<OsString>) {
    (
        OsString::from("sh"),
        vec![
            OsString::from("-c"),
            OsString::from(format!(
                "sleep 10 & echo $! >> {}; {then}sleep 10",
                pid_file.display()
            )),
        ],
    )
}

#[test]
#[cfg(unix)]
fn a_dial_that_never_answers_leaves_no_process_behind() {
    // `ssh` prompting for a passphrase on the terminal rather than stdin, or
    // a host that accepts the connection and then stalls: the command runs,
    // says nothing, and never exits. The handshake gives up -- and the
    // supervisor repeats the dial every couple of seconds, so a command left
    // running is not one orphan but one per attempt, forever.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let dir = scratch("mute");
    let pid_file = dir.join("grandchild.pid");
    let (program, args) = mute_command(&pid_file, "");

    // Matched rather than `expect_err`: what a successful dial hands back is
    // a pair of boxed streams, which cannot be printed.
    let dialled = connect_within(
        "test",
        Role::Interface,
        &Dial::Command { program, args },
        Duration::from_millis(500),
    );
    let Err(error) = dialled else {
        panic!("a command that never speaks cannot be handshaken");
    };

    assert!(
        matches!(error, ClientError::Handshake(_)),
        "expected the handshake to time out, got {error:?}"
    );

    let grandchild = first_recorded_pid(&pid_file);

    // Polled, as every sibling test polls: the tree has been signalled, but a
    // killed process answers `kill(pid, 0)` until whoever inherited it reaps
    // it, and on macOS that is launchd, on its own schedule.
    let deadline = Instant::now() + PATIENCE;
    while pid_is_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !pid_is_alive(grandchild),
        "the dial's process tree outlived the handshake that walked away from it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn a_connection_given_up_on_leaves_no_process_behind() {
    // Liveness declares a silent connection dead, but the reader thread that
    // owns the command is still parked on a peer that will never speak, so
    // nothing it holds can be dropped. Over SSH that leaves one `ssh` alive
    // for the fifteen minutes the kernel takes to give up on the TCP
    // connection -- while the supervisor has already dialled a second one.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let dir = scratch("wedged");
    let pid_file = dir.join("grandchildren.pid");

    // A canned Welcome, written exactly as `Frame` writes one, so the
    // handshake completes and the connection is up before it goes quiet.
    let mut encoded = Vec::new();
    Frame::write(&mut encoded, &welcome()).expect("writing succeeds");
    let escaped = octal(&encoded);

    let (program, args) = mute_command(&pid_file, &format!("printf '{escaped}'; "));

    let client = Client::attach_over(
        Role::Interface,
        "test",
        // Far shorter than a real client's patience, which is tens of
        // seconds: what is under test is what happens once silence has been
        // declared, not how long it takes to declare it.
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(200),
        },
        program,
        args,
    )
    .expect("the command answers the handshake");

    let grandchild = first_recorded_pid(&pid_file);
    assert!(
        pid_is_alive(grandchild),
        "the dial's process tree should be running while the connection is up"
    );

    let deadline = Instant::now() + PATIENCE;
    while pid_is_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        !pid_is_alive(grandchild),
        "a connection declared dead left its command running"
    );

    drop(client);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn a_hint_that_arrives_late_still_reaches_the_caller() {
    // The stderr drain runs on a thread of its own, so a command that dies
    // the instant it starts can lose the race: stdout closes, the handshake
    // fails, and the reason is still in flight. Closing stdout first makes
    // that ordering certain rather than occasional -- which is the same
    // ordering that makes reading the hint immediately a flaky test.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let error = Client::attach_over(
        Role::Interface,
        "test",
        Liveness::default(),
        OsString::from("sh"),
        vec![
            OsString::from("-c"),
            OsString::from(
                "exec 1>&-; sleep 0.02; echo 'dispatchd: command not found' 1>&2; exit 127",
            ),
        ],
    )
    .expect_err("the command refuses");

    assert!(
        error.to_string().contains("command not found"),
        "the command's own words reach the caller even when they arrive last: {error}"
    );
}

#[test]
fn a_command_dial_is_given_longer_to_answer_than_a_socket() {
    // The two dials are not the same question. A socket is a daemon on this
    // machine: it answers at once or it is broken. A command may be
    // `ssh host dispatchd --stdio`, which has a network, an authentication,
    // a remote exec and a possible cold daemon start in front of it -- and
    // `--stdio` alone waits ten seconds for that daemon to listen.
    let socket = patience_for(&Dial::Endpoint(PathBuf::from("/tmp/dispatchd.sock")));
    let command = patience_for(&Dial::Command {
        program: "ssh".into(),
        args: vec!["host".into(), "dispatchd".into(), "--stdio".into()],
    });

    assert_eq!(
        socket, SOCKET_HANDSHAKE_TIMEOUT,
        "a socket keeps the short budget"
    );
    assert!(
        command >= Duration::from_secs(30),
        "a cold `--stdio` start alone may take ten seconds, got {command:?}"
    );
}

#[test]
fn a_dialled_client_starts_down_and_connects_once_a_daemon_answers() {
    // A machine asleep when Dispatch starts must still join when it wakes.
    // Before `Client::dial` there was no client to keep trying.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("dial-later");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Endpoint(endpoint),
    );

    assert_eq!(client.generation(), 0, "nothing has connected yet");
    assert!(!client.is_connected());
    assert_eq!(client.device(), "");
    assert!(
        wait_until(PATIENCE, || client.last_error().is_some()),
        "a failed first dial is remembered for the interface to show"
    );

    client.subscribe();
    let server = serve_one(welcome(), |_| {}, After::Answer);

    assert!(
        wait_until(PATIENCE, || client.generation() >= 1),
        "the supervisor keeps dialling until a daemon answers"
    );
    assert_eq!(client.device(), "desktop");
    assert!(
        client.last_error().is_none(),
        "connected, so nothing is wrong"
    );
    assert!(
        wait_until(PATIENCE, || server.contains(&ClientMessage::Subscribe)),
        "a subscription asked for before the first connection goes out on it"
    );

    drop(client);
    drop(server);
}

#[test]
fn backoff_is_measured_against_what_is_being_dialled() {
    let socket = Dial::Endpoint(PathBuf::from("/tmp/dispatchd.sock"));
    let command = Dial::Command {
        program: "ssh".into(),
        args: vec!["tower".into()],
    };

    assert_eq!(
        retry_for(&socket),
        (FIRST_RETRY, MAX_RETRY),
        "a local daemon is unchanged"
    );
    assert_eq!(
        retry_for(&command),
        (Duration::from_secs(1), Duration::from_secs(30)),
        "every attempt at a command is a new ssh"
    );

    let mut gap = retry_for(&command).0;
    for _ in 0..10 {
        gap = next_backoff(gap, &command);
    }
    assert_eq!(
        gap,
        Duration::from_secs(30),
        "doubling stops at the ceiling"
    );
    assert_eq!(next_backoff(MAX_RETRY, &socket), MAX_RETRY);
}

#[test]
#[cfg(unix)]
fn a_command_that_keeps_failing_is_not_respawned_every_moment() {
    // A machine asleep for an hour must not cost an ssh every two seconds.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("backoff");
    let counter = dir.join("runs");

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Command {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from(format!("echo ran >> {}; exit 1", counter.display())),
            ],
        },
    );

    std::thread::sleep(Duration::from_millis(1500));
    let runs = std::fs::read_to_string(&counter)
        .unwrap_or_default()
        .lines()
        .count();

    assert!(
        (1..=2).contains(&runs),
        "one attempt at once and one a second later, not {runs}"
    );

    drop(client);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn a_client_dropped_while_backing_off_dials_no_more() {
    // The backoff sleep is where a dialling client spends nearly all its
    // time. Dropped there, it must not wake up and run ssh one last time for
    // nobody.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("drop-backoff");
    let counter = dir.join("runs");
    let runs = || {
        std::fs::read_to_string(&counter)
            .unwrap_or_default()
            .lines()
            .count()
    };

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Command {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from(format!("echo ran >> {}; exit 1", counter.display())),
            ],
        },
    );

    assert!(
        wait_until(PATIENCE, || client.last_error().is_some()),
        "the first dial fails at once"
    );
    // Well into the first second-long backoff.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(runs(), 1);
    drop(client);

    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(runs(), 1, "no dial after the client was dropped");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn a_command_failure_is_remembered_in_its_own_words() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Command {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from("echo 'Permission denied (publickey).' 1>&2; exit 255"),
            ],
        },
    );

    assert!(
        wait_until(PATIENCE, || client
            .last_error()
            .is_some_and(|error| error.contains("Permission denied"))),
        "the command's own words are what the user needs to see: {:?}",
        client.last_error()
    );
}

#[test]
#[cfg(unix)]
fn a_client_dropped_mid_dial_leaves_nothing_behind() {
    // The supervisor checks `closed` at the top of its loop. A dial that
    // completes after the client has gone recorded a pid nobody would ever
    // take. The grandchild outlives `PATIENCE` on its own, so only a reap
    // can end it in time.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("drop-mid-dial");
    let pid_file = dir.join("grandchild.pid");

    let mut encoded = Vec::new();
    Frame::write(&mut encoded, &welcome()).expect("writing succeeds");
    let escaped = octal(&encoded);

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Command {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from(format!(
                    "sleep 30 & echo $! >> {}; sleep 0.3; printf '{escaped}'; sleep 30",
                    pid_file.display()
                )),
            ],
        },
    );

    let grandchild = first_recorded_pid(&pid_file);
    drop(client);

    let deadline = Instant::now() + PATIENCE;
    while pid_is_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        !pid_is_alive(grandchild),
        "a dial that finished after its client was dropped left its command running"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_connection_given_up_on_cannot_take_its_successor_down() {
    // Liveness gives up on a silent peer, but the reader parked on that
    // peer's socket is still there: nothing wakes it until the peer closes.
    // When it finally does, that stale reader reports its connection lost --
    // and `lost` acted on whichever connection was current, so it tore down
    // the one the supervisor had already dialled to replace it.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("stale-reader");

    let listener = Listener::bind().expect("binding succeeds");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");
    let (release, released) = std::sync::mpsc::channel::<()>();

    let server = std::thread::spawn(move || {
        // The first connection is welcomed and then says nothing, held open
        // until the test lets go of it.
        let (mut first_reader, mut first_writer) =
            listener.accept().expect("the first dial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut first_reader);
        Frame::write(&mut first_writer, &welcome()).expect("the welcome goes out");

        // The second is the replacement, and answers like a live daemon.
        let (mut second_reader, mut second_writer) =
            listener.accept().expect("the redial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut second_reader);
        Frame::write(&mut second_writer, &welcome()).expect("the welcome goes out");
        std::thread::spawn(move || {
            while let Ok(message) = Frame::read::<_, ClientMessage>(&mut second_reader) {
                if let ClientMessage::Ping { token } = message
                    && Frame::write(&mut second_writer, &ServerMessage::Pong { token }).is_err()
                {
                    return;
                }
            }
        });

        let _ = released.recv();
        drop(first_reader);
        drop(first_writer);
        // Kept alive so the endpoint stays bound for the rest of the test.
        listener
    });

    let client = Client::attach_at(
        Role::Interface,
        "test",
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(300),
        },
        endpoint,
    )
    .expect("the first connection is welcomed");

    assert!(
        wait_until(PATIENCE, || client.generation() == 2
            && client.is_connected()),
        "the silent connection is given up on and replaced"
    );

    // The old peer finally closes, and the reader parked on it wakes up.
    release.send(()).expect("the server is waiting");

    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        assert!(
            client.is_connected() && client.generation() == 2,
            "the stale reader took down the connection that replaced it \
             (connected: {}, generation: {})",
            client.is_connected(),
            client.generation()
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(client);
    drop(server.join());
}

#[test]
fn a_replaced_connection_cannot_speak_for_its_successor() {
    // The other half of a stale reader: its peer, given up on for going
    // silent, starts talking again after the replacement is up. What it says
    // describes a connection that no longer exists, and must not reach the
    // queue the interface reads as the replacement's.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("stale-speaker");

    let listener = Listener::bind().expect("binding succeeds");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");
    let (speak, spoken) = std::sync::mpsc::channel::<()>();
    let stale = ServerMessage::Error {
        error: dispatch_proto::ProtocolError::Other("from a replaced connection".into()),
    };
    let from_old_peer = stale.clone();

    let server = std::thread::spawn(move || {
        // The first connection is welcomed and then goes quiet.
        let (mut first_reader, mut first_writer) =
            listener.accept().expect("the first dial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut first_reader);
        Frame::write(&mut first_writer, &welcome()).expect("the welcome goes out");

        // The replacement answers like a live daemon.
        let (mut second_reader, mut second_writer) =
            listener.accept().expect("the redial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut second_reader);
        Frame::write(&mut second_writer, &welcome()).expect("the welcome goes out");
        std::thread::spawn(move || {
            while let Ok(message) = Frame::read::<_, ClientMessage>(&mut second_reader) {
                if let ClientMessage::Ping { token } = message
                    && Frame::write(&mut second_writer, &ServerMessage::Pong { token }).is_err()
                {
                    return;
                }
            }
        });

        // Then the old peer wakes up and speaks.
        let _ = spoken.recv();
        let _ = Frame::write(&mut first_writer, &from_old_peer);
        (listener, first_reader, first_writer)
    });

    let client = Client::attach_at(
        Role::Interface,
        "test",
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(300),
        },
        endpoint,
    )
    .expect("the first connection is welcomed");

    assert!(
        wait_until(PATIENCE, || client.generation() == 2
            && client.is_connected()),
        "the silent connection is given up on and replaced"
    );
    let _ = client.poll();

    speak.send(()).expect("the server is waiting");

    let deadline = Instant::now() + Duration::from_secs(1);
    let mut heard = Vec::new();
    while Instant::now() < deadline {
        heard.extend(client.poll());
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        !heard.contains(&stale),
        "a replaced connection's message reached the queue: {heard:?}"
    );

    drop(client);
    drop(server.join());
}

#[test]
fn a_daemon_speaking_another_major_version_is_refused_by_the_client() {
    // The daemon checks the client's version; until now the client never
    // checked the daemon's, and would read every frame of a protocol it
    // does not speak.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("future-daemon");

    let _server = serve_one(
        ServerMessage::Welcome {
            version: Version {
                major: 99,
                minor: 0,
            },
            device: "future".into(),
        },
        |_| {},
        After::Silence,
    );

    let error = Client::attach_with("test", Liveness::default())
        .expect_err("a daemon from another major version is refused");

    assert!(
        matches!(
            error,
            ClientError::Refused(ProtocolError::IncompatibleVersion { .. })
        ),
        "expected an incompatible version, got {error:?}"
    );
}

/// A message far bigger than any pipe or socket buffer, so writing it
/// blocks until the peer reads.
fn huge() -> ClientMessage {
    ClientMessage::OpenProject {
        root: PathBuf::from("x".repeat(4 * 1024 * 1024)),
    }
}

#[test]
#[cfg(unix)]
fn a_peer_that_never_reads_is_given_up_on_despite_a_stuck_write() {
    // The audit's probe: welcomed, then the peer neither reads nor speaks.
    // The big write blocked holding the lock the supervisor needed, and the
    // client went on reporting itself connected long past its silence limit.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut encoded = Vec::new();
    Frame::write(&mut encoded, &welcome()).expect("writing succeeds");

    let client = Client::attach_over(
        Role::Interface,
        "test",
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(300),
        },
        OsString::from("sh"),
        vec![
            OsString::from("-c"),
            OsString::from(format!("printf '{}'; sleep 30", octal(&encoded))),
        ],
    )
    .expect("the command answers the handshake");

    client.send(huge());

    // Given up on and redialled -- the command answers again -- well inside
    // the thirty seconds the first peer would otherwise have held it.
    assert!(
        wait_until(Duration::from_secs(5), || client.generation() >= 2),
        "the client never gave up on a peer it could not write to"
    );

    let started = Instant::now();
    drop(client);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "dropping the client took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_peer_that_talks_but_never_reads_is_given_up_on() {
    // Never silent -- it answers on its own every twenty milliseconds -- so
    // only a deadline on the write itself can notice that nothing sent
    // reaches it. The replacement then answers like a live daemon, and the
    // old connection's stuck write, failing once it is closed, must not take
    // the replacement down.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("talks-only");

    let listener = Listener::bind().expect("binding succeeds");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");
    let (stop, stopped) = std::sync::mpsc::channel::<()>();

    let server = std::thread::spawn(move || {
        let (mut first_reader, mut first_writer) =
            listener.accept().expect("the first dial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut first_reader);
        Frame::write(&mut first_writer, &welcome()).expect("the welcome goes out");
        std::thread::spawn(move || {
            while Frame::write(&mut first_writer, &ServerMessage::Pong { token: 0 }).is_ok() {
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        let (mut second_reader, mut second_writer) =
            listener.accept().expect("the redial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut second_reader);
        Frame::write(&mut second_writer, &welcome()).expect("the welcome goes out");
        std::thread::spawn(move || {
            while let Ok(message) = Frame::read::<_, ClientMessage>(&mut second_reader) {
                if let ClientMessage::Ping { token } = message
                    && Frame::write(&mut second_writer, &ServerMessage::Pong { token }).is_err()
                {
                    return;
                }
            }
        });

        let _ = stopped.recv();
        (listener, first_reader)
    });

    let client = Client::attach_at(
        Role::Interface,
        "test",
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(300),
        },
        endpoint,
    )
    .expect("the first connection is welcomed");

    client.send(huge());

    assert!(
        wait_until(PATIENCE, || client.generation() == 2
            && client.is_connected()),
        "a peer that never read was not given up on"
    );

    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        assert!(
            client.is_connected() && client.generation() == 2,
            "the old connection's stuck write took down its replacement"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(client);
    let _ = stop.send(());
    drop(server.join());
}

#[test]
fn a_socket_dial_that_is_given_up_on_lets_its_thread_go() {
    // A peer that accepts and never answers the handshake. The dial gives
    // up at its patience; the thread it left parked in the read used to stay
    // parked until the peer closed -- one per retry, forever.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("mute-socket");

    let listener = Listener::bind().expect("binding succeeds");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");
    let (closed, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut reader, _writer) = listener.accept().expect("the dial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut reader);
        // Now say nothing, and report when the client has let go.
        let mut byte = [0u8; 1];
        while reader.read(&mut byte).is_ok_and(|n| n > 0) {}
        let _ = closed.send(());
        drop(listener);
    });

    let dialled = connect_within(
        "test",
        Role::Interface,
        &Dial::Endpoint(endpoint),
        Duration::from_millis(300),
    );
    assert!(matches!(dialled, Err(ClientError::Handshake(_))));

    assert!(
        ended.recv_timeout(PATIENCE).is_ok(),
        "the abandoned handshake still holds its connection open"
    );
}

/// A writer whose every write blocks until the test lets it go, and then
/// fails: a write to a peer that never reads, until the connection is closed.
struct Stuck {
    /// Told each time a write begins.
    began: Sender<()>,
    /// Dropped to let the write go.
    release: Receiver<()>,
}

impl Write for Stuck {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        let _ = self.began.send(());
        let _ = self.release.recv();
        Err(std::io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A stuck writer, word of each write it begins, and what lets it go.
fn stuck() -> (Stuck, Receiver<()>, Sender<()>) {
    let (began, beginning) = channel();
    let (release, released) = channel();
    (
        Stuck {
            began,
            release: released,
        },
        beginning,
        release,
    )
}

/// A wire up on connection 1, writing to `writer`, with nothing to close.
fn wired(liveness: Liveness, writer: impl Write + Send + 'static) -> Arc<Wire> {
    let wire = Wire::new(
        Role::Interface,
        "test",
        liveness,
        Dial::Endpoint(PathBuf::new()),
    );
    *wire.line.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Arc::new(Line::new(1, Box::new(writer), Closer::default())));
    wire.generation.store(1, Ordering::Relaxed);
    wire.connected.store(true, Ordering::Relaxed);
    Arc::new(wire)
}

#[test]
fn checking_liveness_never_waits_on_the_connection() {
    // Both deadlines -- the stuck write and the silence -- are enforced by
    // the thread that checks liveness, so that thread must never be the one
    // a write holds. A ping written by it, even one that only started when
    // nothing else was writing, blocked it in a write a peer that never
    // reads would hold for good, and then nothing enforced either.
    let (writer, _began, release) = stuck();
    let wire = wired(
        Liveness {
            interval: Duration::from_millis(10),
            silence: Duration::from_secs(60),
        },
        writer,
    );

    // Quiet for longer than the interval, so a question is due.
    std::thread::sleep(Duration::from_millis(50));

    let (outbox, queued) = channel();
    let (done, checked) = channel();
    let checking = Arc::clone(&wire);
    std::thread::spawn(move || {
        check_liveness(&checking, &outbox);
        let _ = done.send(());
    });

    assert!(
        checked.recv_timeout(Duration::from_secs(2)).is_ok(),
        "the liveness check is stuck in a write the peer never takes"
    );
    assert!(
        matches!(queued.try_recv(), Ok(ClientMessage::Ping { .. })),
        "the question is still asked, through the queue"
    );

    drop(release);
}

#[test]
fn a_stale_write_that_fails_cannot_take_its_successor_down() {
    // A write stuck on a connection that has been given up on fails only
    // once that connection is closed -- by which time its replacement may be
    // up. The failure has to end the connection it was written to, not
    // whichever one is current.
    let (writer, began, release) = stuck();
    let wire = wired(Liveness::default(), writer);

    let (done, wrote) = channel();
    let writing = Arc::clone(&wire);
    std::thread::spawn(move || {
        let _ = done.send(writing.write(&ClientMessage::Subscribe));
    });
    began
        .recv_timeout(PATIENCE)
        .expect("the write to connection 1 is under way");

    // Replaced while that write is still stuck, as the supervisor does.
    {
        let mut slot = wire.line.lock().unwrap_or_else(|e| e.into_inner());
        wire.generation.store(2, Ordering::Relaxed);
        *slot = Some(Arc::new(Line::new(
            2,
            Box::new(std::io::sink()),
            Closer::default(),
        )));
        wire.connected.store(true, Ordering::Relaxed);
    }

    // The old write fails, as closing its connection makes it.
    drop(release);
    assert_eq!(
        wrote.recv_timeout(PATIENCE),
        Ok(false),
        "the stuck write reports that its connection is gone"
    );

    assert_eq!(
        wire.current().map(|line| line.generation),
        Some(2),
        "the old connection's failed write took its replacement out"
    );
    assert!(
        wire.connected.load(Ordering::Relaxed),
        "the old connection's failed write marked its replacement down"
    );
}

#[test]
#[cfg(unix)]
fn a_dial_given_up_on_before_it_opened_anything_is_closed_once_it_does() {
    // The caller can stop waiting before the dialling thread has recorded
    // what it opened -- the opening is what took too long. What it opens
    // after that is nobody's to close unless recording it closes it.
    let dialling = Dialling::default();
    dialling.abandon();

    let connection = Connection::over_command(
        std::ffi::OsStr::new("sh"),
        &[OsString::from("-c"), OsString::from("sleep 30")],
    )
    .expect("sh exists");
    let closer = connection.closer();
    let (mut reader, _writer) = connection.split();
    let (ended, read_ended) = channel();
    std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        while reader.read(&mut byte).is_ok_and(|n| n > 0) {}
        let _ = ended.send(());
    });

    dialling.record(closer);

    assert!(
        read_ended.recv_timeout(PATIENCE).is_ok(),
        "a dial recorded after it was abandoned is still open"
    );
}
