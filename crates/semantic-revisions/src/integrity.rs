//! Owns explicit loss acknowledgement and guarded integrity recovery records.
//! Callers use crate-root types; persistence adapters detect absent bodies and commit plans.
use crate::{SemanticRecordKind, SemanticRecordRef, SemanticRevisionError, SemanticRevisionResult};
use serde::{Deserialize, Serialize};

mod recovery;
pub use recovery::*;
mod history;
pub(crate) use history::validate_recovery_history;

/// An acknowledged missing payload, distinct from deliberate retention pruning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionUnavailable {
    pub recovery_operation_id: String,
    pub unavailable_at_ms: i64,
    /// Preserve the original header without inventing an element body.
    pub canonical_lifecycle: Option<String>,
}

impl SemanticRevisionUnavailable {
    pub(crate) fn validate(&self, record: &SemanticRecordRef) -> SemanticRevisionResult<()> {
        let lifecycle_matches = match record.kind {
            SemanticRecordKind::Element => self
                .canonical_lifecycle
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty()),
            SemanticRecordKind::Artifact => self.canonical_lifecycle.is_none(),
        };
        if self.recovery_operation_id.trim().is_empty() || !lifecycle_matches {
            return Err(SemanticRevisionError::invalid_value(
                format!("{}: {self:?}", record.record_id),
                "a recovery operation ID and the retained lifecycle for the exact record kind",
            ));
        }
        Ok(())
    }
}
