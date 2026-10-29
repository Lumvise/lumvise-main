//! One engine-neutral semantic revision policy for native persistence and the web gateway.
//! Callers use crate-root records and policy entrypoints. Database handles, transactions,
//! authorization and scheduling remain in the existing persistence adapters.
//!
//! Build the gateway module explicitly with `cargo rustc -p lumvise-semantic-revisions
//! --target wasm32-unknown-unknown --release --lib --crate-type cdylib`. Native
//! dependents use hashed rlibs so separate workspaces cannot overwrite their ABI.

mod archive_merge;
mod artifact_content;
mod artifact_working;
mod bootstrap;
mod canonical_json;
mod citation_retention;
mod connection_identities;
mod connection_resolution;
mod element_name_query;
mod error;
mod family_selection;
mod fingerprint;
mod matching;
mod metadata;
mod policy_bytes;
mod producer_base;
mod provider_partition;
mod publication;
mod publication_basis;
mod publication_records;
mod record_header;
mod records;
mod relationships;
mod required_revisions;
mod retention;
mod retention_eligibility;
mod revision_index;
mod revisions;
mod selection;
mod selection_history;
mod selection_snapshot;
mod state;
mod validation;
mod vector_payload;
#[cfg(target_arch = "wasm32")]
mod wasm_bytes;

pub use artifact_content::{SEARCHABLE_ARTIFACT_TEXT_LIMIT_BYTES, searchable_artifact_text};
pub use citation_retention::*;
pub use connection_resolution::SemanticRevisionConnectionQuery;
pub use element_name_query::SemanticElementNameQuery;
pub use error::{SemanticRevisionError, SemanticRevisionResult};
pub use family_selection::SemanticRevisionFamilyBasis;
pub use fingerprint::ContentFingerprintParts;
pub use matching::{IdentityRemapIndex, SemanticIdentityRemap, SemanticStructureReconciliation};
pub use metadata::{SemanticRevisionMember, SemanticRevisionMembership, SemanticRevisionMetadata};
pub use policy_bytes::evaluate_revision_policy;
pub use publication_basis::*;
pub use record_header::SemanticRevisionRecordHeader;
pub use records::*;
pub use relationships::{
    contains_relationship_from_parent_hint, contains_relationships_from_parent_hints,
    normalized_relationships, relationship_key, remap_relationships,
};
pub use revision_index::revision_id as semantic_revision_successor_id;
pub use revisions::*;
pub use selection_history::SemanticRevisionSelectionDelta;
pub use vector_payload::validate_vector_payload;

pub use state::*;
