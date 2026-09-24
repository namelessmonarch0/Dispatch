//! Tests for the daemon loop.
//!
//! Each drives the daemon directly rather than through a socket: the transport
//! is tested in `dispatch-os`, and driving the loop keeps these about what the
//! daemon decides rather than how bytes travel.

use super::*;

use std::time::Instant;

/// A harness registry holding a plain shell, so panes run something real.
///
/// Carries a `[task]` form so delegation tests have a harness to delegate to;
/// without one, every delegation request is refused before it is even asked
/// about. Its task is a script. On Windows that script is read by PowerShell
/// from the task's file, since `cmd.exe /c {task}` is the very shape the
/// daemon refuses there; `powershell -Command -` runs what arrives on standard
/// input, where `echo` prints and `sleep` sleeps as they do under `sh`.
///
/// Also registers `no-task-args`: a harness with a `[task]` section but an
/// empty `args`, which `HarnessDef::task_launch` treats as no form at all — a
/// fixture for the difference between `task.is_some()` and
/// `task_launch(..).is_some()`.
fn harnesses(dir: &std::path::Path) -> HarnessRegistry {
    let body = if cfg!(windows) {
        "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"cmd.exe\"\n\n[task]\nargs = [\"/d\", \"/v:off\", \"/c\", \"powershell.exe\", \"-NoProfile\", \"-NonInteractive\", \"-Command\", \"-\", \"<%DISPATCH_TASK_FILE%\"]\ninput = \"file\"\n"
    } else {
        "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"sh\"\n\n[task]\nargs = [\"-c\", \"{task}\"]\n"
    };

    std::fs::create_dir_all(dir).expect("temp dir is writable");
    std::fs::write(dir.join("shell.toml"), body).expect("temp dir is writable");

    let no_task_args = if cfg!(windows) {
        "id = \"no-task-args\"\ndisplay_name = \"No task args\"\ncommand = \"cmd.exe\"\n\n[task]\nargs = []\n"
    } else {
        "id = \"no-task-args\"\ndisplay_name = \"No task args\"\ncommand = \"sh\"\n\n[task]\nargs = []\n"
    };
    std::fs::write(dir.join("no-task-args.toml"), no_task_args).expect("temp dir is writable");

    // Puts its terminal in raw mode so input is buffered rather than
    // processed, says so, and never reads: an agent busy elsewhere when a
    // paste arrives. On Windows it only never reads for itself; see
    // `a_stalled_pane_does_not_stall_the_daemon` for why that is not enough.
    let stall = if cfg!(windows) {
        "id = \"stall\"\ndisplay_name = \"Stall\"\ncommand = \"cmd.exe\"\nargs = [\"/c\", \"echo READY & ping -n 30 127.0.0.1 >nul\"]\n"
    } else {
        "id = \"stall\"\ndisplay_name = \"Stall\"\ncommand = \"sh\"\nargs = [\"-c\", \"stty raw -echo; echo READY; sleep 30\"]\n"
    };
    std::fs::write(dir.join("stall.toml"), stall).expect("temp dir is writable");

    // Prints as fast as it can, forever.
    let flood = if cfg!(windows) {
        "id = \"flood\"\ndisplay_name = \"Flood\"\ncommand = \"cmd.exe\"\nargs = [\"/c\", \"for /l %i in (0,0,1) do @echo flood\"]\n"
    } else {
        "id = \"flood\"\ndisplay_name = \"Flood\"\ncommand = \"yes\"\nargs = [\"flood\"]\n"
    };
    std::fs::write(dir.join("flood.toml"), flood).expect("temp dir is writable");

    // Prints the numbers from one to COUNTED, one to a line, and then waits:
    // output in which every byte has one place, so what a client is replayed
    // can be held against exactly what the pane printed. Through `cat`, so it
    // arrives in large writes: `seq` writing to a terminal writes a line at a
    // time, and a pane is handed a bounded number of reads per tick.
    let count = if cfg!(windows) {
        format!(
            "id = \"count\"\ndisplay_name = \"Count\"\ncommand = \"cmd.exe\"\nargs = [\"/c\", \"(for /l %i in (1,1,{COUNTED}) do @echo %i) & ping -n 30 127.0.0.1 >nul\"]\n"
        )
    } else {
        format!(
            "id = \"count\"\ndisplay_name = \"Count\"\ncommand = \"sh\"\nargs = [\"-c\", \"seq 1 {COUNTED} | cat; sleep 30\"]\n"
        )
    };
    std::fs::write(dir.join("count.toml"), count).expect("temp dir is writable");

    // A pane with a grandchild, so shutdown can be seen to end the whole tree.
    let tree = if cfg!(windows) {
        "id = \"tree\"\ndisplay_name = \"Tree\"\ncommand = \"cmd.exe\"\nargs = [\"/c\", \"ping -n 30 127.0.0.1 >nul\"]\n"
    } else {
        "id = \"tree\"\ndisplay_name = \"Tree\"\ncommand = \"sh\"\nargs = [\"-c\", \"sleep 30 & sleep 30\"]\n"
    };
    std::fs::write(dir.join("tree.toml"), tree).expect("temp dir is writable");

    HarnessRegistry::load_from_dir(dir).expect("loading succeeds")
}

/// How far the `count` harness counts.
///
/// About 2 MB of output: well past what a client that never reads can hold
/// in its socket and its outbox, so it is hung up, and far past the history
/// a late client is replayed.
const COUNTED: u32 = 300_000;

/// A temporary directory that cleans itself up.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let path = std::env::temp_dir().join(format!(
            "dispatch-daemon-{}-{label}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir is writable");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A daemon with one project registered, plus that project's id.
fn daemon(label: &str) -> (Daemon, ProjectId, TempDir) {
    let dir = TempDir::new(label);
    let registry = harnesses(&dir.0.join("harnesses"));

    let mut daemon = Daemon::new(registry, "test-device");
    daemon.set_task_dir(dir.0.join("tasks"));
    let root = dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves");
    let project = daemon.open_project(root);

    (daemon, project, dir)
}

fn hello() -> ClientMessage {
    ClientMessage::Hello {
        version: dispatch_proto::VERSION,
        client: "test".into(),
        role: dispatch_proto::Role::Interface,
    }
}

/// Everything a pane printed, as text, across the messages seen.
fn output_of(messages: &[ServerMessage], pane: PaneId) -> String {
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

/// Drains whatever a client has been sent.
fn drain(inbox: &Inbox) -> Vec<ServerMessage> {
    let mut messages = Vec::new();
    while let Ok(message) = inbox.try_recv() {
        messages.push(message);
    }
    messages
}

/// How long [`wait_for`] ticks the daemon before giving up.
///
/// A deadline, not a delay: a passing test returns the moment its predicate
/// holds, so this costs only a test that is failing anyway. Thirty seconds
/// because a delegation test on Windows starts two cold PowerShell one-shots
/// back to back, and a loaded runner once took longer than ten over them.
const WAIT_FOR_DEADLINE: Duration = Duration::from_secs(30);

/// Ticks the daemon until `predicate` holds, or gives up.
fn wait_for(
    daemon: &mut Daemon,
    inbox: &Inbox,
    predicate: impl Fn(&[ServerMessage]) -> bool,
) -> Vec<ServerMessage> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + WAIT_FOR_DEADLINE;

    loop {
        daemon.tick();
        seen.extend(drain(inbox));

        if predicate(&seen) {
            return seen;
        }
        if Instant::now() >= deadline {
            panic!("timed out; saw {seen:#?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_matching_version_is_welcomed() {
    let (mut daemon, _, _dir) = daemon("welcome");
    let inbox = daemon.attach_for_test(1);

    daemon.request_for_test(1, hello());

    let messages = drain(&inbox);
    assert!(
        matches!(messages.first(), Some(ServerMessage::Welcome { .. })),
        "expected a welcome, got {messages:?}"
    );
}

#[test]
fn an_incompatible_major_version_is_refused_and_the_client_dropped() {
    // Proceeding would let the peer misread everything sent after the
    // handshake, so the connection ends here.
    let (mut daemon, _, _dir) = daemon("refuse");
    let inbox = daemon.attach_for_test(1);

    daemon.request_for_test(
        1,
        ClientMessage::Hello {
            version: dispatch_proto::Version {
                major: 99,
                minor: 0,
            },
            client: "from the future".into(),
            role: dispatch_proto::Role::Interface,
        },
    );

    let messages = drain(&inbox);
    assert!(
        matches!(
            messages.first(),
            Some(ServerMessage::Error {
                error: ProtocolError::IncompatibleVersion { .. }
            })
        ),
        "expected a version error, got {messages:?}"
    );

    // Nothing further reaches a refused client.
    daemon.request_for_test(1, ClientMessage::Ping { token: 1 });
    assert!(drain(&inbox).is_empty(), "a refused client must be dropped");
}

#[test]
fn a_ping_is_answered_with_its_token() {
    let (mut daemon, _, _dir) = daemon("ping");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    let _ = drain(&inbox);

    daemon.request_for_test(1, ClientMessage::Ping { token: 7 });

    assert_eq!(drain(&inbox), vec![ServerMessage::Pong { token: 7 }]);
}

#[test]
fn spawning_a_pane_starts_a_process_and_tells_the_client() {
    let (mut daemon, project, _dir) = daemon("spawn");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    assert_eq!(daemon.pane_count(), 1);
    assert!(
        seen.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    );
}

#[test]
fn pane_output_reaches_the_client() {
    let (mut daemon, project, _dir) = daemon("output");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    let pane = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("a pane was spawned");

    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane,
            bytes: b"echo daemon-works\r".to_vec(),
        },
    );

    let seen = wait_for(&mut daemon, &inbox, |messages| {
        messages.iter().any(|m| match m {
            ServerMessage::PaneOutput { bytes, .. } => {
                String::from_utf8_lossy(bytes).contains("daemon-works")
            }
            _ => false,
        })
    });

    assert!(!seen.is_empty());
}

#[test]
fn every_subscribed_client_sees_the_same_panes() {
    // This is what lets a MacBook and a desktop show one fleet, so output goes
    // to all of them rather than to whoever asked.
    let (mut daemon, project, _dir) = daemon("broadcast");

    let first = daemon.attach_for_test(1);
    let second = daemon.attach_for_test(2);
    for id in [1, 2] {
        daemon.request_for_test(id, hello());
        daemon.request_for_test(id, ClientMessage::Subscribe);
    }
    let _ = drain(&first);
    let _ = drain(&second);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    daemon.tick();

    let saw_spawn = |inbox: &Inbox| {
        drain(inbox)
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    };

    assert!(saw_spawn(&first), "the client that asked should be told");
    assert!(saw_spawn(&second), "so should every other client");
}

#[test]
fn a_client_that_has_not_subscribed_is_left_quiet() {
    // A one-shot command should not be sent a session's worth of output.
    let (mut daemon, project, _dir) = daemon("quiet");

    let subscriber = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);

    let silent = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    let _ = drain(&silent);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    daemon.tick();

    assert!(
        !drain(&subscriber).is_empty(),
        "the subscriber hears about it"
    );
    assert!(drain(&silent).is_empty(), "the other client stays quiet");
}

#[test]
fn a_client_attaching_later_is_told_what_already_exists() {
    // Reattaching from another machine must show the running panes rather
    // than an empty screen until something changes.
    let (mut daemon, project, _dir) = daemon("catch-up");

    let first = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    wait_for(&mut daemon, &first, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    let late = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);

    let messages = drain(&late);
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. })),
        "a late client should be told about existing panes, got {messages:?}"
    );
}

#[test]
fn panes_outlive_the_client_that_started_them() {
    // The whole point of the split: close the laptop, the work keeps running.
    let (mut daemon, project, _dir) = daemon("outlive");

    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    daemon.detach_for_test(1);
    daemon.tick();

    assert_eq!(
        daemon.pane_count(),
        1,
        "the pane must survive its client detaching"
    );
}

#[test]
fn closing_a_pane_removes_it_and_tells_everyone() {
    let (mut daemon, project, _dir) = daemon("close");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    let pane = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("a pane was spawned");

    daemon.request_for_test(1, ClientMessage::ClosePane { pane });

    assert_eq!(daemon.pane_count(), 0);
    assert!(
        drain(&inbox)
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneClosed { .. })),
        "closing should be broadcast"
    );
}

#[test]
fn acting_on_an_unknown_pane_is_reported_rather_than_ignored() {
    let (mut daemon, _, _dir) = daemon("unknown-pane");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    let _ = drain(&inbox);

    let ghost = PaneId::new();
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: ghost,
            bytes: b"x".to_vec(),
        },
    );

    assert!(
        drain(&inbox).iter().any(|m| matches!(
            m,
            ServerMessage::Error {
                error: ProtocolError::NoSuchPane(_)
            }
        )),
        "a write to a pane that is gone should say so"
    );
}

#[test]
fn spawning_into_an_unknown_project_is_reported() {
    let (mut daemon, _, _dir) = daemon("unknown-project");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project: ProjectId::new(),
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    assert!(drain(&inbox).iter().any(|m| matches!(
        m,
        ServerMessage::Error {
            error: ProtocolError::NoSuchProject(_)
        }
    )));
}

#[test]
fn spawning_an_unknown_harness_is_reported_with_its_name() {
    let (mut daemon, project, _dir) = daemon("unknown-harness");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "nonexistent".into(),
            size: (80, 24),
        },
    );

    let messages = drain(&inbox);
    let explained = messages.iter().any(|m| match m {
        ServerMessage::Error {
            error: ProtocolError::Other(text),
        } => text.contains("nonexistent"),
        _ => false,
    });

    assert!(
        explained,
        "the error should name the harness, got {messages:?}"
    );
}

