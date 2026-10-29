//! Passive citation-retention requests. The source revision owner derives claim identities;
//! callers cannot supply an actor or mutate a general source selection through these inputs.
use super::SemanticRecordRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The frozen destination intent, retained in Knowledge's existing publication journal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticCitationRetentionIntent {
    pub space_id: String,
    pub document_id: String,
    pub publication_id: String,
}

/// Exact evidence to retain before acknowledging its citing document publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticCitationRetentionAcquire {
    pub project_root: String,
    pub project_id: String,
    pub source_context: String,
    pub intent: SemanticCitationRetentionIntent,
    pub records: Vec<SemanticRecordRef>,
    pub artifact_edits: BTreeMap<String, u64>,
}

/// Releases only the original actor's acquired claim at its acknowledged generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticCitationRetentionRelease {
    pub project_root: String,
    pub project_id: String,
    pub intent: SemanticCitationRetentionIntent,
    pub expected_generation: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claim_input_round_trips_but_cannot_supply_ownership_or_pin_identity() {
        let input = json!({"project_root":"/source","project_id":"lineage","source_context":"local",
            "intent":{"space_id":"space","document_id":"document","publication_id":"write"},
            "records":[{"kind":"element","record_id":"original"}],"artifact_edits":{}});
        let request: SemanticCitationRetentionAcquire =
            serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), input);
        for field in ["actor", "selection_id", "expected_generation"] {
            let mut forged = input.clone();
            forged[field] = json!("another-owner");
            assert!(serde_json::from_value::<SemanticCitationRetentionAcquire>(forged).is_err());
        }
    }
}
