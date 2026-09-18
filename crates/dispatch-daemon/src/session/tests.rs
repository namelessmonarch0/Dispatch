//! Tests for the daemon loop.
//!
//! Each drives the daemon directly rather than through a socket: the transport
//! is tested in `dispatch-os`, and driving the loop keeps these about what the
//! daemon decides rather than how bytes travel.

use super::*;

use std::time::Instant;

/// A harness registry holding a plain shell, so panes run something real.
fn harnesses(dir: &std::path::Path) -> HarnessRegistry {
    let body = if cfg!(windows) {
        "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"cmd.exe\"\n"
    } else {
        "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"sh\"\n"
    };

    std::fs::create_dir_all(dir).expect("temp dir is writable");
    std::fs::write(dir.join("shell.toml"), body).expect("temp dir is writable");
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
    let root = dir.0.canonicalize().expect("the temp dir resolves");
    let project = daemon.open_project(root);

    (daemon, project, dir)
}

fn hello() -> ClientMessage {
    ClientMessage::Hello {
        version: dispatch_proto::VERSION,
        client: "test".into(),
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
    let root = dir.0.canonicalize().expect("the temp dir resolves");

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

    let seen = drain(&inbox);
    let Some(ServerMessage::ProjectOpened { project }) = seen.first() else {
        panic!("expected a project, got {seen:#?}");
    };
    assert_eq!(project.name, "nested");
    assert_eq!(
        project.root,
        nested.canonicalize().expect("the nested dir resolves"),
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

    daemon.request_for_test(1, ClientMessage::OpenProject { root: file });
    assert!(
        matches!(
            drain(&inbox).first(),
            Some(ServerMessage::Error {
                error: ProtocolError::Other(_)
            })
        ),
        "a file is not a project"
    );

    daemon.request_for_test(
        1,
        ClientMessage::OpenProject {
            root: dir.0.join("missing"),
        },
    );
    assert!(
        matches!(
            drain(&inbox).first(),
            Some(ServerMessage::Error {
                error: ProtocolError::Other(_)
            })
        ),
        "a path that does not exist is not a project"
    );

    assert_eq!(daemon.projects().len(), 1, "neither was registered");
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
