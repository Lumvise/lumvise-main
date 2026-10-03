//! Authenticated, tenant-isolated central resource server.
//!
//! The public module is deliberately small: construction accepts only internal
//! adapters, while verified TLS/H2, OIDC validation, request sequencing,
//! cancellation, and tenant-local persistence lifetime remain implementation
//! details behind [`ResourceServer`].

mod archive_transfer;
pub mod config;
mod http_extension;
pub use http_extension::{ResourceHttpExtension, ResourceHttpRequest, ResourceHttpResponse};
pub mod dispatch;
pub mod server;
pub mod tenant;

pub use config::{ResourceServerConfig, ServerConfigError, ServerNeuralConfig, ServerOidcConfig};
pub use dispatch::{
    ActiveInvocationKey, CentralizedPersistenceProtocolCodec, DispatchError,
    PersistenceProtocolCodec, ResourceDispatcher,
};
pub use server::{
    AccessTokenValidator, OidcAccessTokenValidator, ResourceServer, ResourceServerError,
    ServerResources,
};
pub use tenant::{
    LocalTenantPersistenceFactory, PrincipalPersistenceFactory, TenantAdapterCache, TenantAdapters,
    TenantKey, TenantOpenError, TenantPersistenceFactory, tenant_database_path,
};
