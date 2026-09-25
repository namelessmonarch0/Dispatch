//! What a client and `dispatchd` say to each other.
//!
//! Every message is a struct or an enum with named fields, and every field
//! added after version 1.0 carries `#[serde(default)]`. Together those are
//! what make a version mismatch survivable: a newer peer's extra fields are
//! skipped, and a missing one falls back rather than failing the decode.

use std::path::PathBuf;

use dispatch_core::{PaneId, PaneStatus, Project, ProjectId, RequestId};
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
///
/// Externally tagged — the variants carry data of their own and no `tag`
/// attribute is set — so `#[serde(other)]` does not apply here: serde allows it
/// only on an internally or adjacently tagged enum. [`ProtocolError::Other`] is
/// this type's hatch instead: a variant added here would fail an older peer's
/// whole frame, so a new reason travels as prose in `Other` rather than as a
/// variant that peer has no name for.
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

/// What a connection is for.
///
/// The two audiences want different traffic. An interface draws panes and wants
/// every byte they produce; a delegate caller wants the fate of its own request
/// and nothing else, so sending it pane output would be a firehose it never
/// reads — and would slow the call down on a busy fleet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A client that draws the fleet. The default, because a peer built before
    /// roles existed is one of these.
    #[default]
    Interface,
    /// A `dispatch delegate` call waiting on one request.
    Delegate,
}

