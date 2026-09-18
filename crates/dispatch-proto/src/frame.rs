//! Length-prefixed framing over a byte stream.

use std::io::{Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Largest frame accepted, in bytes.
///
/// A stream socket carries whatever the peer sends, including a corrupt or
/// hostile length. Without a cap, one bad prefix asks for an allocation of up
/// to four gigabytes.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// Failures reading or writing a frame.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The stream ended between frames, which is an orderly disconnect.
    #[error("the peer disconnected")]
    Disconnected,

    /// The stream ended part-way through a frame.
    #[error("the peer disconnected mid-frame")]
    Truncated,

    /// A length prefix exceeded [`MAX_FRAME_BYTES`].
    #[error("frame of {size} bytes exceeds the {MAX_FRAME_BYTES} byte limit")]
    TooLarge {
        /// The size the peer asked for.
        size: u32,
    },

    /// The underlying stream failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The payload was not valid MessagePack, or not the expected shape.
    #[error("failed to decode a message: {0}")]
    Decode(String),

    /// The message could not be encoded.
    #[error("failed to encode a message: {0}")]
    Encode(String),
}

/// Reads and writes length-prefixed messages.
pub struct Frame;

impl Frame {
    /// Encodes `message` into a frame and writes it.
    ///
    /// Structs are encoded as maps keyed by field name, which is what lets a
    /// peer built against a different version skip fields it does not know.
    /// `rmp_serde` defaults to positional arrays, so this is chosen
    /// explicitly, and changing it would break compatibility silently.
    pub fn write<W: Write, T: Serialize>(writer: &mut W, message: &T) -> Result<(), FrameError> {
        let mut payload = Vec::new();
        let mut serializer = rmp_serde::Serializer::new(&mut payload)
            .with_struct_map()
            .with_human_readable();

        message
            .serialize(&mut serializer)
            .map_err(|e| FrameError::Encode(e.to_string()))?;

        let size =
            u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge { size: u32::MAX })?;
        if size > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge { size });
        }

        writer.write_all(&size.to_be_bytes())?;
        writer.write_all(&payload)?;
        writer.flush()?;
        Ok(())
    }

    /// Reads one frame and decodes it.
    pub fn read<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, FrameError> {
        let mut length = [0u8; 4];

        match reader.read_exact(&mut length) {
            Ok(()) => {}
            // Nothing at all means the peer closed between frames, which is
            // an orderly end rather than an error.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(FrameError::Disconnected);
            }
            Err(e) => return Err(e.into()),
        }

        let size = u32::from_be_bytes(length);
        if size > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge { size });
        }

        let mut payload = vec![0u8; size as usize];
        match reader.read_exact(&mut payload) {
            Ok(()) => {}
            // Stopping part-way through is a broken connection, not a clean
            // close, and the caller should treat the two differently.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(FrameError::Truncated);
            }
            Err(e) => return Err(e.into()),
        }

        rmp_serde::from_slice(&payload).map_err(|e| FrameError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests;
