//! Engine-neutral domain rules, shared by local and server persistence.
//! Public callers use methods on the exported portable records; engines and
//! query/transport handles never enter this module.

pub(crate) mod artifact_content;
pub(crate) mod fingerprint;
pub(crate) mod graph_views;
pub(crate) mod matching;
pub(crate) mod relationships;
pub(crate) mod validation;

pub use fingerprint::ContentFingerprintParts;
pub use matching::{SemanticIdentityRemap, SemanticStructureReconciliation};