/// How a delegation request ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DelegateOutcome {
    /// The user approved it, and this pane is running the task.
    Approved {
        /// The subagent's pane.
        pane: PaneId,
    },
    /// The user denied it.
    Denied,
    /// The daemon refused it without asking: a cap or a missing task form.
    ///
    /// Distinct from [`Expired`] because the two call for opposite responses: a
    /// refusal will not change if the caller asks again, while a request that
    /// timed out may well be answered next time.
    Refused {
        /// Why, in words, because an agent reads this and should be able to act
        /// on it.
        reason: String,
    },
    /// Nobody answered before the deadline.
    ///
    /// Distinct from [`Refused`] because the two call for opposite responses: a
    /// refusal will not change if the caller asks again, while a request that
    /// timed out may well be answered next time.
    Expired {
        /// How long it waited, so the caller can say so.
        after_secs: u64,
    },
    /// A message this build does not know.
    ///
    /// The protocol's promise is that an older peer skips what it does not
    /// understand rather than misreading it, and that promise needs somewhere
    /// for the unknown to land: without this, one unrecognised variant fails
    /// the whole frame and takes the connection with it.
    #[serde(other)]
    Unknown,
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
        /// What the connection is for.
        #[serde(default)]
        role: Role,
    },

    /// Asks for the current state of everything.
    Subscribe,

    /// Registers a directory the daemon may spawn panes in.
    ///
    /// The daemon resolves and inspects the path, because it is the machine the
    /// directory is on: a client attached over the network cannot stat it, and
    /// even a local one should not be trusted to have got it right.
    OpenProject {
        /// Where the project lives, as the client knows it.
        root: PathBuf,
    },

    /// Forgets a project the client no longer keeps.
    ///
    /// Refused while the project still has panes: the daemon owns them, and a
    /// project it had forgotten would leave them running with nothing to
    /// spawn into and no row to reach them by.
    CloseProject {
        /// Which project to forget.
        project: ProjectId,
    },

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

    /// Asks for a subagent to be started on a task.
    ///
    /// Sent by `dispatch delegate` from inside a pane. The daemon decides
    /// whether it is allowed, and the user whether it happens.
    DelegateRequest {
        /// The pane asking, from `DISPATCH_PANE` in its environment.
        parent: PaneId,
        /// Which harness should run the task.
        harness: String,
        /// What to do, verbatim.
        task: String,
        /// Initial size in cells.
        size: (u16, u16),
    },

    /// Answers a [`ServerMessage::DelegatePending`].
    DelegateDecision {
        /// Which request.
        request: RequestId,
        /// Whether it may run.
        approve: bool,
        /// Whether every later request from the same pane is approved too, for
        /// as long as this daemon runs.
        #[serde(default)]
        blanket: bool,
    },

    /// A message this build does not know.
    ///
    /// The protocol's promise is that an older peer skips what it does not
    /// understand rather than misreading it, and that promise needs somewhere
    /// for the unknown to land: without this, one unrecognised `type` tag fails
    /// the whole frame and takes the connection with it.
    #[serde(other)]
    Unknown,
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

    /// A project is available to spawn panes in.
    ///
    /// Sent for each project on [`ClientMessage::Subscribe`], and again
    /// whenever one is opened, because a pane can only be started against an
    /// id the client has been told.
    ProjectOpened {
        /// The project, as the daemon resolved it.
        project: Project,
    },

    /// A project was forgotten, and every client should drop its row.
    ProjectClosed {
        /// Which project is gone.
        project: ProjectId,
    },

    /// Something about a project changed after it was opened.
    ProjectChanged {
        /// Which project.
        project: ProjectId,
        /// What changed.
        update: ProjectUpdate,
    },

    /// A root asked for in [`ClientMessage::OpenProject`] could not be opened.
    ///
    /// Sent only to the client that asked. Its own message rather than an
    /// [`ProtocolError`]: that enum has no `Unknown` to land in, so a variant
    /// added there would fail an older client's whole frame — and a fleet is
    /// where an older client meets a newer daemon. An older client skips this
    /// and loses only the status line.
    ProjectRefused {
        /// The root exactly as the client sent it, so the client can find it
        /// in what it keeps without resolving anything itself.
        root: PathBuf,
        /// Why: not a directory, no such file, permission denied.
        reason: String,
    },

    /// A root asked for in [`ClientMessage::OpenProject`] was opened, and this
    /// is the project root it became.
    ///
    /// Sent only to the client that asked, before the [`ProjectOpened`]
    /// broadcast. The client keeps the root as it was typed — `~/code/app` —
    /// but the row it gets back carries the root as the daemon resolved it,
    /// so without this the client could never match the two: dropping the row
    /// would forget nothing, and the root would come back on the next start.
    /// Sent even when the two are equal, which keeps the daemon simple; the
    /// client ignores a rewrite to the same path. An older client skips it.
    ///
    /// [`ProjectOpened`]: ServerMessage::ProjectOpened
    ProjectResolved {
        /// The root exactly as the client sent it.
        root: PathBuf,
        /// The project's root, as every client will see it.
        resolved: PathBuf,
    },

    /// A pane was started.
    PaneSpawned {
        /// The new pane.
        pane: PaneId,
        /// Its project.
        project: ProjectId,
        /// Which harness is running.
        harness: String,
        /// The pane that delegated this one's work, when it was delegated.
        #[serde(default)]
        parent: Option<PaneId>,
        /// Whether this pane outlives the caller that asked for it, because the
        /// user approved its parent with `A` rather than approving once.
        ///
        /// On the wire rather than derived by whoever pressed the key: every
        /// client has to draw the same tree, and only the deciding client could
        /// know otherwise. It is what a closed parent's survivor rule turns on.
        /// A top-level pane has no caller to outlive, so the field says nothing
        /// useful about one.
        #[serde(default)]
        durable: bool,
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

    /// A pane is asking to delegate, and a user has to decide.
    ///
    /// Sent to subscribed interface clients only.
    DelegatePending {
        /// Which request.
        request: RequestId,
        /// The pane asking.
        parent: PaneId,
        /// Its project.
        project: ProjectId,
        /// Which harness would run.
        harness: String,
        /// What it would be asked to do, in full: approving something you
        /// cannot read is not approval.
        task: String,
        /// How deep the parent already is, for display.
        #[serde(default)]
        depth: u8,
    },

    /// A request will not be asked about again.
    DelegateResolved {
        /// Which request.
        request: RequestId,
        /// What happened.
        outcome: DelegateOutcome,
    },

    /// A subagent has exited, and its caller can stop waiting.
    DelegateFinished {
        /// Which request.
        request: RequestId,
        /// The subagent's exit code.
        exit: i32,
        /// The tail of what it printed, for the caller to hand to its agent.
        #[serde(with = "serde_bytes_compat")]
        tail: Vec<u8>,
    },

    /// A message this build does not know.
    ///
    /// The protocol's promise is that an older peer skips what it does not
    /// understand rather than misreading it, and that promise needs somewhere
    /// for the unknown to land: without this, one unrecognised `type` tag fails
    /// the whole frame and takes the connection with it.
    #[serde(other)]
    Unknown,
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
    /// The branch the pane is working on changed.
    ///
    /// `None` once the pane's program is outside any repository.
    Branch {
        /// The branch, or `@` and a short commit when the head is detached.
        branch: Option<String>,
    },
    /// A change this build does not know.
    ///
    /// This enum travels inside [`ServerMessage::PaneChanged`], so without
    /// somewhere for the unknown to land a newer daemon's extra variant fails
    /// the whole frame rather than one update — and a client that drops its
    /// connection over a frame it cannot read reconnects and fails on the next
    /// one. That loop is what every `Unknown` in this module exists to prevent.
    #[serde(other)]
    Unknown,
}

/// A change to a project after it was opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProjectUpdate {
    /// The project root's checked-out branch changed.
    Branch {
        /// The branch, or `@` and a short commit when the head is detached.
        branch: Option<String>,
    },
    /// A change this build does not know.
    ///
    /// Travels inside [`ServerMessage::ProjectChanged`], so it needs the same
    /// landing place [`PaneUpdate::Unknown`] gives a pane's.
    #[serde(other)]
    Unknown,
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
