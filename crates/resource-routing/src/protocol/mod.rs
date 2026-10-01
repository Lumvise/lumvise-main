//! Handwritten, versioned Prost protocol for resource invocations.

mod frame;
mod messages;
mod readiness;
mod sequence;

pub use frame::{FrameError, decode_frame, encode_frame, read_frame, write_frame};
pub use messages::*;
pub use readiness::{ReadinessNegotiationError, negotiate_minor};
pub use sequence::{ProtocolError, SequenceValidator};

pub const PROTOCOL_MAJOR: u32 = 3;
pub const PROTOCOL_MINOR: u32 = 0;
pub const CONTENT_TYPE: &str = "application/vnd.lumvise.resources.v3+protobuf";
