//! Pairing two one-way connections into one logical connection.
//!
//! A single connection cannot carry both directions on Windows. Duplicating
//! its handle -- which is what `try_clone` does there -- yields two handles
//! onto one *file object*, and a file object opened for synchronous I/O
//! serialises its operations: a thread parked in `ReadFile` blocks a
//! `WriteFile` issued from another thread. Dispatch parks a reader thread and
//! writes from elsewhere on every connection, so that deadlock is the normal
//! case rather than an edge one.
//!
//! So a client opens two connections and labels each with the direction it
//! carries. Both platforms do this. The point is that the state machine which
//! can only be exercised slowly, on Windows CI, is the same one the Unix test
//! suite runs on every commit.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::IpcError;

/// Bytes in a token.
const TOKEN_BYTES: usize = 12;

/// Bytes a client writes before anything else on each connection.
const PREAMBLE_BYTES: usize = 1 + TOKEN_BYTES;

/// The client writes here; the server reads.
const TO_SERVER: u8 = 0x01;

/// The client reads here; the server writes.
const TO_CLIENT: u8 = 0x02;

/// How long a half waits for its partner.
///
/// A client connects its two halves back to back, microseconds apart; ten
/// seconds is a client that died between them, and its half is let go.
pub(super) const HALF_PATIENCE: Duration = Duration::from_secs(10);

/// How many halves may wait at once.
///
/// Each is an open connection. Past this the oldest goes, so a burst of
/// clients that each connect once and die costs a bounded table, not one
/// entry per corpse.
pub(super) const MAX_WAITING: usize = 64;

/// Names the two halves of one connection to each other.
pub(super) type Token = [u8; TOKEN_BYTES];

/// Mints a token that no concurrent connect will repeat.
///
/// A process identifier and a counter, not randomness: the token only has to
/// distinguish connections being paired at the same moment on one machine, and
/// a counter guarantees that where random bytes merely make a collision
/// unlikely. It is not a credential — the transport's own access control keeps
/// other users out.
fn token() -> Token {
    static NEXT: AtomicU64 = AtomicU64::new(1);

    let mut token = [0u8; TOKEN_BYTES];
    token[..4].copy_from_slice(&std::process::id().to_be_bytes());
    token[4..].copy_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    token
}

/// Opens two connections and labels them, returning `(reader, writer)`.
///
/// `open` is the platform's connect, called twice.
pub(super) fn dial<S: Read + Write>(
    mut open: impl FnMut() -> Result<S, IpcError>,
) -> Result<(S, S), IpcError> {
    let token = token();

    let mut reader = open()?;
    announce(&mut reader, TO_CLIENT, &token)?;

    let mut writer = open()?;
    announce(&mut writer, TO_SERVER, &token)?;

    Ok((reader, writer))
}

/// Writes the preamble that tells the server what this connection is for.
fn announce<S: Write>(stream: &mut S, role: u8, token: &Token) -> Result<(), IpcError> {
    let mut preamble = [0u8; PREAMBLE_BYTES];
    preamble[0] = role;
    preamble[1..].copy_from_slice(token);

    stream
        .write_all(&preamble)
        .and_then(|()| stream.flush())
        .map_err(|e| IpcError::io("announcing a connection", e))
}

/// Reads the preamble a client wrote.
pub(super) fn listen_for<S: Read>(stream: &mut S) -> Result<(Token, u8), IpcError> {
    let mut preamble = [0u8; PREAMBLE_BYTES];
    stream
        .read_exact(&mut preamble)
        .map_err(|e| IpcError::io("reading a connection's preamble", e))?;

    let role = preamble[0];
    if role != TO_SERVER && role != TO_CLIENT {
        return Err(IpcError::io(
            format!("a connection claimed the unknown role {role:#04x}"),
            std::io::Error::from(std::io::ErrorKind::InvalidData),
        ));
    }

    let mut token = [0u8; TOKEN_BYTES];
    token.copy_from_slice(&preamble[1..]);

    Ok((token, role))
}

/// Halves waiting for the connection they belong with.
pub(super) struct Halves<S> {
    waiting: HashMap<Token, Waiting<S>>,
}

/// One half, and since when it has waited.
struct Waiting<S> {
    role: u8,
    stream: S,
    since: Instant,
}

impl<S> Halves<S> {
    pub(super) fn new() -> Self {
        Self {
            waiting: HashMap::new(),
        }
    }

    /// Files one half at `now`, returning `(reader, writer)` from the
    /// *server's* side once both halves of a token are in.
    ///
    /// A second half claiming a role its partner already claimed is a broken
    /// client, so both are dropped rather than one of them believed. Halves
    /// older than [`HALF_PATIENCE`] are let go first, and the oldest is let go
    /// to keep the table at [`MAX_WAITING`].
    pub(super) fn offer(
        &mut self,
        token: Token,
        role: u8,
        stream: S,
        now: Instant,
    ) -> Option<(S, S)> {
        self.waiting
            .retain(|_, half| now.saturating_duration_since(half.since) < HALF_PATIENCE);

        if let Some(held) = self.waiting.remove(&token) {
            if held.role == role {
                return None;
            }

            // The server reads what the client writes, and writes what it reads.
            return if role == TO_SERVER {
                Some((stream, held.stream))
            } else {
                Some((held.stream, stream))
            };
        }

        if self.waiting.len() >= MAX_WAITING {
            let oldest = self
                .waiting
                .iter()
                .min_by_key(|(_, half)| half.since)
                .map(|(token, _)| *token);
            if let Some(oldest) = oldest {
                self.waiting.remove(&oldest);
            }
        }

        self.waiting.insert(
            token,
            Waiting {
                role,
                stream,
                since: now,
            },
        );
        None
    }
}