#[test]
fn an_exited_pane_is_reported_and_kept() {
    // Its final output is still worth reading, so it stays until closed.
    let (mut daemon, project, _dir) = daemon("exit");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    let pane = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("a pane was spawned");

    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane,
            bytes: b"exit 0\r".to_vec(),
        },
    );

    wait_for(&mut daemon, &inbox, |messages| {
        messages.iter().any(|m| {
            matches!(
                m,
                ServerMessage::PaneChanged {
                    update: PaneUpdate::Status {
                        status: PaneStatus::Exited(_)
                    },
                    ..
                }
            )
        })
    });

    assert_eq!(daemon.pane_count(), 1, "an exited pane stays until closed");
}

#[test]
fn a_shutdown_handle_reports_the_request() {
    let (daemon, _, _dir) = daemon("handle");
    let handle = daemon.shutdown_handle();

    assert!(!handle.is_requested(), "a fresh daemon is not stopping");
    handle.request();
    assert!(handle.is_requested());
}

#[test]
fn a_requested_shutdown_stops_the_loop_and_kills_the_panes() {
    let (mut daemon, project, dir) = daemon("shutdown");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    assert_eq!(daemon.pane_count(), 1);

    // The loop runs on another thread so a missing shutdown check fails the
    // test rather than hanging it.
    let shutdown = daemon.shutdown_handle();
    let (done, finished) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        daemon.run();
        let _ = done.send(daemon.pane_count());
        drop(dir);
    });

    shutdown.request();

    let remaining = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("run returns once a shutdown is requested");
    assert_eq!(
        remaining, 0,
        "panes are the daemon's children and must not outlive it"
    );

    worker.join().expect("the loop thread does not panic");
}

#[test]
fn reopening_a_root_keeps_one_project() {
    // Two sidebar entries for one checkout would be a bug the user has to
    // untangle by hand.
    let (mut daemon, project, dir) = daemon("reopen");
    let root = dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves");

    assert_eq!(daemon.open_project(root), project);
    assert_eq!(daemon.projects().len(), 1);
}

#[test]
fn a_subscriber_is_told_the_projects_before_the_panes() {
    // A pane names the project it belongs to, so a client that heard about the
    // pane first would have nowhere to put it.
    let (mut daemon, project, _dir) = daemon("projects-first");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    // A second client attaching now sees the whole picture.
    let later = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);

    let seen = drain(&later);
    let projects = seen
        .iter()
        .position(|m| matches!(m, ServerMessage::ProjectOpened { .. }))
        .expect("the project is announced");
    let panes = seen
        .iter()
        .position(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
        .expect("the pane is announced");
    assert!(projects < panes, "projects come first, saw {seen:#?}");

    let Some(ServerMessage::ProjectOpened { project: opened }) = seen.get(projects) else {
        unreachable!("checked above");
    };
    assert_eq!(opened.id, project);
}

#[test]
fn opening_a_project_tells_every_subscriber() {
    let (mut daemon, _, dir) = daemon("open");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    let nested = dir.0.join("nested");
    std::fs::create_dir_all(&nested).expect("temp dir is writable");

    daemon.request_for_test(
        1,
        ClientMessage::OpenProject {
            root: nested.clone(),
        },
    );

    // Past the asker's own `ProjectResolved`, which comes first.
    let seen = drain(&inbox);
    let Some(project) = seen.iter().find_map(|m| match m {
        ServerMessage::ProjectOpened { project } => Some(project),
        _ => None,
    }) else {
        panic!("expected a project, got {seen:#?}");
    };
    assert_eq!(project.name, "nested");
    assert_eq!(
        project.root,
        dispatch_os::paths::resolve(&nested).expect("the nested dir resolves"),
        "the daemon resolves the path it was given"
    );
    assert_eq!(daemon.projects().len(), 2);
}

#[test]
fn opening_a_path_that_is_not_a_directory_is_reported() {
    let (mut daemon, _, dir) = daemon("open-bad");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    let _ = drain(&inbox);

    let file = dir.0.join("not-a-directory");
    std::fs::write(&file, b"contents").expect("temp dir is writable");

    daemon.request_for_test(1, ClientMessage::OpenProject { root: file.clone() });
    assert!(
        matches!(
            drain(&inbox).first(),
            Some(ServerMessage::ProjectRefused { root, .. }) if *root == file
        ),
        "a file is not a project, and the refusal names it as it was sent"
    );

    let missing = dir.0.join("missing");
    daemon.request_for_test(
        1,
        ClientMessage::OpenProject {
            root: missing.clone(),
        },
    );
    assert!(
        matches!(
            drain(&inbox).first(),
            Some(ServerMessage::ProjectRefused { root, .. }) if *root == missing
        ),
        "a path that does not exist is not a project"
    );

    assert_eq!(daemon.projects().len(), 1, "neither was registered");
}

#[test]
fn a_root_under_home_is_opened_from_the_daemons_own_home() {
    // The daemon is on the machine the directory is on, so its home is the
    // one `~` means. `~` itself always exists, so this needs no scratch
    // directory under the real home.
    let (mut daemon, _, _dir) = daemon("open-home");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::OpenProject {
            root: PathBuf::from("~"),
        },
    );

    // Through `expand_home` rather than `directories`, which this crate does
    // not depend on: what is under test is that the daemon expands at all.
    let home = dispatch_os::paths::expand_home(Path::new("~"));
    let expected = dispatch_os::paths::resolve(&home).expect("home resolves");

    assert!(
        drain(&inbox).iter().any(|message| matches!(
            message,
            ServerMessage::ProjectOpened { project } if project.root == expected
        )),
        "`~` should open the daemon's home"
    );
}

#[test]
fn the_asker_alone_is_told_what_its_root_resolved_to() {
    // The asker keeps `~`, but the row every client gets carries the home it
    // resolved to. Only the asker has a record to rewrite; the others were
    // never told `~`, and it would mean nothing to them.
    let (mut daemon, _, _dir) = daemon("resolved");
    let asker = daemon.attach_for_test(1);
    let other = daemon.attach_for_test(2);
    for client in [1, 2] {
        daemon.request_for_test(client, hello());
        daemon.request_for_test(client, ClientMessage::Subscribe);
    }
    let _ = drain(&asker);
    let _ = drain(&other);

    daemon.request_for_test(
        1,
        ClientMessage::OpenProject {
            root: PathBuf::from("~"),
        },
    );

    let home = dispatch_os::paths::expand_home(Path::new("~"));
    let expected = dispatch_os::paths::resolve(&home).expect("home resolves");

    let heard = drain(&asker);
    assert!(
        matches!(
            heard.as_slice(),
            [
                ServerMessage::ProjectResolved { root, resolved },
                ServerMessage::ProjectOpened { project },
            ] if root == Path::new("~") && *resolved == expected && project.root == expected
        ),
        "the asker hears how its root resolved, before the row arrives: {heard:#?}"
    );

    let overheard = drain(&other);
    assert!(
        !overheard
            .iter()
            .any(|m| matches!(m, ServerMessage::ProjectResolved { .. })),
        "nobody else is told: {overheard:#?}"
    );
    assert!(
        overheard
            .iter()
            .any(|m| matches!(m, ServerMessage::ProjectOpened { .. })),
        "though everyone still gets the row"
    );
}

#[test]
fn a_client_attaching_later_is_replayed_what_a_pane_printed() {
    // Reattaching should show the work, not a blank rectangle.
    let (mut daemon, project, _dir) = daemon("replay");
    let first = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(&mut daemon, &first, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    let pane = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("a pane was spawned");

    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane,
            bytes: b"echo remembered-42\r".to_vec(),
        },
    );
    wait_for(&mut daemon, &first, |messages| {
        output_of(messages, pane).contains("remembered-42")
    });

    let late = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);

    let messages = drain(&late);
    assert!(
        output_of(&messages, pane).contains("remembered-42"),
        "a late client should be replayed the pane's output, got {messages:#?}"
    );
}

#[test]
fn a_pane_remembers_only_its_most_recent_output() {
    // An agent can print for hours; the daemon cannot keep all of it.
    let (mut daemon, project, _dir) = daemon("history-cap");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    let pane = daemon
        .panes
        .values_mut()
        .next()
        .expect("the pane is registered");

    // Fed directly: driving a real shell into producing a quarter of a megabyte
    // would make this a slow test of the shell rather than of the limit.
    pane.remember(&vec![b'a'; crate::pane::HISTORY_BYTES]);
    pane.remember(b"the newest bytes");

    let history = &daemon
        .panes
        .values()
        .next()
        .expect("the pane is registered")
        .history;
    assert_eq!(history.len(), crate::pane::HISTORY_BYTES);
    assert!(
        history.ends_with(b"the newest bytes"),
        "the newest output is what a client needs"
    );
}

#[test]
fn a_client_attaching_after_a_pane_exited_is_told_it_exited() {
    // The change happened before this client was listening, so a subscribe has
    // to carry it or the pane looks alive forever.
    let (mut daemon, project, _dir) = daemon("late-exit");
    let first = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(&mut daemon, &first, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    let pane = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("a pane was spawned");

    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane,
            bytes: b"exit 3\r".to_vec(),
        },
    );
    wait_for(&mut daemon, &first, |messages| {
        messages.iter().any(|m| {
            matches!(
                m,
                ServerMessage::PaneChanged {
                    update: PaneUpdate::Status {
                        status: PaneStatus::Exited(_)
                    },
                    ..
                }
            )
        })
    });

    let late = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);

    let messages = drain(&late);
    assert!(
        messages.iter().any(|m| matches!(
            m,
            ServerMessage::PaneChanged {
                update: PaneUpdate::Status {
                    status: PaneStatus::Exited(_)
                },
                ..
            }
        )),
        "a late client should be told the pane exited, got {messages:#?}"
    );
}

/// A daemon with one project and non-default limits.
fn daemon_with_limits(label: &str, limits: DelegationLimits) -> (Daemon, ProjectId, TempDir) {
    let dir = TempDir::new(label);
    let registry = harnesses(&dir.0.join("harnesses"));

    let mut daemon = Daemon::with_limits(registry, "test-device", limits);
    daemon.set_task_dir(dir.0.join("tasks"));
    let root = dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves");
    let project = daemon.open_project(root);

    (daemon, project, dir)
}

/// Spawns a pane the ordinary way and returns its id.
fn spawn_pane_for_test(daemon: &mut Daemon, inbox: &Inbox, project: ProjectId) -> PaneId {
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(daemon, inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    seen.iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("a pane was spawned")
}

/// Whether a spawn announcement is for a delegated pane.
fn m_is_child(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::PaneSpawned {
            parent: Some(_),
            ..
        }
    )
}

/// Attaches a delegate caller and asks for a subagent.
fn ask(daemon: &mut Daemon, parent: PaneId, task: &str) -> Inbox {
    ask_as(daemon, 9, parent, task)
}

/// Attaches a delegate caller under a specific client id and asks for a
/// subagent. Needed over `ask` when a test drives two delegate callers at
/// once, since `ask` always reuses id 9.
fn ask_as(daemon: &mut Daemon, id: u64, parent: PaneId, task: &str) -> Inbox {
    let caller = daemon.attach_for_test(id);
    daemon.request_for_test(
        id,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(
        id,
        ClientMessage::DelegateRequest {
            parent,
            harness: "shell".into(),
            task: task.into(),
            size: (80, 24),
        },
    );
    caller
}

/// The first pending request an interface client was told about.
fn pending(messages: &[ServerMessage]) -> Option<dispatch_core::RequestId> {
    messages.iter().find_map(|m| match m {
        ServerMessage::DelegatePending { request, .. } => Some(*request),
        _ => None,
    })
}

#[test]
fn a_delegation_request_is_put_to_the_user() {
    let (mut daemon, project, _dir) = daemon("delegate-ask");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let _caller = ask(&mut daemon, parent, "echo delegated");

    let seen = drain(&ui);
    let request = pending(&seen).expect("the interface is asked");
    assert!(
        matches!(
            seen.iter().find(|m| matches!(m, ServerMessage::DelegatePending { .. })),
            Some(ServerMessage::DelegatePending { task, .. }) if task == "echo delegated"
        ),
        "the whole task travels, got {seen:#?}"
    );
    assert_eq!(daemon.pane_count(), 1, "nothing runs before an answer");
    let _ = request;
}

#[test]
fn approving_a_request_starts_a_subagent_under_its_parent() {
    let (mut daemon, project, _dir) = daemon("delegate-approve");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo delegated-42");
    let request = pending(&drain(&ui)).expect("the interface is asked");

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );

    let seen = wait_for(&mut daemon, &caller, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });

    let finished = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::DelegateFinished { exit, tail, .. } => Some((*exit, tail.clone())),
            _ => None,
        })
        .expect("the caller is answered");
    assert_eq!(finished.0, 0, "the subagent's own exit code");
    assert!(
        String::from_utf8_lossy(&finished.1).contains("delegated-42"),
        "the tail carries what the subagent printed, got {:?}",
        String::from_utf8_lossy(&finished.1)
    );
    assert_eq!(daemon.pane_count(), 2, "the subagent's pane is kept");
}

