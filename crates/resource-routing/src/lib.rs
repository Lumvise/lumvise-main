//! Shared, versioned routing boundary for centralized Lumvise resources.
//!
//! This crate deliberately owns only startup routing, OIDC, protocol, and
//! verified HTTP/2 transport. Resource implementations remain in their owning
//! neural and persistence crates.

pub mod auth;
pub mod config;
pub mod control;
pub mod protocol;
pub mod transport;

pub use config::{
    CentralServerConfig, OidcClientConfig, ResourcePlacement, ResourceRoutingConfig,
    RoutingConfigError,
};
pub use control::InvocationControl;

pub use transport::{
    AuthenticatedFramedClient, BlockingAuthenticatedFramedClient, Http2CentralTransport,
    ResourceInvocationClient, TransportError,
};
