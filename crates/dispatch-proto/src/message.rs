//! What a client and `dispatchd` say to each other.
//!
//! Every message is a struct or an enum with named fields, and every field
//! added after version 1.0 carries `#[serde(default)]`. Together those are
//! what make a version mismatch survivable: a newer peer's extra fields are
//! skipped, and a missing one falls back rather than failing the decode.

use dispatch_core::{PaneId, PaneStatus, ProjectId};
use serde::{Deserialize, Serialize};

/// A protocol version.
///
/// Major differences are incompatible. Minor differences are not: a peer
/// speaking a higher minor version has only added things an older one can
/// ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    /// Incompatible revision.
    pub major: u16,
    /// Backwards-compatible revision.
    pub minor: u16,
}

impl Version {
    /// Whether a peer speaking `self` can talk to one speaking `other`.
    #[must_use]
    pub fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Why a connection was refused or ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ProtocolError {
    /// The peer speaks a major version this build cannot understand.
    #[error("incompatible protocol version: peer speaks {peer}, this build speaks {ours}")]
    IncompatibleVersion {
        /// What the peer offered.
        peer: Version,
        /// What this build speaks.
        ours: Version,
    },

    /// The named pane does not exist, or no longer does.
    #[error("no pane with id {0}")]
    NoSuchPane(PaneId),

    /// The named project does not exist.
    #[error("no project with id {0}")]
    NoSuchProject(ProjectId),

    /// The request failed for a reason with no dedicated variant.
    ///
    /// Carries text rather than a code so a newer daemon can explain a
    /// failure an older client has no name for.
    #[error("{0}")]
    Other(String),
}

/// Something a client asks of the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Opens the connection. Must be the first message.
    Hello {
        /// The version the client speaks.
        version: Version,
        /// Human-readable client description, for the daemon's log.
        #[serde(default)]
        client: String,
    },

    /// Asks for the current state of everything.
    Subscribe,

    /// Starts a pane.
    SpawnPane {
        /// Which project to start it in.
        project: ProjectId,
        /// Which harness to run.
        harness: String,
        /// Initial size in cells.
        size: (u16, u16),
    },

    /// Sends already-encoded bytes to a pane.
    ///
    /// Encoding happens on the client, because it depends on the pane's
    /// current modes and the client already tracks those to draw it.
    WritePane {
        /// Which pane.
        pane: PaneId,
        /// Bytes to write.
        #[serde(with = "serde_bytes_compat")]
        bytes: Vec<u8>,
    },

    /// Resizes a pane.
    ResizePane {
        /// Which pane.
        pane: PaneId,
        /// New size in cells.
        size: (u16, u16),
    },

    /// Closes a pane and terminates its process tree.
    ClosePane {
        /// Which pane.
        pane: PaneId,
    },

    /// Keeps the connection alive and measures round-trip time.
    Ping {
        /// Echoed back in the reply.
        #[serde(default)]
        token: u64,
    },
}

/// Something the daemon tells a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Accepts the connection.
    Welcome {
        /// The version the daemon speaks.
        version: Version,
        /// Identifies this daemon, so a client can tell two apart.
        #[serde(default)]
        device: String,
    },

    /// Refuses or ends the connection.
    Error {
        /// Why.
        error: ProtocolError,
    },

    /// A pane produced output.
    PaneOutput {
        /// Which pane.
        pane: PaneId,
        /// Raw bytes from the child, for the client to feed to its emulator.
        ///
        /// Bytes rather than a rendered screen: the client has the emulator
        /// and re-rendering remotely would cost a full screen per frame.
        #[serde(with = "serde_bytes_compat")]
        bytes: Vec<u8>,
    },

    /// A pane changed in some way other than producing output.
    PaneChanged {
        /// Which pane.
        pane: PaneId,
        /// What changed.
        update: PaneUpdate,
    },

    /// A pane was started.
    PaneSpawned {
        /// The new pane.
        pane: PaneId,
        /// Its project.
        project: ProjectId,
        /// Which harness is running.
        harness: String,
    },

    /// A pane is gone.
    PaneClosed {
        /// Which pane.
        pane: PaneId,
    },

    /// Answers a [`ClientMessage::Ping`].
    Pong {
        /// The token from the ping.
        #[serde(default)]
        token: u64,
    },
}

/// A change to a pane other than output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneUpdate {
    /// The process changed state.
    Status {
        /// The new status.
        status: PaneStatus,
    },
    /// The child set a terminal title.
    Title {
        /// The new title.
        title: String,
    },
}

/// Encodes a byte vector compactly.
///
/// MessagePack has a binary type, but serde maps `Vec<u8>` onto a sequence of
/// integers by default, which is several times larger. Terminal output is the
/// bulk of this protocol's traffic, so it is worth the module.
mod serde_bytes_compat {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        // Accepts either encoding, so a peer that sends a plain sequence is
        // still understood.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Bytes {
            Binary(serde_bytes::ByteBuf),
            Sequence(Vec<u8>),
        }

        Ok(match Bytes::deserialize(deserializer)? {
            Bytes::Binary(buf) => buf.into_vec(),
            Bytes::Sequence(v) => v,
        })
    }
}

#[cfg(test)]
mod tests;
