/// Failure to encode, decode, validate, read, or write a protocol frame.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// Frame does not contain a complete four-byte prefix.
    #[error("frame prefix contains `{actual}` bytes; expected exactly `{expected}` bytes")]
    FramePrefixTooShort {
        /// Available prefix bytes.
        actual: usize,
        /// Required prefix bytes.
        expected: usize,
    },
    /// Declared payload size differs from the supplied payload.
    #[error(
        "frame declares `{declared}` payload bytes but contains `{actual}`; expected an exact match"
    )]
    FrameLengthMismatch {
        /// Length declared by the prefix.
        declared: usize,
        /// Actual payload length.
        actual: usize,
    },
    /// Payload length cannot be represented by this transport or platform.
    #[error("frame payload length `{actual}` cannot be represented; expected {expected}")]
    FrameLengthUnrepresentable {
        /// Offending payload length or the platform maximum when the wire value is larger.
        actual: usize,
        /// Required representation.
        expected: &'static str,
    },
    /// Payload is not a valid Protobuf protocol envelope.
    #[error("malformed Protobuf `{actual}`: {reason}; expected a plugin protocol envelope")]
    MalformedProtobuf {
        /// Short hexadecimal preview of the offending bytes.
        actual: String,
        /// Decoder diagnostic.
        reason: String,
    },
    /// Decoded Protobuf is structurally incomplete or contains an invalid exact value.
    #[error("invalid Protobuf message `{actual}`; expected {expected}")]
    InvalidProtobufMessage {
        /// Offending field or value.
        actual: String,
        /// Required shape.
        expected: String,
    },
    /// Message uses an incompatible breaking protocol generation.
    #[error("unsupported protocol major `{actual}`; expected `{expected}`")]
    UnsupportedProtocolMajor {
        /// Offending protocol generation.
        actual: u16,
        /// Supported protocol generation.
        expected: u16,
    },
    /// A mandatory identifier or digest is blank.
    #[error("{message_type}.{field} is `{actual}`; expected {expected}")]
    MissingRequiredField {
        /// Tagged message type containing the field.
        message_type: String,
        /// Field that failed validation.
        field: String,
        /// Offending field value.
        actual: String,
        /// Required field shape.
        expected: String,
    },
    /// Stream I/O failed while transferring one frame.
    #[error("protocol frame I/O failed during `{operation}`: {source}")]
    Io {
        /// Read or write operation that failed.
        operation: &'static str,
        /// Underlying stream error.
        #[source]
        source: std::io::Error,
    },
}