#[test]
fn denying_a_request_starts_nothing() {
    let (mut daemon, project, _dir) = daemon("delegate-deny");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo never");
    let request = pending(&drain(&ui)).expect("the interface is asked");

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: false,
            blanket: false,
        },
    );

    let seen = drain(&caller);
    assert!(
        seen.iter().any(|m| matches!(
            m,
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Denied,
                ..
            }
        )),
        "the caller is told, got {seen:#?}"
    );
    assert_eq!(daemon.pane_count(), 1);
}

#[test]
fn a_blanket_approval_stops_the_asking() {
    let (mut daemon, project, _dir) = daemon("delegate-blanket");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let first = ask(&mut daemon, parent, "echo one");
    let request = pending(&drain(&ui)).expect("the first is asked about");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: true,
        },
    );
    wait_for(&mut daemon, &first, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });
    let _ = drain(&ui);

    // The second request from the same pane is not put to anyone.
    let second = ask(&mut daemon, parent, "echo two");
    wait_for(&mut daemon, &second, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });

    assert!(
        pending(&drain(&ui)).is_none(),
        "a pane approved with [A] is not asked about again"
    );
}

#[test]
fn a_subagent_dies_with_the_caller_that_asked_for_it() {
    let (mut daemon, project, _dir) = daemon("delegate-orphan");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _caller = ask(&mut daemon, parent, "sleep 30");
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );
    wait_for(&mut daemon, &ui, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }) && m_is_child(m))
    });

    // The agent hits Ctrl-C, or its pane is killed: either way the socket goes.
    daemon.detach_for_test(9);
    daemon.tick();

    assert_eq!(
        daemon.pane_count(),
        1,
        "a one-off subagent has nobody left to answer"
    );
}

#[test]
fn a_blanket_approved_subagent_survives_its_caller() {
    let (mut daemon, project, _dir) = daemon("delegate-durable");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _caller = ask(&mut daemon, parent, "sleep 30");
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: true,
        },
    );
    wait_for(&mut daemon, &ui, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }) && m_is_child(m))
    });

    daemon.detach_for_test(9);
    daemon.tick();

    assert_eq!(
        daemon.pane_count(),
        2,
        "[A] is how the user says to let this pane's work run"
    );
}

#[test]
fn a_request_nobody_answers_is_expired_when_its_time_is_up() {
    let (mut daemon, project, _dir) = daemon_with_limits(
        "delegate-timeout",
        DelegationLimits {
            request_timeout_secs: 0,
            ..DelegationLimits::default()
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo never");

    daemon.tick();

    let seen = drain(&caller);
    assert!(
        seen.iter().any(|m| matches!(
            m,
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Expired { .. },
                ..
            }
        )),
        "a caller must not wait on an unattended daemon forever, got {seen:#?}"
    );
    assert_eq!(daemon.pane_count(), 1, "and a late approval spawns nothing");
}

#[test]
fn a_pane_the_daemon_does_not_own_cannot_delegate() {
    let (mut daemon, _project, _dir) = daemon("delegate-stranger");
    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    let _ = drain(&caller);

    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent: PaneId::new(),
            harness: "shell".into(),
            task: "echo hello".into(),
            size: (80, 24),
        },
    );

    assert!(
        matches!(
            drain(&caller).first(),
            Some(ServerMessage::Error {
                error: ProtocolError::NoSuchPane(_)
            })
        ),
        "an unknown parent is not a pane this daemon can attribute work to"
    );
}

#[test]
fn a_delegate_caller_is_not_sent_pane_output() {
    // It waits on one request; the fleet's output is a firehose it never reads.
    // Subscribing here matters: an unsubscribed client is already excluded by
    // `broadcast`, which would let this pass even if the role filter were
    // missing. Subscribing puts the assertion on the role check alone.
    let (mut daemon, project, _dir) = daemon("delegate-quiet");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let caller = ask(&mut daemon, parent, "echo quiet");
    daemon.request_for_test(9, ClientMessage::Subscribe);
    let _ = drain(&caller);

    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: parent,
            bytes: b"echo noisy\r".to_vec(),
        },
    );
    wait_for(&mut daemon, &ui, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. }))
    });

    assert!(
        !drain(&caller)
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. })),
        "a delegate caller hears about its own request only"
    );
}

#[test]
fn a_delegate_callers_subscribe_catch_up_carries_none_of_the_fleet() {
    // `broadcast` keeps the fleet's ongoing traffic from a delegate caller, but
    // `Subscribe`'s catch-up is a separate path that replays what already
    // happened before this client asked — a pane with history already has
    // something to replay by the time this runs. Building the pane and its
    // history first, and confirming an interface client actually saw the
    // output, is what makes this assertion rest on the role filter rather than
    // on the pane happening to be silent when the delegate caller connects.
    let (mut daemon, project, _dir) = daemon("delegate-catchup-history");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: parent,
            bytes: b"echo history\r".to_vec(),
        },
    );
    wait_for(&mut daemon, &ui, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. }))
    });

    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(9, ClientMessage::Subscribe);

    let seen = drain(&caller);
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. })),
        "a delegate caller's Subscribe catch-up must not replay pane history, got {seen:#?}"
    );
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. })),
        "a delegate caller's Subscribe catch-up must not announce panes either, got {seen:#?}"
    );
}

#[test]
fn a_finished_subagent_survives_its_caller_detaching() {
    // `dispatch delegate` exits the instant it has its answer, so this is the
    // common case, not an edge case: reaping the pane here would throw away
    // the very output TAIL_BYTES exists so a person can still read.
    let (mut daemon, project, _dir) = daemon("delegate-finished-orphan");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo done-42");
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );
    wait_for(&mut daemon, &caller, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });

    // The delegate process is gone by now in real use.
    daemon.detach_for_test(9);
    daemon.tick();

    assert_eq!(
        daemon.pane_count(),
        2,
        "a finished subagent is not reaped just because its caller is gone"
    );
}

#[test]
fn closing_the_asking_pane_refuses_its_pending_request() {
    // Without this, the caller's `Pending` entry is gone the moment the
    // decision arrives (there is none to time out), and `approve`'s missing-
    // pane branch used to return silently: the caller would hang until the
    // daemon itself died.
    let (mut daemon, project, _dir) = daemon("delegate-parent-closed");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo never");
    let _request = pending(&drain(&ui)).expect("the interface is asked");

    daemon.request_for_test(1, ClientMessage::ClosePane { pane: parent });

    let seen = drain(&caller);
    assert!(
        seen.iter().any(|m| matches!(
            m,
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Refused { .. },
                ..
            }
        )),
        "a caller must not hang forever on a pane that closed before answering, got {seen:#?}"
    );
}

/// Ends `pane`'s shell, and ticks until its exit has been reported.
fn exit_pane(daemon: &mut Daemon, ui: &Inbox, pane: PaneId) {
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane,
            bytes: b"exit 0\r".to_vec(),
        },
    );
    wait_for(daemon, ui, |m| {
        m.iter().any(|m| {
            matches!(
                m,
                ServerMessage::PaneChanged {
                    pane: p,
                    update: PaneUpdate::Status {
                        status: PaneStatus::Exited(_)
                    },
                } if *p == pane
            )
        })
    });
}

/// Why a caller's request was refused, if it was.
fn refusal(messages: &[ServerMessage]) -> Option<String> {
    messages.iter().find_map(|m| match m {
        ServerMessage::DelegateResolved {
            outcome: dispatch_proto::DelegateOutcome::Refused { reason },
            ..
        } => Some(reason.clone()),
        _ => None,
    })
}

#[test]
fn approving_a_request_from_a_pane_that_has_exited_starts_nothing() {
    // An exited pane keeps its row, so its last output can be read, but the
    // agent that asked is gone: a subagent started for it would work for
    // nobody, under a parent nothing will ever close.
    let (mut daemon, project, _dir) = daemon("delegate-parent-exited");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo never");
    let request = pending(&drain(&ui)).expect("the interface is asked");

    exit_pane(&mut daemon, &ui, parent);
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );

    let seen = drain(&caller);
    assert_eq!(
        refusal(&seen).as_deref(),
        Some("the pane that asked has exited"),
        "the caller is told why, got {seen:#?}"
    );
    assert_eq!(daemon.pane_count(), 1, "no subagent was started");
}

#[test]
fn a_blanket_approval_starts_nothing_for_a_pane_that_has_exited() {
    // The blanket outlives the agent it was given to, since the row does;
    // what it approved was that agent's requests, and there are no more.
    let (mut daemon, project, _dir) = daemon("delegate-blanket-exited");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let first = ask(&mut daemon, parent, "echo one");
    let request = pending(&drain(&ui)).expect("the first is asked about");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: true,
        },
    );
    wait_for(&mut daemon, &first, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });

    exit_pane(&mut daemon, &ui, parent);
    let second = ask(&mut daemon, parent, "echo two");

    let seen = drain(&second);
    assert_eq!(
        refusal(&seen).as_deref(),
        Some("the pane that asked has exited"),
        "the caller is told why, got {seen:#?}"
    );
    assert_eq!(
        daemon.pane_count(),
        2,
        "the parent and its first subagent, and nothing started since"
    );
}

#[test]
fn every_interface_client_is_told_when_a_request_is_resolved() {
    // The next task draws the prompt on every interface client that saw it;
    // without this, a denied or expired request stays on screen for everyone
    // but the one who answered it.
    let (mut daemon, project, _dir) = daemon("delegate-resolved-broadcast");
    let first = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let second = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &first, project);
    let _ = drain(&second);

    let caller = ask(&mut daemon, parent, "echo resolved");
    let request = pending(&drain(&first)).expect("the first client sees the prompt");
    assert!(
        pending(&drain(&second)).is_some(),
        "the second interface client sees the same prompt"
    );

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: false,
            blanket: false,
        },
    );
    let _ = drain(&caller);

    let seen = drain(&second);
    assert!(
        seen.iter().any(|m| matches!(
            m,
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Denied,
                ..
            }
        )),
        "an interface client that saw the prompt should be told it is resolved, got {seen:#?}"
    );
}

#[test]
fn a_delegate_caller_that_subscribes_is_not_told_about_pending_requests() {
    // The Subscribe catch-up is for interface clients drawing the fleet; a
    // delegate caller does not draw prompts, and `broadcast` already excludes
    // it for the same reason once a request is live.
    let (mut daemon, project, _dir) = daemon("delegate-catchup");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let caller = ask(&mut daemon, parent, "echo catchup");
    let _ = drain(&caller);

    daemon.request_for_test(9, ClientMessage::Subscribe);

    assert!(
        !drain(&caller)
            .iter()
            .any(|m| matches!(m, ServerMessage::DelegatePending { .. })),
        "a delegate caller's own Subscribe catch-up must not include prompts"
    );
}

#[test]
fn closing_a_running_subagents_pane_answers_its_caller() {
    // The pane is killed rather than allowed to finish, so pump_panes never
    // sees its exit, and the request was already removed from `pending` when
    // it was approved -- so without an explicit answer here, nothing would
    // ever tell the caller anything.
    let (mut daemon, project, _dir) = daemon("delegate-close-running-subagent");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "sleep 30");
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );

    let seen = wait_for(&mut daemon, &caller, |m| {
        m.iter().any(|m| {
            matches!(
                m,
                ServerMessage::DelegateResolved {
                    outcome: dispatch_proto::DelegateOutcome::Approved { .. },
                    ..
                }
            )
        })
    });
    let subagent = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Approved { pane },
                ..
            } => Some(*pane),
            _ => None,
        })
        .expect("the subagent was approved");

    daemon.request_for_test(1, ClientMessage::ClosePane { pane: subagent });

    let seen = drain(&caller);
    assert!(
        seen.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. })),
        "closing a running subagent's own pane must still answer its caller, got {seen:#?}"
    );
}

#[test]
fn an_interface_client_that_delegates_is_not_told_twice() {
    // Nothing gates DelegateRequest on role, so an interface client can be its
    // own caller -- an agent delegating from a pane someone happens to be
    // watching through the same connection. `resolve` answers it directly;
    // the broadcast half must not repeat that answer.
    let (mut daemon, project, _dir) = daemon("delegate-self-caller");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _ = drain(&ui);

    daemon.request_for_test(
        1,
        ClientMessage::DelegateRequest {
            parent,
            harness: "shell".into(),
            task: "echo self".into(),
            size: (80, 24),
        },
    );
    let request = pending(&drain(&ui)).expect("the interface is asked");

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: false,
            blanket: false,
        },
    );

    let seen = drain(&ui);
    let resolved = seen
        .iter()
        .filter(|m| matches!(m, ServerMessage::DelegateResolved { .. }))
        .count();
    assert_eq!(
        resolved, 1,
        "an interface client that is also the caller should hear its answer once, got {seen:#?}"
    );
}

#[test]
fn a_harness_with_an_empty_task_form_is_refused_without_asking() {
    // The point of matching approve()'s own predicate: the user is never put
    // in the position of approving something that will just fail afterward.
    let (mut daemon, project, _dir) = daemon("delegate-empty-task-args");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _ = drain(&ui);

    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    let _ = drain(&caller);
    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent,
            harness: "no-task-args".into(),
            task: "echo never".into(),
            size: (80, 24),
        },
    );

    let seen = drain(&caller);
    assert!(
        seen.iter().any(|m| matches!(
            m,
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Refused { .. },
                ..
            }
        )),
        "a harness whose [task] has no runnable args must be refused immediately, got {seen:#?}"
    );
    assert!(
        pending(&drain(&ui)).is_none(),
        "the user must never be asked about a harness that cannot actually run"
    );
}

