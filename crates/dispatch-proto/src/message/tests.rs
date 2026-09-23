//! Tests for the message types.
//!
//! The important ones are the compatibility tests: they encode a message the
//! way a *different* Dispatch version would and prove this build still reads
//! it. Federation means exactly that situation, and a protocol that corrupts
//! state across a version gap is worse than one that refuses to connect.

use super::*;

use serde::Serialize;

use crate::Frame;

fn round_trip<T>(message: &T) -> T
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let mut buf = Vec::new();
    Frame::write(&mut buf, message).expect("writing succeeds");
    Frame::read(&mut buf.as_slice()).expect("reading succeeds")
}

#[test]
fn every_client_message_round_trips() {
    let messages = vec![
        ClientMessage::Hello {
            version: crate::VERSION,
            client: "test".into(),
            role: Role::Interface,
        },
        ClientMessage::Subscribe,
        ClientMessage::OpenProject {
            root: PathBuf::from("/home/someone/code/dispatch"),
        },
        ClientMessage::SpawnPane {
            project: ProjectId::new(),
            harness: "claude".into(),
            size: (80, 24),
        },
        ClientMessage::WritePane {
            pane: PaneId::new(),
            bytes: b"hello".to_vec(),
        },
        ClientMessage::ResizePane {
            pane: PaneId::new(),
            size: (100, 30),
        },
        ClientMessage::ClosePane {
            pane: PaneId::new(),
        },
        ClientMessage::Ping { token: 42 },
    ];

    for message in messages {
        assert_eq!(round_trip(&message), message);
    }
}

#[test]
fn every_server_message_round_trips() {
    let messages = vec![
        ServerMessage::Welcome {
            version: crate::VERSION,
            device: "desktop".into(),
        },
        ServerMessage::Error {
            error: ProtocolError::NoSuchPane(PaneId::new()),
        },
        ServerMessage::PaneOutput {
            pane: PaneId::new(),
            bytes: b"\x1b[31mred".to_vec(),
        },
        ServerMessage::PaneChanged {
            pane: PaneId::new(),
            update: PaneUpdate::Status {
                status: PaneStatus::Exited(3),
            },
        },
        ServerMessage::PaneChanged {
            pane: PaneId::new(),
            update: PaneUpdate::Title {
                title: "building".into(),
            },
        },
        ServerMessage::ProjectOpened {
            project: dispatch_core::Project::new(
                "/home/someone/code/dispatch",
                dispatch_core::ProjectSource::GitRepo {
                    remote: Some("git@github.com:someone/dispatch.git".into()),
                },
            ),
        },
        ServerMessage::PaneSpawned {
            pane: PaneId::new(),
            project: ProjectId::new(),
            harness: "codex".into(),
            parent: Some(PaneId::new()),
            durable: true,
        },
        ServerMessage::PaneClosed {
            pane: PaneId::new(),
        },
        ServerMessage::Pong { token: 7 },
    ];

    for message in messages {
        assert_eq!(round_trip(&message), message);
    }
}

#[test]
fn a_refused_root_round_trips_and_an_older_peer_skips_it() {
    // The root travels back exactly as it was sent, so the client can find
    // it in its own kept list without resolving anything itself.
    let refused = ServerMessage::ProjectRefused {
        root: PathBuf::from("~/code/typo"),
        reason: "No such file or directory".into(),
    };
    assert_eq!(round_trip(&refused), refused);

    // An older client has no `project_refused`. It must land in `Unknown`
    // rather than failing the frame: a fleet is exactly where an older
    // client meets a newer daemon.
    #[derive(Serialize)]
    struct FromNewer {
        #[serde(rename = "type")]
        kind: &'static str,
        root: &'static str,
        reason: &'static str,
    }
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &FromNewer {
            kind: "some_message_from_the_future",
            root: "~/x",
            reason: "no",
        },
    )
    .expect("writing succeeds");
    let read: ServerMessage = Frame::read(&mut buf.as_slice()).expect("reading succeeds");
    assert_eq!(read, ServerMessage::Unknown);
}

#[test]
fn arbitrary_bytes_survive_a_round_trip() {
    // Terminal output is not text. Anything that mangles a byte corrupts the
    // screen on the far side.
    let bytes: Vec<u8> = (0..=255u8).collect();
    let message = ServerMessage::PaneOutput {
        pane: PaneId::new(),
        bytes: bytes.clone(),
    };

    match round_trip(&message) {
        ServerMessage::PaneOutput { bytes: got, .. } => assert_eq!(got, bytes),
        other => panic!("expected PaneOutput, got {other:?}"),
    }
}

#[test]
fn output_is_encoded_as_binary_rather_than_a_list_of_numbers() {
    // MessagePack has a binary type, but serde maps Vec<u8> onto a sequence by
    // default, which is several times larger. Output is the bulk of this
    // protocol's traffic.
    let bytes = vec![0u8; 1000];
    let message = ServerMessage::PaneOutput {
        pane: PaneId::new(),
        bytes,
    };

    let mut buf = Vec::new();
    Frame::write(&mut buf, &message).expect("writing succeeds");

    assert!(
        buf.len() < 1200,
        "1000 bytes of output encoded to {} bytes; it is not using the binary type",
        buf.len()
    );
}

