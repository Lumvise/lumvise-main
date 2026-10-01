use thiserror::Error;

use super::{InvocationEnvelopeV1, PROTOCOL_MAJOR, invocation_envelope_v1::Payload};

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolError {
    #[error("unsupported protocol major {got}; this endpoint supports {supported}")]
    UnsupportedMajor { got: u32, supported: u32 },
    #[error("expected request ID {expected:?}, got {got:?}")]
    WrongRequestId { expected: String, got: String },
    #[error("expected sequence {expected}, got {got}")]
    SequenceGap { expected: u64, got: u64 },
    #[error("a terminal frame has already been received")]
    DuplicateTerminal,
    #[error("invocation envelope has no payload")]
    MissingPayload,
}

/// Validates one direction of an invocation stream. The peer must construct a
/// validator per request and per direction; request IDs and sequence numbers
/// are never shared across invocations.
#[derive(Debug)]
pub struct SequenceValidator {
    request_id: String,
    next_sequence: u64,
    terminal_seen: bool,
}

impl SequenceValidator {
    pub fn new(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            next_sequence: 0,
            terminal_seen: false,
        }
    }

    pub fn validate(&mut self, envelope: &InvocationEnvelopeV1) -> Result<(), ProtocolError> {
        if envelope.protocol_major != PROTOCOL_MAJOR {
            return Err(ProtocolError::UnsupportedMajor {
                got: envelope.protocol_major,
                supported: PROTOCOL_MAJOR,
            });
        }
        if envelope.request_id != self.request_id {
            return Err(ProtocolError::WrongRequestId {
                expected: self.request_id.clone(),
                got: envelope.request_id.clone(),
            });
        }
        if envelope.sequence != self.next_sequence {
            return Err(ProtocolError::SequenceGap {
                expected: self.next_sequence,
                got: envelope.sequence,
            });
        }
        let payload = envelope
            .payload
            .as_ref()
            .ok_or(ProtocolError::MissingPayload)?;
        if self.terminal_seen {
            return Err(ProtocolError::DuplicateTerminal);
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        if matches!(payload, Payload::Terminal(_)) {
            self.terminal_seen = true;
        }
        Ok(())
    }

    pub fn terminal_seen(&self) -> bool {
        self.terminal_seen
    }
}