#[test]
fn closing_a_pane_drops_its_whole_delegation_subtree() {
    // A grandchild must go too, not just the direct child: otherwise it is
    // left with a `parent` pointing at nothing, and depth_of/live_children
    // silently under-count from then on.
    let (mut daemon, project, _dir) = daemon_with_limits(
        "delegate-cascade",
        DelegationLimits {
            max_depth: 2,
            ..DelegationLimits::default()
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let root = spawn_pane_for_test(&mut daemon, &ui, project);

    let first_caller = ask_as(&mut daemon, 9, root, "sleep 30");
    let request = pending(&drain(&ui)).expect("the interface is asked about the child");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );
    let seen = wait_for(&mut daemon, &first_caller, |m| {
        m.iter().any(|m| {
            matches!(
                m,
                ServerMessage::DelegateResolved {
                    outcome: dispatch_proto::DelegateOutcome::Approved { .. },
                    ..
                }
            )
        })
    });
    let child = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Approved { pane },
                ..
            } => Some(*pane),
            _ => None,
        })
        .expect("the child was approved");
    let _ = drain(&ui);

    // A second delegate caller, as if the child's own agent asked for a
    // subagent of its own.
    let second_caller = ask_as(&mut daemon, 10, child, "sleep 30");
    let request = pending(&drain(&ui)).expect("the interface is asked about the grandchild");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );
    wait_for(&mut daemon, &second_caller, |m| {
        m.iter().any(|m| {
            matches!(
                m,
                ServerMessage::DelegateResolved {
                    outcome: dispatch_proto::DelegateOutcome::Approved { .. },
                    ..
                }
            )
        })
    });

    assert_eq!(
        daemon.pane_count(),
        3,
        "root, child, and grandchild all exist"
    );

    daemon.request_for_test(1, ClientMessage::ClosePane { pane: root });

    assert_eq!(
        daemon.pane_count(),
        0,
        "closing the root must drop the whole subtree, not just its direct child"
    );
}

#[test]
fn a_prompt_whose_caller_has_gone_is_withdrawn_rather_than_left_on_screen() {
    // Ctrl-C on `dispatch delegate` closes the socket, and the request goes
    // with it. Dropped in silence, the prompt stayed on every interface client:
    // the user presses `a`, `DelegateDecision` finds no pending entry, returns,
    // and nothing whatsoever happens. Every other resolution path broadcasts,
    // and a withdrawal is exactly what closes a prompt.
    let (mut daemon, project, _dir) = daemon("delegate-caller-gone");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _caller = ask(&mut daemon, parent, "echo never");
    let request = pending(&drain(&ui)).expect("the interface is asked");

    daemon.detach_for_test(9);

    let seen = drain(&ui);
    assert!(
        seen.iter().any(|m| matches!(
            m,
            ServerMessage::DelegateResolved {
                request: withdrawn,
                outcome: dispatch_proto::DelegateOutcome::Refused { .. },
            } if *withdrawn == request
        )),
        "the prompt must be withdrawn from the interface, got {seen:#?}"
    );

    // And answering it afterwards is answering nothing, which is precisely why
    // it must not still be on screen.
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );
    daemon.tick();
    assert_eq!(
        daemon.pane_count(),
        1,
        "a withdrawn request cannot be approved into a subagent"
    );
}

#[test]
fn a_late_subscriber_is_told_about_pending_requests_oldest_first() {
    // The client documents its queue as oldest first and shows the front of it;
    // `HashMap` order would hand a reattaching client the prompts in whatever
    // order the hasher happened to like, so the request the user has been
    // waiting on longest need not be the one they are shown.
    let (mut daemon, project, _dir) = daemon("delegate-catch-up-order");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    // Six, because one hash order in a handful agreeing with insertion order is
    // luck; six agreeing is not.
    let mut asked = Vec::new();
    for (index, id) in (20..26).enumerate() {
        let _caller = ask_as(&mut daemon, id, parent, &format!("task {index}"));
        asked.push(
            pending(&drain(&ui)).unwrap_or_else(|| panic!("the interface is asked about {index}")),
        );
    }

    let late = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);

    let replayed: Vec<dispatch_core::RequestId> = drain(&late)
        .iter()
        .filter_map(|m| match m {
            ServerMessage::DelegatePending { request, .. } => Some(*request),
            _ => None,
        })
        .collect();

    assert_eq!(
        replayed, asked,
        "a late subscriber should be caught up in the order the requests were asked"
    );
}

#[test]
fn closing_an_empty_project_forgets_it_and_tells_everyone() {
    let (mut daemon, project, _dir) = daemon("close-project");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(1, ClientMessage::CloseProject { project });

    assert!(
        daemon.projects().is_empty(),
        "the daemon forgets it, or the next Subscribe hands it straight back"
    );
    assert!(
        drain(&inbox)
            .iter()
            .any(|m| matches!(m, ServerMessage::ProjectClosed { project: p } if *p == project)),
        "closing should be broadcast"
    );
}

#[test]
fn a_project_with_panes_is_not_closed() {
    // Its agents belong to the daemon and would carry on running with nothing
    // left to reach them by.
    let (mut daemon, project, _dir) = daemon("close-project-busy");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    daemon.request_for_test(1, ClientMessage::CloseProject { project });

    assert_eq!(daemon.projects().len(), 1, "it stays");
    assert!(
        drain(&inbox)
            .iter()
            .any(|m| matches!(m, ServerMessage::Error { .. })),
        "and the client is told why"
    );
}

#[test]
fn closing_an_unknown_project_is_reported() {
    let (mut daemon, _, _dir) = daemon("close-project-unknown");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::CloseProject {
            project: ProjectId::new(),
        },
    );

    assert!(drain(&inbox).iter().any(|m| matches!(
        m,
        ServerMessage::Error {
            error: ProtocolError::NoSuchProject(_)
        }
    )));
}

/// A task that is still running thirty seconds from now, on every platform.
///
/// `sleep` is not a command under `cmd.exe`: there it fails at once, and a
/// test about what is *running* would pass for the wrong reason. No `>nul`:
/// from Task 17 the Windows fixture runs its task under PowerShell, where
/// that redirection fails.
fn long_task() -> &'static str {
    if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    }
}

/// Every `DelegateResolved` outcome among `messages`.
fn outcomes(messages: &[ServerMessage]) -> Vec<DelegateOutcome> {
    messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::DelegateResolved { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn approving_more_requests_than_the_cap_allows_starts_only_what_fits() {
    // Both requests are asked about while nothing is running, so both pass
    // the check on arrival. Approving them one after the other must still
    // start only one: the cap is on what runs, not on what is asked.
    let (mut daemon, project, _dir) = daemon_with_limits(
        "cap-at-approval",
        DelegationLimits {
            max_depth: 1,
            max_live_per_parent: 1,
            request_timeout_secs: 600,
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let first = ask_as(&mut daemon, 8, parent, long_task());
    let second = ask_as(&mut daemon, 9, parent, long_task());
    let requests: Vec<RequestId> = drain(&ui)
        .iter()
        .filter_map(|m| match m {
            ServerMessage::DelegatePending { request, .. } => Some(*request),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 2, "both fit while nothing runs yet");

    for request in requests {
        daemon.request_for_test(
            1,
            ClientMessage::DelegateDecision {
                request,
                approve: true,
                blanket: false,
            },
        );
    }

    assert_eq!(
        daemon.pane_count(),
        2,
        "the parent and exactly one subagent"
    );

    let mut told = outcomes(&drain(&first));
    told.extend(outcomes(&drain(&second)));
    assert_eq!(
        told.iter()
            .filter(|o| matches!(o, DelegateOutcome::Approved { .. }))
            .count(),
        1,
        "one caller is told it runs: {told:?}"
    );
    assert!(
        told.iter()
            .any(|o| matches!(o, DelegateOutcome::Refused { reason } if reason.contains("cap"))),
        "the other is told why it does not: {told:?}"
    );
}

#[test]
fn approvals_from_two_interfaces_do_not_share_one_slot() {
    // The same race with the approvals coming from two people at two
    // screens, which is the ordinary shape on a shared fleet.
    let (mut daemon, project, _dir) = daemon_with_limits(
        "cap-two-uis",
        DelegationLimits {
            max_depth: 1,
            max_live_per_parent: 1,
            request_timeout_secs: 600,
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let other_ui = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _ = drain(&other_ui);

    let _first = ask_as(&mut daemon, 8, parent, long_task());
    let _second = ask_as(&mut daemon, 9, parent, long_task());
    let requests: Vec<RequestId> = drain(&ui)
        .iter()
        .filter_map(|m| match m {
            ServerMessage::DelegatePending { request, .. } => Some(*request),
            _ => None,
        })
        .collect();

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request: requests[0],
            approve: true,
            blanket: false,
        },
    );
    daemon.request_for_test(
        2,
        ClientMessage::DelegateDecision {
            request: requests[1],
            approve: true,
            blanket: false,
        },
    );

    assert_eq!(
        daemon.pane_count(),
        2,
        "the parent and exactly one subagent"
    );
}

#[test]
fn a_blanket_approved_pane_at_its_cap_is_refused_rather_than_started() {
    let (mut daemon, project, _dir) = daemon_with_limits(
        "cap-blanket",
        DelegationLimits {
            max_depth: 1,
            max_live_per_parent: 1,
            request_timeout_secs: 600,
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let _first = ask_as(&mut daemon, 8, parent, long_task());
    let request = pending(&drain(&ui)).expect("the first is asked about");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: true,
        },
    );

    let second = ask_as(&mut daemon, 9, parent, long_task());

    assert_eq!(
        daemon.pane_count(),
        2,
        "blanket approval is not a second slot"
    );
    assert!(
        outcomes(&drain(&second))
            .iter()
            .any(|o| matches!(o, DelegateOutcome::Refused { .. })),
        "the second caller is refused"
    );
}

fn spawn_request(project: ProjectId) -> ClientMessage {
    ClientMessage::SpawnPane {
        project,
        harness: "shell".into(),
        size: (80, 24),
    }
}

#[test]
fn nothing_is_acted_on_before_a_hello() {
    let (mut daemon, project, _dir) = daemon("before-hello");
    let inbox = daemon.attach_for_test(1);

    daemon.request_for_test(1, spawn_request(project));

    assert_eq!(
        daemon.pane_count(),
        0,
        "a request before Hello starts nothing"
    );
    assert!(
        drain(&inbox)
            .iter()
            .any(|m| matches!(m, ServerMessage::Error { .. })),
        "and says why"
    );

    // The connection is over: a Hello now is too late to rescue it.
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, spawn_request(project));
    assert_eq!(daemon.pane_count(), 0);
    assert!(drain(&inbox).is_empty(), "nothing more reaches it");
}

#[test]
fn a_refused_client_cannot_act_afterwards() {
    // The audit's probe: an incompatible Hello is answered with an error, and
    // then a SpawnPane from the same client started a pane anyway.
    let (mut daemon, project, _dir) = daemon("refused-acts");
    let refused = daemon.attach_for_test(2);
    daemon.request_for_test(
        2,
        ClientMessage::Hello {
            version: dispatch_proto::Version {
                major: 99,
                minor: 0,
            },
            client: "incompatible".into(),
            role: dispatch_proto::Role::Interface,
        },
    );
    assert!(matches!(
        refused.try_recv(),
        Ok(ServerMessage::Error {
            error: ProtocolError::IncompatibleVersion { .. }
        })
    ));

    daemon.request_for_test(2, spawn_request(project));

    assert_eq!(daemon.pane_count(), 0, "a refused client spawned a pane");
}

#[test]
fn a_detached_client_cannot_act() {
    let (mut daemon, project, _dir) = daemon("detached-acts");
    let _inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.detach_for_test(1);

    // A request already read off the socket arrives after the reader said
    // the client left: events from one client are ordered, but a request
    // queued by a thread that is gone is still a request from nobody.
    daemon.request_for_test(1, spawn_request(project));

    assert_eq!(daemon.pane_count(), 0);
}

/// Unix only, although `stall` has a Windows form: no program on Windows
/// can stop its pane's input being read. The pipe a pane's input goes down
/// is read by the pseudoconsole host, not by the program in the console,
/// and the host moves what arrives into the console's input buffer whether
/// or not anything reads that. The queue this test fills drains instead,
/// and the refusal it ends on -- input past the budget, waiting for a pane
/// that is not reading -- cannot be brought about.
#[test]
#[cfg(unix)]
fn a_stalled_pane_does_not_stall_the_daemon() {
    let (mut daemon, project, _dir) = daemon_with_limits(
        "stalled",
        DelegationLimits {
            request_timeout_secs: 1,
            ..DelegationLimits::default()
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);

    // A shell whose output must keep flowing, and a request whose deadline
    // must keep counting, while another pane is stalled.
    let shell = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, shell, "echo never");

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "stall".into(),
            size: (80, 24),
        },
    );
    let seen = wait_for(&mut daemon, &ui, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { harness, .. } if harness == "stall"))
    });
    let stalled = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, harness, .. } if harness == "stall" => Some(*pane),
            _ => None,
        })
        .expect("the stalled pane was spawned");
    wait_for(&mut daemon, &ui, |m| {
        output_of(m, stalled).contains("READY")
    });

    // The audit's probe: this one request held the loop for as long as the
    // pane went on not reading.
    let started = Instant::now();
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: stalled,
            bytes: vec![b'x'; 2 * 1024 * 1024],
        },
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "one write held the daemon for {:?}",
        started.elapsed()
    );

    // Another client is still answered.
    let other = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    let _ = drain(&other);
    daemon.request_for_test(2, ClientMessage::Ping { token: 5 });
    assert_eq!(drain(&other), vec![ServerMessage::Pong { token: 5 }]);

    // Another pane's output still flows.
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: shell,
            bytes: b"echo still-flowing\n".to_vec(),
        },
    );
    wait_for(&mut daemon, &ui, |m| {
        output_of(m, shell).contains("still-flowing")
    });

    // The pending request still runs out of time.
    wait_for(&mut daemon, &caller, |m| {
        m.iter().any(|m| {
            matches!(
                m,
                ServerMessage::DelegateResolved {
                    outcome: DelegateOutcome::Expired { .. },
                    ..
                }
            )
        })
    });

    // More than the budget is refused out loud rather than queued.
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: stalled,
            bytes: vec![b'y'; dispatch_pty::INPUT_BUDGET],
        },
    );
    let told = drain(&ui);
    assert!(
        told.iter().any(|m| matches!(
            m,
            ServerMessage::Error { error: ProtocolError::Other(text) } if text.contains("not reading")
        )),
        "the writer is told its input was dropped, got {told:#?}"
    );

    // Closing the stalled pane takes effect at once.
    let started = Instant::now();
    daemon.request_for_test(1, ClientMessage::ClosePane { pane: stalled });
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(daemon.pane_count(), 1, "only the shell is left");

    // And the daemon still stops when asked.
    daemon.shutdown_handle().request();
    let started = Instant::now();
    daemon.run();
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// A client that says `Ping` for ever, counting the frames it has begun.
struct EndlessPings {
    frame: Vec<u8>,
    at: usize,
    begun: Arc<AtomicUsize>,
}

