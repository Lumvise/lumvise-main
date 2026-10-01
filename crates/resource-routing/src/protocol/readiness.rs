use thiserror::Error;

use super::{PROTOCOL_MAJOR, ReadinessRequestV1};

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ReadinessNegotiationError {
    #[error("client does not support protocol major {server_major}")]
    MajorMismatch { server_major: u32 },
    #[error("client and server have no common protocol minor for major {major}")]
    MinorMismatch { major: u32 },
}

/// Negotiates the highest common V1 minor. A major mismatch is fatal and is
/// never silently downgraded.
pub fn negotiate_minor(
    request: &ReadinessRequestV1,
    server_supported_minors: &[u32],
) -> Result<u32, ReadinessNegotiationError> {
    if !request.supported_majors.contains(&PROTOCOL_MAJOR) {
        return Err(ReadinessNegotiationError::MajorMismatch {
            server_major: PROTOCOL_MAJOR,
        });
    }
    request
        .supported_minors
        .iter()
        .filter(|minor| server_supported_minors.contains(minor))
        .copied()
        .max()
        .ok_or(ReadinessNegotiationError::MinorMismatch {
            major: PROTOCOL_MAJOR,
        })
}
