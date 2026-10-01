use std::{
    io::{Read, Write},
    marker::PhantomData,
};

use prost::Message;

use crate::protobuf::Envelope;
use crate::{ProtocolError, WireMessage};

const FRAME_PREFIX_BYTES: usize = 8;

/// Encodes and decodes exact eight-byte-big-endian-length-prefixed Protobuf frames.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrameCodec {
    marker: PhantomData<()>,
}

impl FrameCodec {
    /// Encodes one validated message as a complete dynamic-length Protobuf frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ProtocolError`] when message validation or Protobuf encoding fails.
    pub fn encode(&self, message: &WireMessage) -> Result<Vec<u8>, ProtocolError> {
        message.validate()?;
        let payload = Envelope::from(message).encode_to_vec();
        let declared = u64::try_from(payload.len()).map_err(|_| {
            ProtocolError::FrameLengthUnrepresentable {
                actual: payload.len(),
                expected: "an unsigned 64-bit byte length",
            }
        })?;
        let mut frame = Vec::with_capacity(FRAME_PREFIX_BYTES + payload.len());
        frame.extend_from_slice(&declared.to_be_bytes());
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    /// Decodes one exact complete frame and validates the resulting message.
    ///
    /// # Errors
    ///
    /// Returns a [`ProtocolError`] for incomplete, malformed, or invalid messages.
    pub fn decode(&self, frame: &[u8]) -> Result<WireMessage, ProtocolError> {
        let declared = declared_payload_length(frame)?;
        let payload = &frame[FRAME_PREFIX_BYTES..];
        if payload.len() != declared {
            return Err(ProtocolError::FrameLengthMismatch {
                declared,
                actual: payload.len(),
            });
        }
        decode_payload(payload)
    }

    /// Writes one complete encoded frame to a blocking byte stream.
    ///
    /// # Errors
    ///
    /// Returns encoding errors or [`ProtocolError::Io`] when the stream write fails.
    pub fn write_to<W: Write>(
        &self,
        writer: &mut W,
        message: &WireMessage,
    ) -> Result<(), ProtocolError> {
        let frame = self.encode(message)?;
        writer
            .write_all(&frame)
            .map_err(|source| ProtocolError::Io {
                operation: "write frame",
                source,
            })
    }

    /// Reads and decodes one dynamic-length frame from a blocking byte stream.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Io`] for truncated streams, plus normal decode and validation
    /// errors.
    pub fn read_from<R: Read>(&self, reader: &mut R) -> Result<WireMessage, ProtocolError> {
        let mut prefix = [0_u8; FRAME_PREFIX_BYTES];
        reader
            .read_exact(&mut prefix)
            .map_err(|source| ProtocolError::Io {
                operation: "read frame prefix",
                source,
            })?;
        let declared = usize::try_from(u64::from_be_bytes(prefix)).map_err(|_| {
            ProtocolError::FrameLengthUnrepresentable {
                actual: usize::MAX,
                expected: "a payload length representable on this platform",
            }
        })?;
        let mut payload = vec![0_u8; declared];
        reader
            .read_exact(&mut payload)
            .map_err(|source| ProtocolError::Io {
                operation: "read frame payload",
                source,
            })?;
        decode_payload(&payload)
    }
}

fn declared_payload_length(frame: &[u8]) -> Result<usize, ProtocolError> {
    let prefix = frame
        .get(..FRAME_PREFIX_BYTES)
        .ok_or(ProtocolError::FramePrefixTooShort {
            actual: frame.len(),
            expected: FRAME_PREFIX_BYTES,
        })?;
    let declared =
        u64::from_be_bytes(
            prefix
                .try_into()
                .map_err(|_| ProtocolError::FramePrefixTooShort {
                    actual: prefix.len(),
                    expected: FRAME_PREFIX_BYTES,
                })?,
        );
    usize::try_from(declared).map_err(|_| ProtocolError::FrameLengthUnrepresentable {
        actual: usize::MAX,
        expected: "a payload length representable on this platform",
    })
}

fn decode_payload(payload: &[u8]) -> Result<WireMessage, ProtocolError> {
    let envelope = Envelope::decode(payload).map_err(|error| ProtocolError::MalformedProtobuf {
        actual: hex_preview(payload),
        reason: error.to_string(),
    })?;
    let message: WireMessage = envelope.try_into()?;
    message.validate()?;
    Ok(message)
}

fn hex_preview(value: &[u8]) -> String {
    value
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