impl Read for EndlessPings {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at == 0 {
            self.begun.fetch_add(1, Ordering::Relaxed);
        }
        let n = buf.len().min(self.frame.len() - self.at);
        buf[..n].copy_from_slice(&self.frame[self.at..self.at + n]);
        self.at = (self.at + n) % self.frame.len();
        Ok(n)
    }
}

/// Waits for `count` to reach `floor` and then stop moving, and says where
/// it stopped.
///
/// Stillness alone could be a thread the scheduler set aside for a moment
/// on a loaded machine; the floor rules that out. Gives up after ten
/// seconds with wherever it has got to: a count that never reaches the
/// floor, or never stops past it, is what the caller asserts against, not
/// a hang.
fn settled(count: &AtomicUsize, floor: usize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = count.load(Ordering::Relaxed);
    let mut still_since = Instant::now();

    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        let now = count.load(Ordering::Relaxed);
        if now != last {
            last = now;
            still_since = Instant::now();
        } else if last >= floor && still_since.elapsed() >= Duration::from_millis(300) {
            break;
        }
    }

    last
}

#[test]
fn a_client_outrunning_a_busy_loop_waits_at_the_event_backlog() {
    // The loop is never run here, which is how a loop busy with something
    // else looks to a client's reader thread. With nothing taking events,
    // the reader may queue EVENT_BACKLOG of them -- its attach and then its
    // requests -- and must then wait to hand over the one it has just read,
    // reading nothing further. Unbounded, a client sending faster than the
    // daemon acts would be queued for without limit.
    let (daemon, _, _dir) = daemon("backlog");

    let mut frame = Vec::new();
    Frame::write(&mut frame, &ClientMessage::Ping { token: 1 }).expect("a ping encodes");
    let begun = Arc::new(AtomicUsize::new(0));
    let reader = EndlessPings {
        frame,
        at: 0,
        begun: Arc::clone(&begun),
    };
    let connection = Connection::from_halves(Box::new(reader), Box::new(std::io::sink()));
    spawn_client(
        1,
        connection,
        &daemon.sender,
        &Arc::new(AtomicUsize::new(0)),
    );

    // The attach takes a place, so EVENT_BACKLOG - 1 requests are queued and
    // one more has been read and is waiting to join them.
    assert_eq!(
        settled(&begun, EVENT_BACKLOG),
        EVENT_BACKLOG,
        "the reader should stop once the backlog is full"
    );

    // One place freed lets exactly one more through: the reader was waiting
    // on the backlog, not finished or stuck on anything else.
    assert!(
        matches!(daemon.events.try_recv(), Ok(Event::Attached(1, _))),
        "the attach is first in the queue"
    );
    assert_eq!(
        settled(&begun, EVENT_BACKLOG + 1),
        EVENT_BACKLOG + 1,
        "one place freed should let one more request in"
    );
}

use std::io::{Read, Write};

/// A daemon serving a real endpoint on a thread of its own, stopped when
/// dropped.
///
/// Most tests here drive the loop directly; these are the ones about what
/// happens to the connection itself, which only a socket can show.
struct Served {
    endpoint: PathBuf,
    project: ProjectId,
    shutdown: Shutdown,
    thread: Option<std::thread::JoinHandle<()>>,
    _dir: TempDir,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.shutdown.request();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Serves a fresh daemon under `budgets`. Keep `label` short: a Unix
/// socket's whole path must fit in about a hundred bytes.
fn served(label: &str, budgets: Budgets) -> Served {
    let (mut daemon, project, dir) = daemon(label);
    daemon.set_budgets(budgets);

    let endpoint = dir.0.join("d.sock");
    let listener = Listener::bind_to(&endpoint).expect("binding succeeds");
    let shutdown = daemon.shutdown_handle();
    let thread = std::thread::spawn(move || {
        let _ = daemon.serve(listener);
    });

    Served {
        endpoint,
        project,
        shutdown,
        thread: Some(thread),
        _dir: dir,
    }
}

type RawReader = Box<dyn Read + Send>;
type RawWriter = Box<dyn Write + Send>;

fn raw_client(endpoint: &Path) -> (RawReader, RawWriter) {
    Connection::connect_to(endpoint)
        .expect("the daemon is listening")
        .split()
}

/// Whether writes to the daemon start failing within `patience` -- that is,
/// whether the daemon has let go of the half it reads from.
fn stops_listening(mut writer: RawWriter, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        if Frame::write(&mut writer, &ClientMessage::Ping { token: 0 }).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Connects as a well-behaved interface client and collects what arrives in
/// `window`.
fn subscribe_and_collect(endpoint: &Path, window: Duration) -> Vec<ServerMessage> {
    subscribe_and_collect_until(endpoint, window, |_| false)
}

/// Connects as a well-behaved interface client and collects what arrives
/// until `done` holds of it, or `window` has passed.
fn subscribe_and_collect_until(
    endpoint: &Path,
    window: Duration,
    done: impl Fn(&[ServerMessage]) -> bool,
) -> Vec<ServerMessage> {
    let (mut reader, mut writer) = raw_client(endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");
    Frame::write(&mut writer, &ClientMessage::Subscribe).expect("writing succeeds");

    let (heard, hearing) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(message) = Frame::read::<_, ServerMessage>(&mut reader) {
            if heard.send(message).is_err() {
                return;
            }
        }
    });

    let deadline = Instant::now() + window;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match hearing.recv_timeout(left) {
            Ok(message) => seen.push(message),
            Err(_) => break,
        }
        if done(&seen) {
            break;
        }
    }
    drop(writer);
    seen
}

/// The output bytes among `messages`, however many panes they came from.
///
/// Used only by the budget tests below, which are `#[cfg(unix)]`: without
/// the same gate here, a Windows build has nothing left that calls it.
#[cfg(unix)]
fn output_bytes(messages: &[ServerMessage]) -> usize {
    messages
        .iter()
        .map(|m| match m {
            ServerMessage::PaneOutput { bytes, .. } => bytes.len(),
            _ => 0,
        })
        .sum()
}

/// How long a test waits for something the daemon is expected to do about a
/// client -- hang up on it, or admit the next one.
///
/// Not itself past every budget a test sets: the default `handshake` is
/// exactly this long. A test that cares about a different deadline and
/// must rule the handshake one out raises it clear of `PATIENCE` instead,
/// so a failure cannot be mistaken for the wrong cause.
const PATIENCE: Duration = Duration::from_secs(10);

#[test]
#[cfg(unix)]
fn a_client_that_never_reads_costs_no_more_than_its_budget() {
    const BUDGET: usize = 256 * 1024;
    let (mut daemon, project, _dir) = daemon("unread");
    daemon.set_budgets(Budgets {
        outbox_bytes: BUDGET,
        ..Budgets::default()
    });

    let reading = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let never_reads = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "flood".into(),
            size: (80, 24),
        },
    );

    // Two megabytes reach the client that reads. The other one's queue
    // stops growing at the budget, instead of holding all two.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut delivered = 0;
    while delivered < 2 * 1024 * 1024 {
        assert!(
            Instant::now() < deadline,
            "the flood stopped reaching the client that reads ({delivered} bytes)"
        );
        daemon.tick();
        delivered += output_bytes(&drain(&reading));
        std::thread::sleep(Duration::from_millis(5));
    }

    let backlog = output_bytes(&drain(&never_reads));
    assert!(
        backlog <= BUDGET + dispatch_pty::DRAIN_BUDGET + 8192,
        "{backlog} bytes were queued for a client that never read"
    );
    assert!(
        matches!(
            never_reads.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "the daemon let go of it"
    );
}

/// Every byte of pane output among `messages`, in the order it arrived.
#[cfg(unix)]
fn pane_output(messages: &[ServerMessage]) -> Vec<u8> {
    messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::PaneOutput { bytes, .. } => Some(bytes.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

/// `bytes` without the carriage returns a terminal puts before each newline.
#[cfg(unix)]
fn without_returns(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().copied().filter(|&b| b != b'\r').collect()
}

/// What the `count` harness prints, carriage returns aside.
#[cfg(unix)]
fn counted() -> Vec<u8> {
    (1..=COUNTED)
        .map(|n| format!("{n}\n"))
        .collect::<String>()
        .into_bytes()
}

#[test]
#[cfg(unix)]
fn a_client_that_stops_reading_is_hung_up_and_can_come_back() {
    let served = served(
        "stop-read",
        Budgets {
            outbox_bytes: 256 * 1024,
            ..Budgets::default()
        },
    );

    let (_reader, mut writer) = raw_client(&served.endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");
    Frame::write(&mut writer, &ClientMessage::Subscribe).expect("writing succeeds");
    Frame::write(
        &mut writer,
        &ClientMessage::SpawnPane {
            project: served.project,
            harness: "count".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");

    // Never read: the socket fills, the outbox passes its budget, and the
    // daemon hangs up -- both halves, so this side's writes start failing.
    assert!(
        stops_listening(writer, Duration::from_secs(20)),
        "a client that stopped reading is still connected"
    );

    // Coming back is an ordinary late subscription: the pane is described,
    // and what was missed is replayed -- as much as the daemon keeps, in the
    // order it was printed, with the pane's next output carrying on from
    // exactly where the replay stops.
    let printed = counted();
    let last = format!("{COUNTED}\n");
    // Only the newest messages are looked at: the whole output, gathered
    // afresh for every message, is quadratic in a replay this size.
    let seen = subscribe_and_collect_until(&served.endpoint, Duration::from_secs(20), |m| {
        let start = m.len().saturating_sub(4);
        without_returns(&pane_output(&m[start..])).ends_with(last.as_bytes())
    });
    assert!(
        seen.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { harness, .. } if harness == "count")),
        "the reconnected client is told about the pane"
    );

    let replay = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneOutput { bytes, .. } => Some(bytes.len()),
            _ => None,
        })
        .expect("the pane's history is replayed");
    let received = without_returns(&pane_output(&seen));
    assert!(
        received.ends_with(last.as_bytes()),
        "the pane's output stopped arriving after {} bytes",
        received.len()
    );
    assert!(
        received.len() <= printed.len() && printed.ends_with(&received),
        "the {} bytes a late client was sent are not the last {} the pane printed, in order; \
         the first to differ is at {:?}",
        received.len(),
        received.len(),
        received
            .iter()
            .rev()
            .zip(printed.iter().rev())
            .position(|(got, want)| got != want)
            .map(|from_end| received.len() - 1 - from_end)
    );
    assert!(
        replay == crate::pane::HISTORY_BYTES || received == printed,
        "the replay held back part of the history: {replay} bytes of {}",
        crate::pane::HISTORY_BYTES
    );
}

#[test]
#[cfg(unix)]
fn a_late_subscriber_is_not_hung_up_for_the_replay_it_asked_for() {
    // Small enough that a single pane's full history is several times
    // over it -- the point of the test is that the replay is not judged
    // against this budget at all.
    const BUDGET: usize = 64 * 1024;
    let (mut daemon, project, _dir) = daemon("late-subscribe");
    daemon.set_budgets(Budgets {
        outbox_bytes: BUDGET,
        ..Budgets::default()
    });

    // A reading client keeps the flood's output moving so pump_panes keeps
    // draining it, until the pane's history -- replayed whole to whoever
    // subscribes next -- is full.
    let producer = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "flood".into(),
            size: (80, 24),
        },
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut delivered = 0;
    while delivered < 256 * 1024 {
        assert!(
            Instant::now() < deadline,
            "the flood did not fill the pane's history ({delivered} bytes)"
        );
        daemon.tick();
        delivered += output_bytes(&drain(&producer));
        std::thread::sleep(Duration::from_millis(5));
    }

    // Subscribing now asks for that whole history in one reply -- many
    // times the live-traffic budget above. It must be delivered rather
    // than refused: it is what was asked for, not live traffic.
    let subscriber = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);

    // One tick before anything is drained, with the whole reply still
    // sitting unread: this is exactly the moment a live broadcast used to
    // see the reply's own bulk as backlog and hang the client up for it
    // (round 1, finding 1) -- ticking here, rather than draining first,
    // gives that bug a real chance to happen before this test would ever
    // notice.
    daemon.tick();

    // Drained together, and not counted below: both are what was asked
    // for, or arrived before this client had a chance to read anything --
    // not the ongoing live traffic the loop below measures.
    let replay = drain(&subscriber);
    assert!(
        output_bytes(&replay) >= 256 * 1024,
        "the replay should have carried the pane's full history, got {} bytes",
        output_bytes(&replay)
    );
    assert!(
        !matches!(
            subscriber.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "the daemon hung up on the subscriber before it had read anything"
    );

    // Reading in lock-step, like any subscribed client, it goes on being
    // sent LIVE output well past the live-traffic budget above -- proving
    // that traffic, arriving over time, is not eaten into by the replay
    // already delivered -- and is never hung up, for as long as it reads.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut total = 0;
    while total < 3 * BUDGET {
        assert!(
            Instant::now() < deadline,
            "the subscriber stopped receiving output ({total} bytes)"
        );
        daemon.tick();
        total += output_bytes(&drain(&subscriber));
        assert!(
            !matches!(
                subscriber.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected)
            ),
            "the daemon hung up on a client that was reading"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_client_that_never_reads_its_own_replies_is_hung_up() {
    // Small enough that a loop of tiny replies -- nothing the fleet
    // produced on its own -- passes it well within a handful of iterations.
    const BUDGET: usize = 2 * 1024;
    let (mut daemon, _project, _dir) = daemon("never-reads-replies");
    daemon.set_budgets(Budgets {
        outbox_bytes: BUDGET,
        ..Budgets::default()
    });

    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());

    // Never drained: every `Pong` piles up in what this client asked for,
    // which is bounded too -- or a client could grow its queue forever just
    // by asking, without the fleet doing anything at all.
    for token in 0..500 {
        daemon.request_for_test(1, ClientMessage::Ping { token });
    }

    // Whatever was queued before the hang-up is still there to read; once
    // it runs out, the channel is gone rather than merely empty.
    let _ = drain(&inbox);
    assert!(
        matches!(
            inbox.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "a client that never read its own replies should have been hung up"
    );
}

#[test]
#[cfg(unix)]
fn a_refused_client_is_told_why_before_the_connection_ends() {
    let served = served("refuse-why", Budgets::default());

    let (mut reader, mut writer) = raw_client(&served.endpoint);
    Frame::write(
        &mut writer,
        &ClientMessage::Hello {
            version: dispatch_proto::Version {
                major: 99,
                minor: 0,
            },
            client: "from the future".into(),
            role: dispatch_proto::Role::Interface,
        },
    )
    .expect("writing succeeds");

    // The reason arrives first...
    let reason = Frame::read::<_, ServerMessage>(&mut reader)
        .expect("the daemon answers with why before closing");
    assert!(
        matches!(
            reason,
            ServerMessage::Error {
                error: ProtocolError::IncompatibleVersion { .. }
            }
        ),
        "expected an IncompatibleVersion error, got {reason:?}"
    );

    // ...and only then does the connection itself end, rather than an
    // immediate close racing the write of the refusal and the peer seeing
    // a bare disconnect instead.
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(Frame::read::<_, ServerMessage>(&mut reader).is_err());
    });
    assert!(
        ended.recv_timeout(Duration::from_secs(2)).unwrap_or(false),
        "the connection did not end after the refusal"
    );
}

