//! DB Core owns Lumvise persistence behind two named entry points.
//!
//! Portable contracts and domain records live in the private `interface` module.
//! The local and centralized implementations are private behind
//! [`LocalPersistence`] and [`CentralizedPersistence`].

#[cfg(test)]
extern crate self as lumvise_db_core;

mod archive;
mod domain;
mod interface;
mod local;
mod resource;

pub use archive::SemanticArchive;
pub use domain::vector_search::rank_vector_candidates;
pub use domain::{ContentFingerprintParts, SemanticIdentityRemap, SemanticStructureReconciliation};
pub use interface::*;
pub use local::LocalPersistence;
pub use lumvise_semantic_revisions::SemanticElementNameQuery;
pub use resource::CentralizedPersistence;

/// Validates stored vector dimensions and values before a native or remote adapter writes them.
/// Example: `validate_vector_payload("artifact-id", &vector)?`.
pub fn validate_vector_payload(id: &str, vector: &ArtifactTextVector) -> PersistenceResult<()> {
    lumvise_semantic_revisions::validate_vector_payload(id, vector).map_err(DbError::from)
}

#[cfg(test)]
pub(crate) use local::{DbCore, PZ_REQUIRED_ENTRIES, PzArchive, SemanticStorage};
