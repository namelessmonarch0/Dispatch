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
/// about.
///
/// Also registers `no-task-args`: a harness with a `[task]` section but an
/// empty `args`, which `HarnessDef::task_launch` treats as no form at all — a
/// fixture for the difference between `task.is_some()` and
/// `task_launch(..).is_some()`.
fn harnesses(dir: &std::path::Path) -> HarnessRegistry {
    let body = if cfg!(windows) {
        "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"cmd.exe\"\n\n[task]\nargs = [\"/c\", \"{task}\"]\n"
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
    // paste arrives.
    let stall = if cfg!(windows) {
        "id = \"stall\"\ndisplay_name = \"Stall\"\ncommand = \"cmd.exe\"\nargs = [\"/c\", \"echo READY & ping -n 30 127.0.0.1 >nul\"]\n"
    } else {
        "id = \"stall\"\ndisplay_name = \"Stall\"\ncommand = \"sh\"\nargs = [\"-c\", \"stty raw -echo; echo READY; sleep 30\"]\n"
    };
    std::fs::write(dir.join("stall.toml"), stall).expect("temp dir is writable");

    HarnessRegistry::load_from_dir(dir).expect("loading succeeds")
}

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
fn drain(inbox: &Receiver<ServerMessage>) -> Vec<ServerMessage> {
    let mut messages = Vec::new();
    while let Ok(message) = inbox.try_recv() {
        messages.push(message);
    }
    messages
}

/// Ticks the daemon until `predicate` holds, or gives up.
fn wait_for(
    daemon: &mut Daemon,
    inbox: &Receiver<ServerMessage>,
    predicate: impl Fn(&[ServerMessage]) -> bool,
) -> Vec<ServerMessage> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);

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

    let saw_spawn = |inbox: &Receiver<ServerMessage>| {
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
    let (done, finished) = channel();
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
    let root = dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves");
    let project = daemon.open_project(root);

    (daemon, project, dir)
}

/// Spawns a pane the ordinary way and returns its id.
fn spawn_pane_for_test(
    daemon: &mut Daemon,
    inbox: &Receiver<ServerMessage>,
    project: ProjectId,
) -> PaneId {
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
fn ask(daemon: &mut Daemon, parent: PaneId, task: &str) -> Receiver<ServerMessage> {
    ask_as(daemon, 9, parent, task)
}

/// Attaches a delegate caller under a specific client id and asks for a
/// subagent. Needed over `ask` when a test drives two delegate callers at
/// once, since `ask` always reuses id 9.
fn ask_as(daemon: &mut Daemon, id: u64, parent: PaneId, task: &str) -> Receiver<ServerMessage> {
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