/// Writes one client-to-server half's preamble, for the transport tests that
/// need a connection with no partner coming.
#[cfg(test)]
pub(super) fn half_for_test<S: Write>(stream: &mut S) -> Result<(), IpcError> {
    announce(stream, TO_SERVER, &token())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens both halves against a pair of in-memory buffers.
    fn dialled() -> Vec<Vec<u8>> {
        let mut written: Vec<Vec<u8>> = Vec::new();
        let (first, second) = dial(|| Ok::<_, IpcError>(std::io::Cursor::new(Vec::new())))
            .expect("dialling succeeds");
        written.push(first.into_inner());
        written.push(second.into_inner());
        written
    }

    #[test]
    fn the_two_halves_of_one_connection_share_a_token() {
        // Without this the server cannot tell which two connections belong
        // together when two clients connect at the same instant.
        let written = dialled();

        assert_eq!(written[0][1..], written[1][1..], "the tokens must match");
        assert_ne!(written[0][0], written[1][0], "the roles must differ");
    }

    #[test]
    fn two_connections_are_paired_whichever_arrives_first() {
        // The server has no say in the order two connects land in.
        for reversed in [false, true] {
            let mut halves = Halves::new();
            let token = token();

            let mut order = [(TO_SERVER, "from the client"), (TO_CLIENT, "to the client")];
            if reversed {
                order.reverse();
            }

            assert!(
                halves
                    .offer(token, order[0].0, order[0].1, Instant::now())
                    .is_none(),
                "one half alone is not a pair"
            );

            let (reader, writer) = halves
                .offer(token, order[1].0, order[1].1, Instant::now())
                .expect("the second half completes the pair");

            assert_eq!(reader, "from the client");
            assert_eq!(writer, "to the client");
        }
    }

    #[test]
    fn two_clients_connecting_at_once_are_not_crossed() {
        // The whole reason a token exists.
        let mut halves = Halves::new();
        let (first, second) = (token(), token());

        assert!(
            halves
                .offer(first, TO_SERVER, "first reads", Instant::now())
                .is_none()
        );
        assert!(
            halves
                .offer(second, TO_SERVER, "second reads", Instant::now())
                .is_none()
        );

        let (reader, _) = halves
            .offer(second, TO_CLIENT, "second writes", Instant::now())
            .expect("the second client pairs");
        assert_eq!(reader, "second reads");

        let (reader, _) = halves
            .offer(first, TO_CLIENT, "first writes", Instant::now())
            .expect("the first client pairs");
        assert_eq!(reader, "first reads");
    }

    #[test]
    fn two_halves_claiming_one_direction_are_both_dropped() {
        // A client that labels both its connections the same way cannot be
        // served, and guessing which to believe would wire it backwards.
        let mut halves = Halves::new();
        let token = token();

        assert!(
            halves
                .offer(token, TO_SERVER, "one", Instant::now())
                .is_none()
        );
        assert!(
            halves
                .offer(token, TO_SERVER, "two", Instant::now())
                .is_none(),
            "a duplicate role is not a pair"
        );
        assert!(
            halves
                .offer(token, TO_CLIENT, "three", Instant::now())
                .is_none(),
            "both duplicates must have been dropped, leaving nothing to pair with"
        );
    }

    #[test]
    fn a_preamble_survives_the_round_trip() {
        let written = dialled();

        let (token, role) =
            listen_for(&mut written[0].as_slice()).expect("the preamble reads back");
        assert_eq!(role, TO_CLIENT);
        assert_eq!(token, written[0][1..]);
    }

    #[test]
    fn a_connection_claiming_an_unknown_role_is_refused() {
        // A peer speaking some other protocol at our endpoint must be turned
        // away rather than filed under a role we invented for it.
        let mut preamble = vec![0x7f];
        preamble.extend_from_slice(&token());

        let error = listen_for(&mut preamble.as_slice()).expect_err("0x7f is not a role");
        assert!(
            error.to_string().contains("unknown role"),
            "expected the role to be named, got {error}"
        );
    }

    #[test]
    fn a_client_that_vanished_mid_preamble_is_an_error() {
        // The server must be able to tell this from a complete half.
        let short = [TO_SERVER, 0x00, 0x01];
        listen_for(&mut short.as_slice()).expect_err("a truncated preamble cannot be filed");
    }

    #[test]
    fn a_half_whose_partner_never_comes_is_let_go() {
        // A client that died between its two connects used to leave its
        // first half in the table for the life of the daemon.
        let mut halves = Halves::new();
        let start = Instant::now();
        let token = token();

        assert!(halves.offer(token, TO_SERVER, "early", start).is_none());

        let late = start + HALF_PATIENCE + Duration::from_secs(1);
        assert!(
            halves.offer(token, TO_CLIENT, "late", late).is_none(),
            "a partner arriving after the wait is over finds nothing to pair with"
        );
    }

    #[test]
    fn only_so_many_halves_wait_at_once() {
        let mut halves = Halves::new();
        let now = Instant::now();

        let first = token();
        assert!(halves.offer(first, TO_SERVER, 0, now).is_none());
        let mut last = first;
        for i in 1..=MAX_WAITING {
            last = token();
            let at = now + Duration::from_millis(u64::try_from(i).expect("small"));
            assert!(halves.offer(last, TO_SERVER, i, at).is_none());
        }

        let later = now + Duration::from_secs(1);
        assert!(
            halves.offer(first, TO_CLIENT, 1000, later).is_none(),
            "the oldest was let go to make room"
        );
        assert!(
            halves.offer(last, TO_CLIENT, 1001, later).is_some(),
            "the newest is still waiting"
        );
    }
}
