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
