//! The wire protocol between a Dispatch client and `dispatchd`.
//!
//! # Why this encoding
//!
//! Federation means a MacBook and a desktop running *different* Dispatch
//! versions talk to each other. A compact positional format such as bincode or
//! postcard has no schema evolution: add a field and an older peer misreads
//! every byte after it, silently. So messages are encoded as MessagePack
//! **maps**, keyed by field name, and unknown fields are ignored on the way
//! in. An older peer skips what it does not understand instead of
//! misinterpreting it.
//!
//! That costs bytes per message. It is the right trade: this carries terminal
//! output, which is already dominated by the payload, and a protocol that
//! corrupts state across a version mismatch is not worth the saving.
//!
//! Every frame is length-prefixed, because a stream socket has no message
//! boundaries of its own.

pub mod frame;
pub mod message;

pub use frame::{Frame, FrameError, MAX_FRAME_BYTES};
pub use message::{
    ClientMessage, DelegateOutcome, PaneUpdate, ProtocolError, Role, ServerMessage, Version,
};

/// The protocol version this build speaks.
///
/// Bumped only for a change an older peer cannot safely ignore. Adding a
/// message variant or an optional field is not such a change.
pub const VERSION: Version = Version { major: 1, minor: 1 };