// --- Compatibility ---------------------------------------------------------
//
// These stand in for a peer built from different source. Each mirrors a real
// message but differs the way a future or past version would.

/// A newer daemon that added a field to `Welcome`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NewerServerMessage {
    Welcome {
        version: Version,
        device: String,
        /// Added after 1.0.
        capabilities: Vec<String>,
    },
}

/// An older client that never learned about `client` on `Hello`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OlderClientMessage {
    Hello { version: Version },
}

#[test]
fn a_field_added_by_a_newer_peer_is_ignored() {
    // The failure this prevents: a positional format would read
    // `capabilities` as part of the next field and corrupt everything after.
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &NewerServerMessage::Welcome {
            version: crate::VERSION,
            device: "desktop".into(),
            capabilities: vec!["worktrees".into()],
        },
    )
    .expect("writing succeeds");

    let read: ServerMessage = Frame::read(&mut buf.as_slice())
        .expect("a newer peer's extra field must be skipped, not fail the decode");

    assert_eq!(
        read,
        ServerMessage::Welcome {
            version: crate::VERSION,
            device: "desktop".into(),
        }
    );
}

#[test]
fn a_field_missing_from_an_older_peer_falls_back_to_its_default() {
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &OlderClientMessage::Hello {
            version: crate::VERSION,
        },
    )
    .expect("writing succeeds");

    let read: ClientMessage =
        Frame::read(&mut buf.as_slice()).expect("a field an older peer never sent must fall back");

    assert_eq!(
        read,
        ClientMessage::Hello {
            version: crate::VERSION,
            client: String::new(),
            role: Role::Interface,
        }
    );
}

#[test]
fn versions_differing_only_in_minor_are_compatible() {
    let ours = Version { major: 1, minor: 0 };
    let newer = Version { major: 1, minor: 7 };

    assert!(ours.is_compatible_with(newer));
    assert!(newer.is_compatible_with(ours));
}

#[test]
fn a_different_major_version_is_incompatible() {
    // A major bump is reserved for a change an older peer cannot ignore, so
    // refusing is the only safe answer.
    let ours = Version { major: 1, minor: 9 };
    let next = Version { major: 2, minor: 0 };

    assert!(!ours.is_compatible_with(next));
}

#[test]
fn an_incompatible_version_error_names_both_sides() {
    // The message is what a user sees when two machines disagree, so it has
    // to say which one to upgrade.
    let error = ProtocolError::IncompatibleVersion {
        peer: Version { major: 2, minor: 0 },
        ours: Version { major: 1, minor: 0 },
    };

    let text = error.to_string();
    assert!(text.contains("2.0"), "{text}");
    assert!(text.contains("1.0"), "{text}");
}

#[test]
fn an_unknown_failure_can_still_be_explained() {
    // A newer daemon must be able to report something an older client has no
    // variant for, rather than being reduced to "error".
    let error = ProtocolError::Other("worktree is locked".into());
    assert_eq!(round_trip(&error).to_string(), "worktree is locked");
}

#[test]
fn the_delegation_messages_round_trip() {
    let request = RequestId::new();
    let messages = vec![
        ClientMessage::DelegateRequest {
            parent: PaneId::new(),
            harness: "claude".into(),
            task: "write the tests".into(),
            size: (80, 24),
        },
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    ];
    for message in messages {
        assert_eq!(round_trip(&message), message);
    }

    let replies = vec![
        ServerMessage::DelegatePending {
            request,
            parent: PaneId::new(),
            project: ProjectId::new(),
            harness: "claude".into(),
            task: "write the tests".into(),
            depth: 0,
        },
        ServerMessage::DelegateResolved {
            request,
            outcome: DelegateOutcome::Approved {
                pane: PaneId::new(),
            },
        },
        ServerMessage::DelegateResolved {
            request,
            outcome: DelegateOutcome::Denied,
        },
        ServerMessage::DelegateResolved {
            request,
            outcome: DelegateOutcome::Refused {
                reason: "harness \"agy\" has no [task] form".into(),
            },
        },
        ServerMessage::DelegateFinished {
            request,
            exit: 0,
            tail: b"done\r\n".to_vec(),
        },
    ];
    for message in replies {
        assert_eq!(round_trip(&message), message);
    }
}

#[test]
fn an_older_peer_is_an_interface_client() {
    // A client built before delegation existed sends no role, and is exactly
    // what Interface means: it draws panes.
    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OldClientMessage {
        Hello { version: Version, client: String },
    }

    let old = OldClientMessage::Hello {
        version: crate::VERSION,
        client: "old dispatch".into(),
    };

    let mut buf = Vec::new();
    Frame::write(&mut buf, &old).expect("writing succeeds");
    let read: ClientMessage = Frame::read(&mut buf.as_slice()).expect("reading succeeds");

    assert_eq!(
        read,
        ClientMessage::Hello {
            version: crate::VERSION,
            client: "old dispatch".into(),
            role: Role::Interface,
        }
    );
}

