//! Connector setup and native permission review shapes. Credentials are never tool arguments.
//! The host authenticates each invocation; a displayed connector ID is not proof of possession.
use crate::KnowledgeProjectIdentity;
use serde::{Deserialize, Serialize};

/// Opaque installation credential. Example: deserialize a private connector configuration file.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ConnectorCredential(pub String);

impl ConnectorCredential {
    /// Validates a proof without including it in diagnostics. Example: validate a private file before registration.
    pub fn validate(&self) -> Result<(), String> {
        if self.0.len() == 64 && self.0.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        Err("connector credential [redacted]; expected 256-bit hexadecimal proof".into())
    }
}

impl std::fmt::Debug for ConnectorCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ConnectorCredential([redacted])")
    }
}

/// Authenticated installation displayed during native approval, e.g. a Codex integration.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConnectorIdentity {
    pub connector_id: String,
    pub display_name: String,
}

/// Knowledge mutations covered by this approval; deletion and administration are excluded.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKnowledgeOperation {
    Create,
    Update,
}

/// Review state retained across connections and app restarts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorGrantState {
    Pending,
    Granted,
    Revoked,
}

/// One project-lineage permission. Example: Settings approves the observed pending revision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConnectorKnowledgeGrant {
    pub connector: ConnectorIdentity,
    pub project: KnowledgeProjectIdentity,
    pub revision: u64,
    pub state: ConnectorGrantState,
    pub operations: Vec<ConnectorKnowledgeOperation>,
}

/// Native account adapter proves current ownership separately from content-write membership.
/// Example: Settings supplies an adapter that refreshes the website's owner role.
pub trait ConnectorGrantMaintainer {
    fn require_maintainer(&self, project: &KnowledgeProjectIdentity) -> Result<(), String>;
}
