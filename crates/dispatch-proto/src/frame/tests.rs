//! Tests for framing.

use super::*;

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
