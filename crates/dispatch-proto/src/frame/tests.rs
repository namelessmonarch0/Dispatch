//! Tests for framing.

use super::*;

use crate::ClientMessage;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Sample {
    name: String,
    count: u32,
}

fn sample() -> Sample {
    Sample {
        name: "pane".into(),
        count: 7,
    }
}

#[test]
fn a_message_round_trips() {
    let mut buf = Vec::new();
    Frame::write(&mut buf, &sample()).expect("writing succeeds");

    let read: Sample = Frame::read(&mut buf.as_slice()).expect("reading succeeds");
    assert_eq!(read, sample());
}

#[test]
fn several_messages_round_trip_in_order() {
    // A stream socket has no message boundaries, so the length prefix is the
    // only thing keeping these apart.
    let mut buf = Vec::new();
    for count in 0..5u32 {
        Frame::write(
            &mut buf,
            &Sample {
                name: format!("pane{count}"),
                count,
            },
        )
        .expect("writing succeeds");
    }

    let mut cursor = buf.as_slice();
    for count in 0..5u32 {
        let read: Sample = Frame::read(&mut cursor).expect("reading succeeds");
        assert_eq!(read.count, count);
    }
}

#[test]
fn an_empty_stream_reports_a_clean_disconnect() {
    // Distinct from a truncated frame: one is the peer closing, the other is
    // the connection breaking, and a caller should log them differently.
    let mut empty: &[u8] = &[];
    let result: Result<Sample, _> = Frame::read(&mut empty);

    assert!(matches!(result, Err(FrameError::Disconnected)));
}

#[test]
fn a_frame_cut_short_is_reported_as_truncated() {
    let mut buf = Vec::new();
    Frame::write(&mut buf, &sample()).expect("writing succeeds");
    buf.truncate(buf.len() - 2);

    let result: Result<Sample, _> = Frame::read(&mut buf.as_slice());
    assert!(matches!(result, Err(FrameError::Truncated)));
}

#[test]
fn an_oversized_length_prefix_is_refused_without_allocating() {
    // A corrupt or hostile prefix would otherwise ask for a four gigabyte
    // allocation.
    let mut buf = u32::MAX.to_be_bytes().to_vec();
    buf.extend_from_slice(b"nowhere near that long");

    let result: Result<Sample, _> = Frame::read(&mut buf.as_slice());
    assert!(matches!(result, Err(FrameError::TooLarge { .. })));
}

#[test]
fn a_malformed_payload_is_reported_as_a_decode_failure() {
    let mut buf = 4u32.to_be_bytes().to_vec();
    buf.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);

    let result: Result<Sample, _> = Frame::read(&mut buf.as_slice());
    assert!(matches!(result, Err(FrameError::Decode(_))));
}

#[test]
fn an_empty_payload_is_not_mistaken_for_a_disconnect() {
    let mut buf = 0u32.to_be_bytes().to_vec();
    buf.extend_from_slice(&[]);

    let result: Result<Sample, _> = Frame::read(&mut buf.as_slice());
    assert!(
        matches!(result, Err(FrameError::Decode(_))),
        "a zero-length frame is malformed, not a disconnect"
    );
}

/// A stream that records the largest read it was asked for.
struct Measured<'a> {
    bytes: &'a [u8],
    largest: usize,
}

impl std::io::Read for Measured<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.largest = self.largest.max(buf.len());
        let n = buf.len().min(self.bytes.len());
        buf[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        Ok(n)
    }
}

#[test]
fn a_frame_announcing_more_than_arrives_is_not_reserved_up_front() {
    // Just under the cap, then ten bytes, then the peer goes away. Reading
    // into a buffer sized from the prefix asked for all 64 MiB at once.
    let mut bytes = (MAX_FRAME_BYTES - 1).to_be_bytes().to_vec();
    bytes.extend_from_slice(&[0u8; 10]);
    let mut stream = Measured {
        bytes: &bytes,
        largest: 0,
    };

    let result = Frame::read::<_, ClientMessage>(&mut stream);

    assert!(matches!(result, Err(FrameError::Truncated)), "{result:?}");
    assert!(
        stream.largest <= 64 * 1024,
        "a single read asked for {} bytes of a payload that never came",
        stream.largest
    );
}

#[test]
fn the_watch_hook_runs_once_a_frame_has_begun() {
    let mut bytes = Vec::new();
    Frame::write(&mut bytes, &ClientMessage::Ping { token: 3 }).expect("writing succeeds");

    let mut began = 0;
    let message: ClientMessage =
        Frame::read_watched(&mut bytes.as_slice(), || began += 1).expect("reading succeeds");

    assert_eq!(message, ClientMessage::Ping { token: 3 });
    assert_eq!(began, 1);
}

#[test]
fn the_watch_hook_does_not_run_for_a_stream_that_ended_between_frames() {
    let mut began = 0;
    let result = Frame::read_watched::<_, ClientMessage>(&mut [].as_slice(), || began += 1);

    assert!(matches!(result, Err(FrameError::Disconnected)));
    assert_eq!(began, 0, "nothing began");
}
