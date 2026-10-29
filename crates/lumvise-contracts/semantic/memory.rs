//! Passive memory-space contracts. Knowledge owns documents and access decisions;
//! the host alone binds authenticated authority to an invocation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Reserved document-root schemes; these roots never join ordinary project discovery.
pub const KNOWLEDGE_SPACE_SCHEMES: &[&str] =
    &["assistant-space://", "agent-space://", "mcp-space://"];

/// Adapter-issued route context; App Core resolves its registered project and generation.
pub const KNOWLEDGE_MCP_SCOPE: &str = "mcp_project";

/// Identifies reserved document spaces for source-only discovery and lifecycle routes.
/// Example: `assert!(is_knowledge_space_root("assistant-space://profile/default"));`
pub fn is_knowledge_space_root(root: &str) -> bool {
    KNOWLEDGE_SPACE_SCHEMES
        .iter()
        .any(|scheme| root.starts_with(scheme))
}

/// The authenticated origin of a memory operation, independent of its executor.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgePrincipalKind {
    Human,
    Assistant,
    Agent,
    Mcp,
}

/// A project within its authoritative store. A copied lineage is not a grant.
#[derive(
    Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeProjectIdentity {
    pub project_id: String,
    pub project_root: String,
    pub storage_authority: String,
}

impl KnowledgeProjectIdentity {
    /// Compare durable project ownership after each route has been authorized independently.
    /// Example: a moved checkout keeps its lineage and authority despite a different root.
    pub fn same_project(&self, other: &Self) -> bool {
        self.project_id == other.project_id && self.storage_authority == other.storage_authority
    }
}

/// A host-resolved grant; write includes read and authorized citation retention.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeProjectGrant {
    pub project: KnowledgeProjectIdentity,
    pub writable: bool,
}

/// Current authority returned only by the permission-gated host capability.
/// It is never a model-editable document request or reusable credential.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeAccessContext {
    pub principal_kind: KnowledgePrincipalKind,
    pub principal_id: String,
    pub security_domain: String,
    pub storage_authority: String,
    pub project_grants: Vec<KnowledgeProjectGrant>,
    /// Host-resolved owner binding; Knowledge conditionally persists the descriptor.
    pub space: Option<KnowledgeSpaceDescriptor>,
    pub active_project: Option<KnowledgeProjectIdentity>,
    /// Conservative provenance supplied during this generation, including reads.
    pub source_projects: Vec<KnowledgeProjectIdentity>,
}

/// Lifetime and sharing of a Knowledge-owned document space.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSpaceKind {
    Assistant,
    Agent,
    McpProject,
}

/// An immutable owner descriptor persisted at the Knowledge document root.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSpaceDescriptor {
    pub space_id: String,
    pub space_kind: KnowledgeSpaceKind,
    pub project_root: String,
    pub principal_id: String,
    pub security_domain: String,
    pub storage_authority: String,
    pub shared_project: Option<KnowledgeProjectIdentity>,
    pub revision: u64,
}

/// Owner-controlled restrictions survive summarization and citation removal.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeDocumentAccess {
    pub space_id: String,
    pub required_projects: Vec<KnowledgeProjectIdentity>,
}

/// Resolve the caller's own space, without accepting an arbitrary owner ID.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSpacePurpose {
    #[serde(rename = "self")]
    Own,
    CurrentProject,
}

/// A native human's space selection, never a credential or model-owned authority.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "space_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KnowledgeSpaceInspection {
    Assistant,
    Agent { agent_id: String },
    McpProject { project_root: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reserved_spaces_are_distinct_from_source_projects() {
        for scheme in KNOWLEDGE_SPACE_SCHEMES {
            assert!(is_knowledge_space_root(&format!("{scheme}owner")));
        }
        for root in ["/repo", "lumvise://projects/id", "https://example.test", ""] {
            assert!(!is_knowledge_space_root(root));
        }
    }

    #[test]
    fn space_descriptor_round_trips_with_its_authority_and_sharing() {
        let value = json!({"space_id":"space-a", "space_kind":"assistant",
            "project_root":"assistant-space://space-a", "principal_id":"default",
            "security_domain":"user-a", "storage_authority":"store-a",
            "shared_project":null, "revision":1});
        let descriptor: KnowledgeSpaceDescriptor = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(descriptor).unwrap(), value);
    }

    #[test]
    fn authority_is_explicit_and_rejects_undeclared_fields() {
        let mut value = json!({"principal_kind":"assistant", "principal_id":"default",
            "security_domain":"user-a", "storage_authority":"store-a",
            "project_grants":[], "space":null, "active_project":null, "source_projects":[]});
        assert!(serde_json::from_value::<KnowledgeAccessContext>(value.clone()).is_ok());
        value["administrator"] = json!(true);
        assert!(serde_json::from_value::<KnowledgeAccessContext>(value).is_err());
        assert_eq!(
            serde_json::to_value(KnowledgeSpacePurpose::Own).unwrap(),
            "self"
        );
    }

    #[test]
    fn durable_project_identity_ignores_routes_but_never_storage_authorities() {
        let original = KnowledgeProjectIdentity {
            project_id: "lineage".into(),
            project_root: "/old".into(),
            storage_authority: "store-a".into(),
        };
        let mut moved = original.clone();
        moved.project_root = "/moved".into();
        assert!(original.same_project(&moved));
        moved.storage_authority = "store-b".into();
        assert!(!original.same_project(&moved));
        moved = original.clone();
        moved.project_id = "replacement".into();
        assert!(!original.same_project(&moved));
    }
}
