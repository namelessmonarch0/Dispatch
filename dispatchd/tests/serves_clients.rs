//! End-to-end: a real client, a real socket, a real `dispatchd`.
//!
//! The daemon's decisions are tested in `dispatch-daemon` and the transport in
//! `dispatch-os`. What neither covers is the binary: that it comes up, binds,
//! and carries a pane from a spawn request to the bytes the child printed. This
//! runs the built `dispatchd` and talks to it over the socket.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use dispatch_os::ipc::Connection;
use dispatch_proto::{ClientMessage, Frame, ServerMessage};

/// How long to wait for the daemon to do anything before failing.
const PATIENCE: Duration = Duration::from_secs(20);

/// Points this process and the daemon at a configuration directory of the
/// test's own, so neither touches the developer's harnesses or socket.
///
/// The variable is process-wide, so tests that use it run one at a time.
struct Endpoint {
    dir: PathBuf,
    previous: Option<std::ffi::OsString>,
}

impl Endpoint {
    fn new(label: &str) -> Self {
        // Under the system temp directory rather than a deeper path: a Unix
        // socket address is limited to about a hundred bytes.
        let dir = std::env::temp_dir().join(format!("dispatchd-it-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("harnesses")).expect("temp dir is writable");

        // A harness of the test's own, so what a pane runs is a plain shell
        // rather than whichever agents happen to be installed. The `[task]`
        // form is what lets a delegation request against it succeed: without
        // one, the daemon refuses before ever asking anybody.
        let shell = if cfg!(windows) {
            "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"cmd.exe\"\n\n[task]\nargs = [\"/c\", \"{task}\"]\n"
        } else {
            "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"sh\"\n\n[task]\nargs = [\"-c\", \"{task}\"]\n"
        };
        std::fs::write(dir.join("harnesses").join("shell.toml"), shell)
            .expect("temp dir is writable");

        let previous = std::env::var_os(dispatch_os::paths::CONFIG_DIR_ENV);

        // SAFETY: the tests that move this variable are serialised by the
        // mutex below, and nothing else in this binary reads it concurrently.
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

/// A running daemon, killed when the test ends however it ends.
struct RunningDaemon(Child);

impl RunningDaemon {
    fn start(config_dir: &Path, project: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_dispatchd"))
            .arg(project)
            .env(dispatch_os::paths::CONFIG_DIR_ENV, config_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the built dispatchd runs");

        Self(child)
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Connects, retrying until the daemon has bound the endpoint.
fn connect() -> Connection {
    let deadline = Instant::now() + PATIENCE;

    loop {
        match Connection::connect() {
            Ok(connection) => return connection,
            Err(error) if Instant::now() >= deadline => {
                panic!("the daemon never started listening: {error}")
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Reads frames on a thread, so a message that never arrives fails the test
/// instead of blocking it forever.
fn read_in_background(mut reader: impl Read + Send + 'static) -> Receiver<ServerMessage> {
    let (sender, receiver) = channel();

    std::thread::spawn(move || {
        while let Ok(message) = Frame::read::<_, ServerMessage>(&mut reader) {
            if sender.send(message).is_err() {
                break;
            }
        }
    });

    receiver
}

/// Collects messages until `predicate` holds, or fails.
fn wait_for(
    inbox: &Receiver<ServerMessage>,
    what: &str,
    predicate: impl Fn(&[ServerMessage]) -> bool,
) -> Vec<ServerMessage> {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();

    while Instant::now() < deadline {
        if predicate(&seen) {
            return seen;
        }

        match inbox.recv_timeout(Duration::from_millis(200)) {
            Ok(message) => seen.push(message),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if predicate(&seen) {
        return seen;
    }
    panic!("timed out waiting for {what}; saw {seen:#?}");
}

/// Lines to type at a pane's shell so that its answer proves the shell ran them.
///
/// A terminal echoes what is typed, so a line containing the marker it looks for
/// would prove nothing. A POSIX shell can compute the marker in one line; a
/// `cmd.exe` expansion has no arithmetic, so it sets a variable on one line and
/// expands it on the next — which it can only do having run the first.
///
/// Submitted with a carriage return, which is what a key press sends, and what
/// ConPTY needs.
fn prove_the_shell_ran(marker: &str) -> Vec<Vec<u8>> {
    if cfg!(windows) {
        vec![
            b"set answer=42\r".to_vec(),
            format!("echo {marker}-%answer%\r").into_bytes(),
        ]
    } else {
        vec![format!("echo {marker}-$((6*7))\r").into_bytes()]
    }
}

/// Connects, says hello, and subscribes, returning the queue and the writer.
fn attach() -> (Receiver<ServerMessage>, impl std::io::Write) {
    attach_as(dispatch_proto::Role::Interface)
}

/// Connects, says hello with the given role, and subscribes, returning the
/// queue and the writer.
fn attach_as(role: dispatch_proto::Role) -> (Receiver<ServerMessage>, impl std::io::Write) {
    let (reader, mut writer) = connect().split();
    let inbox = read_in_background(reader);

    Frame::write(
        &mut writer,
        &ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "integration test".into(),
            role,
        },
    )
    .expect("writing succeeds");
    let welcome = wait_for(&inbox, "a welcome", |m| !m.is_empty());
    assert!(
        matches!(welcome.first(), Some(ServerMessage::Welcome { .. })),
        "expected a welcome, got {welcome:#?}"
    );

    Frame::write(&mut writer, &ClientMessage::Subscribe).expect("writing succeeds");

    (inbox, writer)
}

/// Everything a pane printed, as text, across the messages seen.
fn output_of(messages: &[ServerMessage], pane: dispatch_core::PaneId) -> String {
    let bytes: Vec<u8> = messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::PaneOutput { pane: p, bytes } if *p == pane => Some(bytes.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    String::from_utf8_lossy(&bytes).into_owned()
}

/// A configuration directory for a bridge test, with the environment set
/// process-wide rather than passed to one child's `envs()`.
///
/// `dispatch_client::Client::attach_over` respawns its command on every
/// reconnect and gives it no environment of its own -- it just inherits this
/// process's -- so a test that reaches `dispatchd --stdio` through it has to
/// point the whole test process at its directory, the way [`Endpoint`] above
/// does, rather than the per-child [`Fixture::env`]-style scoping that
/// `dispatch/tests/end_to_end.rs` uses for daemons it starts directly.
struct Fixture {
    dir: PathBuf,
    previous: Option<std::ffi::OsString>,
    project: PathBuf,
}

impl Fixture {
    /// The label is short because a Unix socket address is limited to about a
    /// hundred bytes, and the daemon's endpoint lives in here.
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dispatchd-it-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir is writable");

        let project = dir.join("project");
        std::fs::create_dir_all(&project).expect("temp dir is writable");

        let previous = std::env::var_os(dispatch_os::paths::CONFIG_DIR_ENV);

        // SAFETY: callers hold `LOCK` for the fixture's whole lifetime, so no
        // other test observes this process's environment mid-change.
        unsafe { std::env::set_var(dispatch_os::paths::CONFIG_DIR_ENV, &dir) };

        Self {
            dir,
            previous,
            project,
        }
    }

    /// Where a daemon serving this fixture listens.
    fn endpoint(&self) -> PathBuf {
        self.dir.join("dispatchd.sock")
    }

    /// The project a daemon started for this fixture should serve.
    fn project(&self) -> PathBuf {
        self.project.clone()
    }
}

impl Drop for Fixture {
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

/// The daemon binary, as a path `Client::attach_over` can respawn.
fn dispatchd_binary() -> &'static str {
    env!("CARGO_BIN_EXE_dispatchd")
}

/// A running `dispatchd`, started directly rather than through a bridge.
///
/// Separate from [`RunningDaemon`]: the bridge tests need a `stop` that waits
/// for a graceful exit, the way an operator's `kill` would, so that the
/// daemon it started for a cold bridge is not still shutting down when the
/// next test claims the same temp directory.
struct Daemon(Child);

impl Daemon {
    /// Starts a daemon named `"local"`, serving `fixture`'s project.
    fn start(fixture: &Fixture) -> Self {
        let child = Command::new(dispatchd_binary())
            .arg(fixture.project())
            .arg("--device")
            .arg("local")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the built dispatchd runs");

        let daemon = Self(child);
        daemon.wait_until_listening(fixture);
        daemon
    }

    /// Blocks until the daemon accepts a connection, or explains why it
    /// never did.
    fn wait_until_listening(&self, fixture: &Fixture) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if Connection::connect_to(&fixture.endpoint()).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "the daemon never listened on {}",
            fixture.endpoint().display()
        );
    }

    /// Stops the daemon the way an operator would, so it takes its panes
    /// with it rather than orphaning them.
    fn stop(mut self) {
        #[cfg(unix)]
        let asked = Command::new("kill")
            .arg("-TERM")
            .arg(self.0.id().to_string())
            .status()
            .is_ok_and(|s| s.success());
        #[cfg(not(unix))]
        let asked = false;

        if asked {
            let deadline = Instant::now() + PATIENCE;
            while Instant::now() < deadline {
                if matches!(self.0.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Stops the daemon a cold-started bridge left running, by the pid it
/// recorded.
///
/// A bridge-started daemon outlives the bridge on purpose -- that is the
/// point of Task 4 -- so a test that triggers one has to kill it by hand, the
/// way `dispatch/tests/end_to_end.rs`'s `stop_recorded_daemon` does, or it
/// leaks a `dispatchd` per run.
fn stop_recorded_daemon(fixture: &Fixture) {
    let pid_file = fixture.dir.join("dispatchd.pid");

    let Ok(contents) = std::fs::read_to_string(&pid_file) else {
        return;
    };
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return;
    };

    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status();
    } else {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
}

#[test]
#[cfg_attr(windows, ignore = "the bridge test drives a POSIX pipeline")]
fn a_bridge_carries_a_clients_hello_to_the_daemon() {
    // The bridge is a byte pump: what a client writes to its stdin has to
    // reach the daemon, and the daemon's answer has to come back on stdout.
    // It must never parse the frames -- a bridge that understood the protocol
    // would break a client this daemon could otherwise serve.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("bridge");
    let daemon = Daemon::start(&fixture);

    let client = dispatch_client::Client::attach_over(
        dispatch_proto::Role::Interface,
        "bridge-test",
        dispatch_client::Liveness::default(),
        dispatchd_binary().into(),
        vec![
            "--stdio".into(),
            "--endpoint".into(),
            fixture.endpoint().into(),
        ],
    )
    .expect("the bridge reaches the daemon");

    assert!(client.is_connected());
    assert_eq!(
        client.device(),
        "local",
        "the daemon named itself through the pipe"
    );

    drop(client);
    daemon.stop();
}

#[test]
#[cfg_attr(windows, ignore = "the bridge test drives a POSIX pipeline")]
fn a_bridge_starts_a_daemon_when_none_is_listening() {
    // A machine nobody has used yet still has to answer: the bridge starts
    // the daemon it needs, rather than failing and leaving the user to ssh in
    // and do it by hand.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("bridge-cold");

    let client = dispatch_client::Client::attach_over(
        dispatch_proto::Role::Interface,
        "bridge-test",
        dispatch_client::Liveness::default(),
        dispatchd_binary().into(),
        vec![
            "--stdio".into(),
            "--endpoint".into(),
            fixture.endpoint().into(),
            fixture.project().into(),
        ],
    )
    .expect("the bridge starts a daemon and reaches it");

    assert!(client.is_connected());

    drop(client);
    stop_recorded_daemon(&fixture);
}

#[test]
fn a_second_connection_is_replayed_what_a_pane_printed() {
    // What a reattaching client depends on, at the socket rather than through
    // the interface: the daemon remembers and repeats.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let endpoint = Endpoint::new("replay");

    let project_dir = endpoint.dir.join("project");
    std::fs::create_dir_all(&project_dir).expect("temp dir is writable");
    let _daemon = RunningDaemon::start(&endpoint.dir, &project_dir);

    let (inbox, mut writer) = attach();
    let announced = wait_for(&inbox, "the project", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::ProjectOpened { .. }))
    });
    let project = announced
        .iter()
        .find_map(|m| match m {
            ServerMessage::ProjectOpened { project } => Some(project.id),
            _ => None,
        })
        .expect("checked by wait_for");

    Frame::write(
        &mut writer,
        &ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");
    let spawned = wait_for(&inbox, "a pane", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    let pane = spawned
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("checked by wait_for");

    for line in prove_the_shell_ran("remembered") {
        Frame::write(&mut writer, &ClientMessage::WritePane { pane, bytes: line })
            .expect("writing succeeds");
    }
    wait_for(&inbox, "the pane's output", |m| {
        output_of(m, pane).contains("remembered-42")
    });

    // The first connection goes away, as a client exiting would.
    drop(writer);
    drop(inbox);

    let (second, _writer) = attach();
    let replayed = wait_for(&second, "the replayed output", |m| {
        output_of(m, pane).contains("remembered-42")
    });
    assert!(
        replayed
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. })),
        "the pane is announced as well as replayed, got {replayed:#?}"
    );
}

#[test]
fn a_client_drives_a_pane_through_the_socket() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let endpoint = Endpoint::new("pane");

    let project_dir = endpoint.dir.join("project");
    std::fs::create_dir_all(&project_dir).expect("temp dir is writable");
    let _daemon = RunningDaemon::start(&endpoint.dir, &project_dir);

    let (reader, mut writer) = connect().split();
    let inbox = read_in_background(reader);

    Frame::write(
        &mut writer,
        &ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "integration test".into(),
            role: dispatch_proto::Role::Interface,
        },
    )
    .expect("writing succeeds");

    let welcome = wait_for(&inbox, "a welcome", |m| !m.is_empty());
    assert!(
        matches!(welcome.first(), Some(ServerMessage::Welcome { .. })),
        "expected a welcome, got {welcome:#?}"
    );

    // The project named on the daemon's command line is announced, which is how
    // a client learns an id it can spawn against.
    Frame::write(&mut writer, &ClientMessage::Subscribe).expect("writing succeeds");
    let announced = wait_for(&inbox, "the project", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::ProjectOpened { .. }))
    });
    let project = announced
        .iter()
        .find_map(|m| match m {
            ServerMessage::ProjectOpened { project } => Some(project.clone()),
            _ => None,
        })
        .expect("checked by wait_for");
    assert_eq!(
        project.root,
        dispatch_os::paths::resolve(&project_dir).expect("the project dir resolves")
    );

    Frame::write(
        &mut writer,
        &ClientMessage::SpawnPane {
            project: project.id,
            harness: "shell".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");

    let spawned = wait_for(&inbox, "a pane", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    let pane = spawned
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("checked by wait_for");

    // The marker is computed, not typed: the shell has to have run the line for
    // the answer to appear, so this proves the whole path rather than an echo of
    // what was sent.
    for line in prove_the_shell_ran("alive") {
        Frame::write(&mut writer, &ClientMessage::WritePane { pane, bytes: line })
            .expect("writing succeeds");
    }

    wait_for(&inbox, "the pane's output", |messages| {
        let output: Vec<u8> = messages
            .iter()
            .filter_map(|m| match m {
                ServerMessage::PaneOutput { pane: p, bytes } if *p == pane => Some(bytes.clone()),
                _ => None,
            })
            .flatten()
            .collect();

        String::from_utf8_lossy(&output).contains("alive-42")
    });

    Frame::write(&mut writer, &ClientMessage::ClosePane { pane }).expect("writing succeeds");
    wait_for(&inbox, "the pane to close", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneClosed { .. }))
    });
}

#[test]
fn a_delegate_caller_and_an_interface_client_share_one_daemon() {
    // The shim's path through the real binary: one connection asks, another
    // approves, and the asker is answered with the subagent's output.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let endpoint = Endpoint::new("delegate");

    let project_dir = endpoint.dir.join("project");
    std::fs::create_dir_all(&project_dir).expect("temp dir is writable");
    let _daemon = RunningDaemon::start(&endpoint.dir, &project_dir);

    // The interface client, which will approve.
    let (ui, mut ui_writer) = attach();
    let announced = wait_for(&ui, "the project", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::ProjectOpened { .. }))
    });
    let project = announced
        .iter()
        .find_map(|m| match m {
            ServerMessage::ProjectOpened { project } => Some(project.id),
            _ => None,
        })
        .expect("checked by wait_for");

    Frame::write(
        &mut ui_writer,
        &ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");
    let spawned = wait_for(&ui, "the parent pane", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    let parent = spawned
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("checked by wait_for");

    // The parent has history before the delegate caller ever connects, and the
    // interface client's own receipt of it is confirmed first. Without this, a
    // caller that merely happened to subscribe before any output existed would
    // pass the "spared the fleet's output" assertion below even with no role
    // filter on `Subscribe`'s catch-up at all.
    Frame::write(
        &mut ui_writer,
        &ClientMessage::WritePane {
            pane: parent,
            bytes: b"echo history\r".to_vec(),
        },
    )
    .expect("writing succeeds");
    wait_for(&ui, "the parent's own history", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { pane, .. } if *pane == parent))
    });

    // The delegate caller. It subscribes, via `attach_as`, exactly like the
    // interface client above — onto a pane that already has history and an
    // already-broadcast spawn to catch up on.
    let (caller, mut caller_writer) = attach_as(dispatch_proto::Role::Delegate);

    // A generous pause for whatever `Subscribe`'s catch-up was going to send —
    // over a local socket, on the order of milliseconds — followed by taking
    // whatever arrived. `wait_for` cannot express "nothing more is coming";
    // only a bounded wait can.
    std::thread::sleep(Duration::from_millis(500));
    let caught_up: Vec<ServerMessage> = std::iter::from_fn(|| caller.try_recv().ok()).collect();
    assert!(
        !caught_up
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. })),
        "a delegate caller's own Subscribe catch-up must not replay pane history, got {caught_up:#?}"
    );
    assert!(
        !caught_up
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. })),
        "a delegate caller's own Subscribe catch-up must not announce panes either, got {caught_up:#?}"
    );

    Frame::write(
        &mut caller_writer,
        &ClientMessage::DelegateRequest {
            parent,
            harness: "shell".into(),
            // No arithmetic needed here: a one-shot task runs under `-c` or
            // `/c`, which does not echo the command, so a literal marker in the
            // tail can only have come from the subagent running.
            task: "echo delegated-42".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");

    let asked = wait_for(&ui, "the pending request", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegatePending { .. }))
    });
    let request = asked
        .iter()
        .find_map(|m| match m {
            ServerMessage::DelegatePending { request, .. } => Some(*request),
            _ => None,
        })
        .expect("checked by wait_for");

    Frame::write(
        &mut ui_writer,
        &ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    )
    .expect("writing succeeds");

    let finished = wait_for(&caller, "the subagent's result", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });
    let (exit, tail) = finished
        .iter()
        .find_map(|m| match m {
            ServerMessage::DelegateFinished { exit, tail, .. } => Some((*exit, tail.clone())),
            _ => None,
        })
        .expect("checked by wait_for");

    assert_eq!(exit, 0);
    assert!(
        String::from_utf8_lossy(&tail).contains("delegated-42"),
        "the arithmetic proves the subagent ran, got {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(
        !finished
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. })),
        "a delegate caller is spared the fleet's output"
    );
}