/// Same guarantee as the test above, under real contention.
///
/// One connection at a time essentially never catches the race a review
/// found in `hang_up`'s immediate close: the write of a small refusal all
/// but always wins against a freshly spawned thread. A handful refused at
/// once give the daemon's per-connection close threads real scheduler
/// contention to race the writer threads under, which reliably does catch
/// it (more at once risks the transport's own connection-pairing limits
/// instead, which are not what this is testing).
#[test]
#[cfg(unix)]
fn many_refused_clients_are_all_told_why_before_the_connection_ends() {
    let served = std::sync::Arc::new(served("refuse-many", Budgets::default()));

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let served = std::sync::Arc::clone(&served);
            std::thread::spawn(move || {
                let (mut reader, mut writer) = raw_client(&served.endpoint);
                Frame::write(
                    &mut writer,
                    &ClientMessage::Hello {
                        version: dispatch_proto::Version {
                            major: 99,
                            minor: 0,
                        },
                        client: "from the future".into(),
                        role: dispatch_proto::Role::Interface,
                    },
                )
                .expect("writing succeeds");

                let reason = Frame::read::<_, ServerMessage>(&mut reader);
                assert!(
                    matches!(
                        reason,
                        Ok(ServerMessage::Error {
                            error: ProtocolError::IncompatibleVersion { .. }
                        })
                    ),
                    "connection {i}: expected an IncompatibleVersion error, got {reason:?}"
                );
            })
        })
        .collect();

    for handle in handles {
        handle
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
    }
}

/// Whether the daemon ends the connection `reader` reads from within
/// `patience`, discarding whatever arrives first.
fn hung_up(mut reader: RawReader, patience: Duration) -> bool {
    let (ended, end) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while Frame::read::<_, ServerMessage>(&mut reader).is_ok() {}
        let _ = ended.send(());
    });
    end.recv_timeout(patience).is_ok()
}

#[test]
fn a_refused_client_is_hung_up_on_and_its_pipelined_request_ignored() {
    // A Hello the daemon refuses, with a SpawnPane right behind it in the
    // same burst: the shape that got a pane started for a refused client.
    let served = served("refused", Budgets::default());
    let (mut reader, mut writer) = raw_client(&served.endpoint);

    Frame::write(
        &mut writer,
        &ClientMessage::Hello {
            version: dispatch_proto::Version {
                major: 99,
                minor: 0,
            },
            client: "future".into(),
            role: dispatch_proto::Role::Interface,
        },
    )
    .expect("writing succeeds");
    let _ = Frame::write(&mut writer, &spawn_request(served.project));

    let answer: ServerMessage = Frame::read(&mut reader).expect("refused out loud");
    assert!(matches!(
        answer,
        ServerMessage::Error {
            error: ProtocolError::IncompatibleVersion { .. }
        }
    ));
    assert!(hung_up(reader, PATIENCE), "the half it reads was left open");
    assert!(
        stops_listening(writer, PATIENCE),
        "the half it writes to was left open"
    );

    let seen = subscribe_and_collect(&served.endpoint, Duration::from_millis(500));
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. })),
        "the refused client's request was acted on: {seen:#?}"
    );
}

#[test]
fn a_client_that_never_says_hello_is_hung_up_on() {
    let served = served(
        "no-hello",
        Budgets {
            handshake: Duration::from_millis(200),
            ..Budgets::default()
        },
    );
    let (reader, _writer) = raw_client(&served.endpoint);

    assert!(
        hung_up(reader, PATIENCE),
        "a silent client is still connected"
    );
}

