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
    let project = ProjectId::new();
    daemon.add_project(project, dir.0.clone());

    (daemon, project, dir)
}

fn hello() -> ClientMessage {
    ClientMessage::Hello {
        version: dispatch_proto::VERSION,
        client: "test".into(),
    }
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