#[test]
fn a_delegate_callers_tail_is_binary_not_a_list_of_numbers() {
    // Same reason pane output is: this is the bulk of what the message carries.
    //
    // The bytes are deliberately above 0x7f. A byte below that encodes as a
    // one-byte positive fixint, so a sequence of zeroes costs exactly what
    // binary does and a size assertion over it proves nothing.
    let message = ServerMessage::DelegateFinished {
        request: RequestId::new(),
        exit: 0,
        tail: vec![200u8; 1024],
    };

    let mut buf = Vec::new();
    Frame::write(&mut buf, &message).expect("writing succeeds");
    assert!(
        buf.len() < 1200,
        "1 KiB of output should cost about 1 KiB, not {} bytes — a sequence \
         encoding would need two bytes for every byte above 0x7f",
        buf.len()
    );
}

#[test]
fn a_message_from_a_newer_peer_is_skipped_rather_than_fatal() {
    // A newer daemon may send a message this build has no name for. Failing the
    // frame would take the whole connection down over something ignorable.
    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum FutureServerMessage {
        SomethingNewEntirely { detail: String },
    }

    let future = FutureServerMessage::SomethingNewEntirely {
        detail: "from a later version".into(),
    };

    let mut buf = Vec::new();
    Frame::write(&mut buf, &future).expect("writing succeeds");
    let read: ServerMessage = Frame::read(&mut buf.as_slice()).expect("an unknown message decodes");

    assert_eq!(read, ServerMessage::Unknown);
}

#[test]
fn an_unknown_client_message_is_skipped_rather_than_fatal() {
    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum FutureClientMessage {
        AskSomethingNew { detail: String },
    }

    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &FutureClientMessage::AskSomethingNew {
            detail: "from a later version".into(),
        },
    )
    .expect("writing succeeds");
    let read: ClientMessage = Frame::read(&mut buf.as_slice()).expect("an unknown message decodes");

    assert_eq!(read, ClientMessage::Unknown);
}

#[test]
fn an_unknown_pane_update_is_skipped_rather_than_fatal() {
    // `PaneUpdate` travels inside `PaneChanged`, so an unrecognised variant
    // here fails the enclosing frame — and a client that drops its connection
    // over a frame it cannot read reconnects and fails on the next one. That
    // loop is what every `Unknown` in this module exists to prevent.
    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum FuturePaneUpdate {
        Cwd { path: String },
    }

    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum FutureServerMessage {
        PaneChanged {
            pane: PaneId,
            update: FuturePaneUpdate,
        },
    }

    let pane = PaneId::new();
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &FutureServerMessage::PaneChanged {
            pane,
            update: FuturePaneUpdate::Cwd {
                path: "/somewhere/new".into(),
            },
        },
    )
    .expect("writing succeeds");

    let read: ServerMessage =
        Frame::read(&mut buf.as_slice()).expect("an unknown update must not fail the frame");

    assert_eq!(
        read,
        ServerMessage::PaneChanged {
            pane,
            update: PaneUpdate::Unknown,
        },
        "the frame survives, carrying an update this build can ignore"
    );
}

#[test]
fn a_pane_announced_by_an_older_daemon_is_not_durable() {
    // `durable` decides whether a closed parent keeps its row over this pane,
    // and an older daemon says nothing about it. Not durable is the safe
    // fallback: the pane dies with its caller, which is what every delegated
    // pane did before blanket approval existed.
    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OlderServerMessage {
        PaneSpawned {
            pane: PaneId,
            project: ProjectId,
            harness: String,
        },
    }

    let pane = PaneId::new();
    let project = ProjectId::new();
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &OlderServerMessage::PaneSpawned {
            pane,
            project,
            harness: "claude".into(),
        },
    )
    .expect("writing succeeds");

    let read: ServerMessage = Frame::read(&mut buf.as_slice()).expect("reading succeeds");

    assert_eq!(
        read,
        ServerMessage::PaneSpawned {
            pane,
            project,
            harness: "claude".into(),
            parent: None,
            durable: false,
        }
    );
}

#[test]
fn closing_a_project_round_trips_both_ways() {
    // A client keeps its own list of projects; the daemon has to be told when
    // one leaves it, or the next Subscribe hands it straight back.
    let project = ProjectId::new();

    let asked = ClientMessage::CloseProject { project };
    assert_eq!(round_trip(&asked), asked);

    let answered = ServerMessage::ProjectClosed { project };
    assert_eq!(round_trip(&answered), answered);
}

#[test]
fn a_higher_minor_version_is_still_compatible() {
    // Adding a message is a minor bump: an older peer ignores a variant it has
    // no name for. It must not be read as a reason to refuse the connection.
    let older = Version {
        major: crate::VERSION.major,
        minor: 0,
    };

    assert!(crate::VERSION.is_compatible_with(older));
    assert!(older.is_compatible_with(crate::VERSION));
}