#[test]
fn a_client_that_stops_mid_frame_is_hung_up_on() {
    let served = served(
        "mid-frame",
        Budgets {
            frame: Duration::from_millis(200),
            ..Budgets::default()
        },
    );
    let (reader, mut writer) = raw_client(&served.endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");

    // A length prefix promising a hundred bytes, then three of them.
    writer
        .write_all(&100u32.to_be_bytes())
        .and_then(|()| writer.write_all(b"abc"))
        .and_then(|()| writer.flush())
        .expect("writing succeeds");

    assert!(
        hung_up(reader, PATIENCE),
        "a client stalled part-way through a frame is still connected"
    );
}

#[test]
fn clients_past_the_limit_are_turned_away() {
    let served = served(
        "quota",
        Budgets {
            max_clients: 2,
            // Clear of PATIENCE, which equals the default: otherwise a
            // broken quota could be masked by the handshake deadline
            // hanging the third client up on its own, and the assertion
            // below would point at the wrong cause.
            handshake: Duration::from_secs(60),
            ..Budgets::default()
        },
    );

    let first = raw_client(&served.endpoint);
    let second = raw_client(&served.endpoint);
    // Both must be counted before the third arrives.
    std::thread::sleep(Duration::from_millis(200));

    let (third_reader, _third_writer) = raw_client(&served.endpoint);
    assert!(
        hung_up(third_reader, PATIENCE),
        "a third client was let in past a limit of two"
    );

    // The two already in are unaffected.
    for (mut reader, mut writer) in [first, second] {
        Frame::write(&mut writer, &hello()).expect("writing succeeds");
        let answer: ServerMessage = Frame::read(&mut reader).expect("still served");
        assert!(matches!(answer, ServerMessage::Welcome { .. }));
    }
}

/// Round 1, finding 1(a): a client that disconnects cleanly must free its
/// seat, not merely stop being able to use it.
#[test]
fn a_seat_freed_by_a_disconnected_client_admits_the_next_one() {
    let served = served(
        "seat-freed",
        Budgets {
            max_clients: 1,
            ..Budgets::default()
        },
    );

    {
        let (mut reader, mut writer) = raw_client(&served.endpoint);
        Frame::write(&mut writer, &hello()).expect("writing succeeds");
        let welcome: ServerMessage = Frame::read(&mut reader).expect("welcomed");
        assert!(matches!(welcome, ServerMessage::Welcome { .. }));
        // Both halves drop here, disconnecting.
    }

    // Retried rather than tried once: freeing the seat is asynchronous with
    // this end noticing the first client is gone, so the very next connect
    // attempt can still land before the daemon has caught up.
    let deadline = Instant::now() + PATIENCE;
    let mut admitted = false;
    while Instant::now() < deadline && !admitted {
        let (mut reader, mut writer) = raw_client(&served.endpoint);
        if Frame::write(&mut writer, &hello()).is_ok()
            && matches!(
                Frame::read::<_, ServerMessage>(&mut reader),
                Ok(ServerMessage::Welcome { .. })
            )
        {
            admitted = true;
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    assert!(
        admitted,
        "the seat freed by the first client was never reused"
    );
}

/// Round 1, finding 1(b): the audit's probe. A client whose reader has
/// ended -- an ordinary disconnect, from the daemon's point of view -- can
/// still be holding its seat open through a writer stuck delivering to it,
/// if freeing the seat is counted per thread rather than per connection.
#[test]
#[cfg(unix)]
fn a_seat_held_by_a_stuck_writer_is_freed_once_the_daemon_notices() {
    let served = served(
        "stuck-writer",
        Budgets {
            max_clients: 1,
            ..Budgets::default()
        },
    );

    let (reader, mut writer) = raw_client(&served.endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");
    Frame::write(&mut writer, &ClientMessage::Subscribe).expect("writing succeeds");
    Frame::write(
        &mut writer,
        &ClientMessage::SpawnPane {
            project: served.project,
            harness: "flood".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");

    // Long enough for the pane to spawn and flood the socket with output
    // nothing here is draining, so the daemon's writer thread to this
    // client is genuinely stuck mid-write -- not merely queued -- by the
    // time the read half closes below. The default `outbox_bytes` (32 MiB)
    // is left in place so that budget cannot hang the client up on its own
    // first: what this test means to catch is the reader ending while the
    // writer is still blocked on the OS socket, not the daemon's own
    // live-traffic limit.
    std::thread::sleep(Duration::from_secs(1));

    // Shuts down only the half the daemon reads from -- an ordinary
    // disconnect to its reader thread -- while its writer, blocked as
    // above, is left mid-write. Never reading from `reader` is what keeps
    // it that way; it stays open, not dropped, until the loop below no
    // longer needs it.
    drop(writer);

    let deadline = Instant::now() + PATIENCE;
    let mut admitted = false;
    while Instant::now() < deadline && !admitted {
        let (mut second_reader, mut second_writer) = raw_client(&served.endpoint);
        if Frame::write(&mut second_writer, &hello()).is_ok()
            && matches!(
                Frame::read::<_, ServerMessage>(&mut second_reader),
                Ok(ServerMessage::Welcome { .. })
            )
        {
            admitted = true;
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    drop(reader);
    assert!(
        admitted,
        "a second client was never admitted past the limit of one"
    );
}

/// Round 1, finding 3: `enforce_deadlines` must not mistake an idle,
/// already-welcomed client for one that is late -- only a `Hello` not yet
/// said, or a frame started and not finished, is a deadline at all.
#[test]
fn a_ready_client_left_idle_past_its_budgets_is_still_served() {
    let served = served(
        "idle-ready",
        Budgets {
            frame: Duration::from_millis(200),
            handshake: Duration::from_millis(200),
            ..Budgets::default()
        },
    );

    let (mut reader, mut writer) = raw_client(&served.endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");
    let welcome: ServerMessage = Frame::read(&mut reader).expect("welcomed");
    assert!(matches!(welcome, ServerMessage::Welcome { .. }));

    // Well past both budgets above, with nothing sent in between.
    std::thread::sleep(Duration::from_secs(1));

    Frame::write(&mut writer, &ClientMessage::Ping { token: 7 }).expect("writing succeeds");
    let answer: ServerMessage =
        Frame::read(&mut reader).expect("an idle, welcomed client is still served");
    assert!(matches!(answer, ServerMessage::Pong { token: 7 }));
}

/// Round 2, finding 1, assertion 2: pins the both-threads seat directly,
/// independent of `Daemon::hang_up`. A client refused mid-flood is only
/// forgotten by `Daemon::refuse`, which never closes its connection; its
/// writer, already genuinely stuck delivering to it, is left running all
/// the same, and its seat must stay held for exactly as long as that writer
/// does -- not released early just because the client's reader, separately,
/// has ended.
#[test]
#[cfg(unix)]
fn a_seat_held_by_a_refused_clients_stuck_writer_is_not_released_early() {
    let served = served(
        "refused-stuck",
        Budgets {
            max_clients: 1,
            ..Budgets::default()
        },
    );

    let (reader, mut writer) = raw_client(&served.endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");
    Frame::write(&mut writer, &ClientMessage::Subscribe).expect("writing succeeds");
    Frame::write(
        &mut writer,
        &ClientMessage::SpawnPane {
            project: served.project,
            harness: "flood".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");

    // Never read; long enough for the writer to genuinely block on the
    // socket, same reasoning as the test above.
    std::thread::sleep(Duration::from_secs(1));

    // A second `Hello`, sent now that this client is already welcomed, is
    // refused for its version -- `Daemon::refuse`, not `Daemon::hang_up`:
    // it forgets the client but does not touch its connection, relying on
    // the writer thread to end on its own once nothing is left queued or a
    // write fails. A writer genuinely stuck mid-write does neither, which
    // is exactly the shape that would let a reader-only seat free itself
    // the moment the read half closes next, though nothing has actually
    // ended.
    Frame::write(
        &mut writer,
        &ClientMessage::Hello {
            version: dispatch_proto::Version {
                major: 99,
                minor: 0,
            },
            client: "future".into(),
            role: dispatch_proto::Role::Interface,
        },
    )
    .expect("writing succeeds");
    drop(writer);

    // Closing only the read half does not touch the OS-level write the
    // daemon's writer is blocked on, so the seat must still be held: no
    // second client is admitted for as long as this window runs.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut admitted = false;
    while Instant::now() < deadline && !admitted {
        let (mut second_reader, mut second_writer) = raw_client(&served.endpoint);
        if Frame::write(&mut second_writer, &hello()).is_ok()
            && matches!(
                Frame::read::<_, ServerMessage>(&mut second_reader),
                Ok(ServerMessage::Welcome { .. })
            )
        {
            admitted = true;
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    assert!(
        !admitted,
        "a second client was welcomed while the first's writer was still stuck"
    );

    // Only once the first client's other half closes too does its writer's
    // blocked write finally fail, freeing the seat for real.
    drop(reader);

    let deadline = Instant::now() + PATIENCE;
    let mut admitted = false;
    while Instant::now() < deadline && !admitted {
        let (mut second_reader, mut second_writer) = raw_client(&served.endpoint);
        if Frame::write(&mut second_writer, &hello()).is_ok()
            && matches!(
                Frame::read::<_, ServerMessage>(&mut second_reader),
                Ok(ServerMessage::Welcome { .. })
            )
        {
            admitted = true;
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    assert!(
        admitted,
        "a second client was never admitted once the first was fully gone"
    );
}

#[test]
fn shutting_down_ends_every_panes_whole_tree() {
    let (mut daemon, project, _dir) = daemon("tree-down");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "tree".into(),
            size: (80, 24),
        },
    );
    wait_for(&mut daemon, &inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    let pid = daemon
        .pane_pids_for_test()
        .into_iter()
        .next()
        .expect("the pane has a pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    let everyone = loop {
        let below = dispatch_os::process::descendants(pid);
        if !below.is_empty() {
            break std::iter::once(pid).chain(below).collect::<Vec<_>>();
        }
        assert!(
            Instant::now() < deadline,
            "the pane never started its children"
        );
        std::thread::sleep(Duration::from_millis(20));
    };

    daemon.shutdown_handle().request();
    daemon.run();

    let deadline = Instant::now() + Duration::from_secs(10);
    while everyone
        .iter()
        .any(|p| dispatch_os::process::is_running(*p))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        everyone
            .iter()
            .all(|p| !dispatch_os::process::is_running(*p)),
        "a process a pane started outlived the daemon: {everyone:?}"
    );
}

/// Not a test of its own: what the `capture` harness runs inside a pane, to
/// record exactly what reached its standard input. Run without
/// `DISPATCH_CAPTURE_TO`, it does nothing.
#[test]
fn capture_standard_input() {
    let Some(out) = std::env::var_os("DISPATCH_CAPTURE_TO") else {
        return;
    };
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .expect("standard input is readable");
    std::fs::write(out, bytes).expect("the capture is writable");
}

/// Delegates `task` to a `capture` harness started as `command`, whose
/// `[task]` arguments `args` spells out given the binary that records what
/// it is sent, and checks that the task reached that binary's standard input
/// byte for byte, that nothing in it ran, and that its file went with the
/// run.
///
/// The agent is this test binary running [`capture_standard_input`], so what
/// it received can be compared with what was asked.
fn assert_a_task_reaches_the_capture_exactly(
    label: &str,
    command: &str,
    args: impl Fn(&Path) -> String,
    task: &str,
) {
    let dir = TempDir::new(label);
    let harness_dir = dir.0.join("harnesses");
    let _ = harnesses(&harness_dir);

    let captured = dir.0.join("captured.bin");
    let exe = std::env::current_exe().expect("the test binary");
    // A harness's own spelling of the task file's variable, naming another
    // file. On Windows it is the same variable, and the task's file has to
    // win it; elsewhere it is another variable, and nothing reads it.
    let decoy = dir.0.join("decoy.txt");
    std::fs::write(&decoy, "not the task").expect("temp dir is writable");
    std::fs::write(
        harness_dir.join("capture.toml"),
        format!(
            "id = \"capture\"\ndisplay_name = \"Capture\"\ncommand = \"{command}\"\n\n\
             [env]\nDISPATCH_CAPTURE_TO = '{}'\nDispatch_Task_File = '{}'\n\n\
             [task]\nargs = {}\ninput = \"file\"\n",
            captured.display(),
            decoy.display(),
            args(&exe)
        ),
    )
    .expect("temp dir is writable");

    let registry = HarnessRegistry::load_from_dir(&harness_dir).expect("loading succeeds");
    let mut daemon = Daemon::new(registry, "test-device");
    // Every character cmd.exe would act on outside quotes -- a space, as in
    // many Windows profile paths, `&`, `(`, `)`, `^`, a `%VAR%` and a
    // `!VAR!` -- so the file's path is shown to reach cmd.exe's `<` whole,
    // quoted and expanded once.
    let task_dir = dir.0.join("task files & (x) ^ %PATH% !y!");
    daemon.set_task_dir(task_dir.clone());
    let project =
        daemon.open_project(dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves"));

    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent,
            harness: "capture".into(),
            task: task.into(),
            size: (80, 24),
        },
    );
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );

    let seen = wait_for(&mut daemon, &caller, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });
    assert!(
        seen.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { exit: 0, .. })),
        "the capture ran and succeeded: {seen:#?}"
    );

    assert_eq!(
        std::fs::read(&captured).expect("the agent recorded its input"),
        task.as_bytes(),
        "the task arrived changed"
    );
    assert!(
        !dir.0.join("marker.txt").exists(),
        "a command inside the task ran"
    );
    assert!(
        std::fs::read_dir(&task_dir).map_or(true, |mut d| d.next().is_none()),
        "the task's file outlived the run"
    );
}

#[test]
#[cfg_attr(not(windows), ignore = "exercises cmd.exe")]
fn a_task_reaches_a_cmd_wrapped_agent_exactly() {
    // The audit's A01: through `cmd.exe /c agent {task}`, `&` in a task ran
    // a second command. Here the agent is this test binary, reached through
    // cmd.exe exactly as claude.cmd is, recording what its standard input
    // received.
    assert_a_task_reaches_the_capture_exactly(
        "cmd-task",
        "cmd.exe",
        |exe| {
            format!(
                "[\"/d\", \"/v:off\", \"/c\", '{}', \"--exact\", \
                 \"session::tests::capture_standard_input\", \"--nocapture\", \
                 \"<%DISPATCH_TASK_FILE%\"]",
                exe.display()
            )
        },
        "literal & echo DISPATCH_AUDIT_MARKER> marker.txt | \"quoted\" %PATH% !PATH! ^caret\r\n\
         second line \u{fc}n\u{ef}c\u{f8}d\u{e9} \u{2713}",
    );
}

#[test]
#[cfg(unix)]
fn a_task_redirected_by_a_posix_shell_reaches_the_agent_exactly() {
    // The same delivery where it can run on every machine: the daemon writes
    // the task down, names the file in the environment, and removes it once
    // the run is over. Here `sh` does the redirecting, quoting the variable
    // itself.
    assert_a_task_reaches_the_capture_exactly(
        "sh-task",
        "sh",
        |exe| {
            format!(
                "[\"-c\", 'exec \"$0\" --exact session::tests::capture_standard_input \
                 --nocapture < \"$DISPATCH_TASK_FILE\"', '{}']",
                exe.display()
            )
        },
        "literal; echo DISPATCH_AUDIT_MARKER > marker.txt $(touch marker.txt) `touch marker.txt` \
         \"quoted\" '$HOME'\r\nsecond line \u{fc}n\u{ef}c\u{f8}d\u{e9} \u{2713}",
    );
}

/// A daemon serving the fixture harnesses plus `extra`, with a parent pane
/// and a delegate caller attached, and the request for `task` under
/// `harness` already made.
///
/// Returns the daemon, the interface client, the caller, and the test's
/// directory, which the daemon's project and task files live in.
fn delegating_to(
    label: &str,
    extra: &[(&str, String)],
    harness: &str,
    task: &str,
) -> (Daemon, Inbox, Inbox, TempDir) {
    let dir = TempDir::new(label);
    let harness_dir = dir.0.join("harnesses");
    let _ = harnesses(&harness_dir);
    for (name, body) in extra {
        std::fs::write(harness_dir.join(format!("{name}.toml")), body)
            .expect("temp dir is writable");
    }

    let registry = HarnessRegistry::load_from_dir(&harness_dir).expect("loading succeeds");
    let mut daemon = Daemon::new(registry, "test-device");
    daemon.set_task_dir(dir.0.join("tasks"));
    let project =
        daemon.open_project(dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves"));
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent,
            harness: harness.into(),
            task: task.into(),
            size: (80, 24),
        },
    );

    (daemon, ui, caller, dir)
}

/// A harness whose command is the bare name `agent`, looked for only in
/// `bin`, where `.CMD` completes it: what `claude` written bare is on a
/// machine where npm installed `claude.cmd`.
fn bare_agent(bin: &Path) -> String {
    format!(
        "id = \"agent\"\ndisplay_name = \"Agent\"\ncommand = \"agent\"\n\n\
         [env]\nPATH = '{}'\nPATHEXT = \".CMD\"\n\n\
         [task]\nargs = [\"{{task}}\"]\n",
        bin.display()
    )
}

#[test]
#[cfg_attr(
    not(windows),
    ignore = "a batch file runs through cmd.exe only on Windows"
)]
fn a_command_windows_finds_as_a_batch_file_is_refused_before_anyone_is_asked() {
    let bin = TempDir::new("batch-bin");
    std::fs::write(bin.0.join("agent.CMD"), "@echo ran\r\n").expect("temp dir is writable");

    let (daemon, ui, caller, _dir) = delegating_to(
        "batch-on-path",
        &[("agent", bare_agent(&bin.0))],
        "agent",
        "x & echo DISPATCH_AUDIT_MARKER",
    );

    let told = outcomes(&drain(&caller));
    assert!(
        told.iter().any(|o| matches!(
            o,
            DelegateOutcome::Refused { reason }
                if reason.contains("agent.toml") && reason.to_lowercase().contains("agent.cmd")
        )),
        "the caller is told what was found and which file to fix: {told:?}"
    );
    assert!(
        pending(&drain(&ui)).is_none(),
        "nobody is asked to approve it"
    );
    assert_eq!(daemon.pane_count(), 1, "nothing started");
}

#[test]
#[cfg_attr(
    not(windows),
    ignore = "a batch file runs through cmd.exe only on Windows"
)]
fn a_command_that_becomes_a_batch_file_while_asking_is_refused_at_approval() {
    // Judged when the request arrived, `agent` found nothing. By the time
    // the user approves, `agent.CMD` is there -- and what starts is decided
    // when it starts.
    let bin = TempDir::new("late-bin");
    let (mut daemon, ui, caller, dir) = delegating_to(
        "late-batch",
        &[("agent", bare_agent(&bin.0))],
        "agent",
        // No space, so no quotes around it on the command line: the shape
        // that ran a second command.
        "x&echo.DISPATCH_AUDIT_MARKER>marker.txt",
    );
    let request = pending(&drain(&ui)).expect("nothing is a batch file yet, so the user is asked");

    std::fs::write(bin.0.join("agent.CMD"), "@echo ran\r\n").expect("temp dir is writable");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );

    let told = outcomes(&drain(&caller));
    assert!(
        told.iter().any(|o| matches!(
            o,
            DelegateOutcome::Refused { reason } if reason.to_lowercase().contains("agent.cmd")
        )),
        "the approval is judged again on what would start: {told:?}"
    );
    assert_eq!(daemon.pane_count(), 1, "nothing started");
    // Given time to have run, had it started.
    for _ in 0..50 {
        daemon.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !dir.0.join("marker.txt").exists(),
        "a command inside the task ran"
    );
}

/// A daemon whose delegated pane was closed while its task's file could not
/// be removed, as on Windows while a killed agent still holds it open: here
/// the directory is made read-only for the close.
///
/// Returns the daemon, the task directory (read-only again only if the
/// caller makes it so), the file left behind, and the test's directory. `None`
/// when permissions stop nobody, as for root.
#[cfg(unix)]
fn with_a_task_file_the_close_left(label: &str) -> Option<(Daemon, PathBuf, PathBuf, TempDir)> {
    use std::os::unix::fs::PermissionsExt;

    let hold = "id = \"hold\"\ndisplay_name = \"Hold\"\ncommand = \"sh\"\n\n\
                [task]\nargs = [\"-c\", 'exec sleep 30 < \"$DISPATCH_TASK_FILE\"']\n\
                input = \"file\"\n";
    let (mut daemon, ui, caller, dir) =
        delegating_to(label, &[("hold", hold.to_string())], "hold", "a task");
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );
    let pane = outcomes(&drain(&caller))
        .into_iter()
        .find_map(|o| match o {
            DelegateOutcome::Approved { pane } => Some(pane),
            _ => None,
        })
        .expect("the subagent started");

    let tasks = dir.0.join("tasks");
    let file = std::fs::read_dir(&tasks)
        .expect("the task directory exists")
        .flatten()
        .map(|entry| entry.path())
        .next()
        .expect("the task was written down");

    let set = |mode| {
        std::fs::set_permissions(&tasks, std::fs::Permissions::from_mode(mode))
            .expect("permissions change");
    };
    set(0o500);
    if std::fs::write(tasks.join("probe"), "").is_ok() {
        set(0o700);
        eprintln!("skipped: permissions do not stop this user writing");
        return None;
    }

    daemon.request_for_test(1, ClientMessage::ClosePane { pane });
    assert!(file.exists(), "the close could remove it after all");
    set(0o700);

    Some((daemon, tasks, file, dir))
}

#[test]
#[cfg(unix)]
fn a_task_file_a_closed_pane_left_is_removed_on_a_later_pass() {
    let Some((mut daemon, _tasks, file, _dir)) = with_a_task_file_the_close_left("retry-pass")
    else {
        return;
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    while file.exists() && Instant::now() < deadline {
        daemon.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!file.exists(), "the task's file is still there");
}

#[test]
#[cfg(unix)]
fn a_task_file_a_closed_pane_left_is_removed_at_shutdown() {
    let Some((mut daemon, _tasks, file, _dir)) = with_a_task_file_the_close_left("retry-shutdown")
    else {
        return;
    };

    // Asked to stop before the loop takes a single pass: only the way out
    // is left to try again.
    daemon.shutdown_handle().request();
    daemon.run();

    assert!(!file.exists(), "the task's file outlived the daemon");
}

#[test]
fn a_daemon_that_starts_serving_clears_away_task_files_left_behind() {
    // A daemon that was killed never dropped its panes, so their task files
    // stayed. The next to serve this configuration is the only one that
    // could clear them, and nothing still means to hand them over. Only
    // names Dispatch gives are touched.
    let (daemon, _project, dir) = daemon("sweep");
    let tasks = dir.0.join("tasks");
    // Made as the daemon makes it. Made any other way on Windows under an
    // elevated account -- as CI runs -- the Administrators group would own
    // it, and the daemon rightly refuses a directory this user does not.
    dispatch_os::paths::create_private_dir(&tasks).expect("temp dir is writable");
    let left = tasks.join(format!(
        "dispatch-task-{}.txt",
        dispatch_core::RequestId::new()
    ));
    let lookalike = tasks.join("dispatch-task-notes.txt");
    let unrelated = tasks.join("notes.txt");
    for path in [&left, &lookalike, &unrelated] {
        std::fs::write(path, "a task").expect("temp dir is writable");
    }

    let listener = Listener::bind_to(&dir.0.join("d.sock")).expect("binding succeeds");
    let shutdown = daemon.shutdown_handle();
    let serving = std::thread::spawn(move || {
        let _ = daemon.serve(listener);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while left.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    shutdown.request();
    serving.join().expect("the daemon stops");

    assert!(!left.exists(), "a task file left behind is still there");
    assert!(
        lookalike.exists() && unrelated.exists(),
        "a file Dispatch did not name was removed"
    );
}

#[test]
#[cfg(unix)]
fn the_sweep_never_reads_through_a_linked_directory() {
    // What a link names is somebody's choice, not Dispatch's directory.
    let dir = TempDir::new("sweep-link");
    let elsewhere = dir.0.join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("temp dir is writable");
    let left = elsewhere.join(format!(
        "dispatch-task-{}.txt",
        dispatch_core::RequestId::new()
    ));
    std::fs::write(&left, "a task").expect("temp dir is writable");
    let tasks = dir.0.join("tasks");
    std::os::unix::fs::symlink(&elsewhere, &tasks).expect("the file system links");

    crate::task_file::sweep(&tasks);

    assert!(left.exists(), "the sweep reached through the link");
}

#[test]
fn only_the_daemon_holding_the_task_directory_sweeps_it() {
    // Two daemons can share a task directory -- on Linux, different
    // XDG_CONFIG_HOMEs and one XDG_DATA_HOME -- and bind different endpoints,
    // so binding proves nothing about the directory. Its lock does.
    let dir = TempDir::new("sweep-lock");
    let tasks = dir.0.join("tasks");
    // Made as the daemon makes it. Made any other way on Windows under an
    // elevated account -- as CI runs -- the Administrators group would own
    // it, and the daemon rightly refuses a directory this user does not.
    dispatch_os::paths::create_private_dir(&tasks).expect("temp dir is writable");
    let left = tasks.join(format!(
        "dispatch-task-{}.txt",
        dispatch_core::RequestId::new()
    ));
    std::fs::write(&left, "another daemon's live task").expect("temp dir is writable");

    let other = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(tasks.join(crate::task_file::LOCK_FILE))
        .expect("the lock file opens");
    other.try_lock().expect("nobody holds it yet");

    assert!(
        crate::task_file::claim(&tasks).is_none(),
        "the directory was claimed while another daemon held it"
    );
    assert!(left.exists(), "another daemon's task file was swept");

    drop(other);
    let held = crate::task_file::claim(&tasks);
    assert!(held.is_some(), "a free directory was not claimed");
    assert!(!left.exists(), "the holder did not sweep");
}

#[test]
fn a_harness_spelling_of_a_daemon_variable_is_the_one_its_agent_gets() {
    // On Windows `Dispatch_Pane` and `DISPATCH_PANE` are one variable, so
    // the harness's spelling replaces the daemon's rather than sitting
    // beside it for the spawn to choose between. Elsewhere they are two.
    let spelled = "id = \"spelled\"\ndisplay_name = \"Spelled\"\ncommand = \"agent\"\n\n\
                   [env]\nDispatch_Pane = \"the harness's\"\n\n\
                   [task]\nargs = [\"{task}\"]\n";
    let (daemon, _ui, _caller, _dir) = delegating_to(
        "case-variant-env",
        &[("spelled", spelled.to_string())],
        "spelled",
        "anything",
    );

    let run = daemon
        .task_run("spelled", "anything", PaneId::new())
        .expect("the harness has a task form");
    let spellings: Vec<_> = run
        .launch
        .env
        .keys()
        .filter(|name| name.eq_ignore_ascii_case("DISPATCH_PANE"))
        .map(String::as_str)
        .collect();

    if cfg!(windows) {
        assert_eq!(spellings, ["Dispatch_Pane"], "one variable, the harness's");
    } else {
        assert_eq!(
            spellings,
            ["DISPATCH_PANE", "Dispatch_Pane"],
            "two variables"
        );
    }
    assert_eq!(
        run.launch.env.get("Dispatch_Pane").map(String::as_str),
        Some("the harness's")
    );
}

#[test]
#[cfg_attr(
    not(windows),
    ignore = "a batch file runs through cmd.exe only on Windows"
)]
fn a_batch_file_on_a_path_spelled_as_windows_spells_it_is_refused() {
    // `Path` is how Windows itself spells it, and so how a harness written
    // there does.
    let bin = TempDir::new("spelled-bin");
    std::fs::write(bin.0.join("agent.CMD"), "@echo ran\r\n").expect("temp dir is writable");
    let agent = bare_agent(&bin.0).replace("[env]\nPATH = ", "[env]\nPath = ");
    assert!(agent.contains("Path = "), "the fixture spells it Path");

    let (daemon, ui, caller, _dir) =
        delegating_to("spelled-path", &[("agent", agent)], "agent", "anything");

    let told = outcomes(&drain(&caller));
    assert!(
        told.iter().any(|o| matches!(
            o,
            DelegateOutcome::Refused { reason } if reason.to_lowercase().contains("agent.cmd")
        )),
        "the batch file on Path is refused: {told:?}"
    );
    assert!(
        pending(&drain(&ui)).is_none(),
        "nobody is asked to approve it"
    );
    assert_eq!(daemon.pane_count(), 1, "nothing started");
}

#[test]
fn an_argument_form_is_never_handed_a_task_file() {
    // Its task is in its arguments. A DISPATCH_TASK_FILE in its environment
    // could only be stale -- inherited, or set in the harness file -- and a
    // redirect written against it would read some other file.
    let argue = "id = \"argue\"\ndisplay_name = \"Argue\"\ncommand = \"agent\"\n\n\
                 [env]\nDISPATCH_TASK_FILE = \"elsewhere.txt\"\n\n\
                 [task]\nargs = [\"{task}\"]\n";
    let (daemon, _ui, _caller, _dir) = delegating_to(
        "argument-form-env",
        &[("argue", argue.to_string())],
        "argue",
        "anything",
    );

    let run = daemon
        .task_run("argue", "anything", PaneId::new())
        .expect("the harness has a task form");
    assert_eq!(run.input, TaskInput::Argument);
    assert!(
        !run.launch.env.contains_key(dispatch_config::TASK_FILE_ENV),
        "the harness file's value is passed on: {:?}",
        run.launch.env
    );
    assert!(
        run.launch.unset.contains(dispatch_config::TASK_FILE_ENV),
        "a value this process holds would be inherited"
    );
}

#[test]
fn a_file_form_that_also_names_the_task_is_refused_before_anyone_is_asked() {
    // On every platform: a file form fills nothing in, so its `{task}` would
    // reach the agent as those six characters.
    let dir = TempDir::new("mixed-form");
    let harness_dir = dir.0.join("harnesses");
    let _ = harnesses(&harness_dir);
    std::fs::write(
        harness_dir.join("mixed.toml"),
        "id = \"mixed\"\ndisplay_name = \"Mixed\"\ncommand = \"agent\"\n\n\
         [task]\nargs = [\"-p\", \"{task}\"]\ninput = \"file\"\n",
    )
    .expect("temp dir is writable");

    let registry = HarnessRegistry::load_from_dir(&harness_dir).expect("loading succeeds");
    let mut daemon = Daemon::new(registry, "test-device");
    daemon.set_task_dir(dir.0.join("tasks"));
    let project =
        daemon.open_project(dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves"));
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent,
            harness: "mixed".into(),
            task: "anything".into(),
            size: (80, 24),
        },
    );

    let told = outcomes(&drain(&caller));
    assert!(
        told.iter().any(|o| matches!(
            o,
            DelegateOutcome::Refused { reason } if reason.contains("mixed.toml") && reason.contains("{task}")
        )),
        "the caller is told which file to fix: {told:?}"
    );
    assert!(
        pending(&drain(&ui)).is_none(),
        "nobody is asked to approve it"
    );
    assert_eq!(daemon.pane_count(), 1, "nothing started");
}

#[test]
#[cfg_attr(
    not(windows),
    ignore = "the refusal is for cmd.exe, which only Windows has"
)]
fn a_task_form_that_would_put_the_task_on_cmds_command_line_is_refused() {
    // What every Windows installation from before this fix still has in any
    // harness file its user edited.
    let dir = TempDir::new("unsafe-form");
    let harness_dir = dir.0.join("harnesses");
    let _ = harnesses(&harness_dir);
    std::fs::write(
        harness_dir.join("old.toml"),
        "id = \"old\"\ndisplay_name = \"Old\"\ncommand = \"cmd.exe\"\n\n[task]\nargs = [\"/c\", \"{task}\"]\n",
    )
    .expect("temp dir is writable");

    let registry = HarnessRegistry::load_from_dir(&harness_dir).expect("loading succeeds");
    let mut daemon = Daemon::new(registry, "test-device");
    daemon.set_task_dir(dir.0.join("tasks"));
    let project =
        daemon.open_project(dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves"));
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent,
            harness: "old".into(),
            task: "x & echo DISPATCH_AUDIT_MARKER".into(),
            size: (80, 24),
        },
    );

    let told = outcomes(&drain(&caller));
    assert!(
        told.iter().any(|o| matches!(
            o,
            DelegateOutcome::Refused { reason } if reason.contains("old.toml") && reason.contains("cmd.exe")
        )),
        "the caller is told which file to fix: {told:?}"
    );
    assert!(
        pending(&drain(&ui)).is_none(),
        "nobody is asked to approve it"
    );
    assert_eq!(daemon.pane_count(), 1, "nothing started");
}
