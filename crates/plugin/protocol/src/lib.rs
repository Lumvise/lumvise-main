//! Versioned, framed messages exchanged between the Lumvise host and compiled plugins.
//!
//! This crate owns the process-boundary contract. It deliberately has no dependency on
//! App Core, database types, frontend types, or plugin implementations.
//!
//! # Example
//!
//! ```
//! use lumvise_plugin_protocol::{
//!     FrameCodec, MessageBody, WireMessage, CURRENT_PROTOCOL_VERSION,
//! };
//!
//! let hello = WireMessage {
//!     protocol: CURRENT_PROTOCOL_VERSION,
//!     body: MessageBody::HostHello {
//!         session_id: "session-1".into(),
//!         host_id: "lumvise-desktop".into(),
//!         package_digest: "sha256:abc123".into(),
//!     },
//! };
//! let frame = FrameCodec::default().encode(&hello)?;
//! let decoded = FrameCodec::default().decode(&frame)?;
//! assert_eq!(decoded, hello);
//! # Ok::<(), lumvise_plugin_protocol::ProtocolError>(())
//! ```

#![deny(missing_docs)]

mod codec;
mod error;
mod message;
mod protobuf;

pub use codec::FrameCodec;
pub use error::ProtocolError;
pub use message::{
    CURRENT_PROTOCOL_VERSION, MessageBody, PluginWireError, ProtocolVersion, StorageTriggerChanged,
    StorageTriggerDisposition, StorageTriggerRequest, StorageTriggerResponse, WireMessage,
    WireOutcome,
};
