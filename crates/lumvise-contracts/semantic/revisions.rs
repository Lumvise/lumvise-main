//! Passive revision observations shared with indexing DTOs; policy stays in semantic-revisions.
use serde::{Deserialize, Serialize};

/// The identity namespace of an element revision or artifact state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRecordKind {
    Element,
    Artifact,
}

/// An exact record identity, never its latest successor. Working artifacts additionally need an edit token.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRecordRef {
    pub kind: SemanticRecordKind,
    pub record_id: String,
}

impl SemanticRecordRef {
    /// Refers to an exact element body. Example: `SemanticRecordRef::element("e17")`.
    pub fn element(id: impl Into<String>) -> Self {
        Self {
            kind: SemanticRecordKind::Element,
            record_id: id.into(),
        }
    }

    /// Refers to an artifact state, immutable once frozen. Example: `SemanticRecordRef::artifact("a4")`.
    pub fn artifact(id: impl Into<String>) -> Self {
        Self {
            kind: SemanticRecordKind::Artifact,
            record_id: id.into(),
        }
    }
}

/// The records and connections observed in the write's authoritative scope.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSelectionExpectation {
    pub generation: i64,
    pub records: Vec<SemanticRecordRef>,
    pub connection_ids: Vec<String>,
    /// Original edit counters fence working bodies whose artifact IDs have not changed.
    #[serde(default)]
    pub artifact_edits: std::collections::BTreeMap<String, u64>,
}

/// The pinned editing-base selection fenced independently of the shared publication head.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProducerBase {
    pub selection_id: String,
    pub expected_generation: i64,
}

/// A producer's frozen base; supplied before indexing, never refreshed at commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticIndexObservation {
    pub project_id: String,
    pub selection_id: String,
    pub source_context: String,
    pub producer_base: Option<SemanticProducerBase>,
    pub expected: SemanticSelectionExpectation,
}

/// Explicit selected-family lookup; this read never supplies a publication expectation.
/// Example: resolve `SemanticRecordRef::element("e17")` in the `working` selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionFamilyQuery {
    pub project_root: String,
    pub selection_id: String,
    #[serde(default)]
    pub expected_commit_version: Option<i64>,
    pub records: Vec<SemanticRecordRef>,
}

/// Maps an incoming identity to the revision selected or accepted by its owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionRemap {
    pub incoming: SemanticRecordRef,
    pub accepted: SemanticRecordRef,
}

/// Selected identities at one commit; exact reads keep their original meaning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionFamilyRead {
    pub commit_version: i64,
    pub remaps: Vec<SemanticRevisionRemap>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_round_trip_exact_ids_and_full_width_generation() {
        let observation = SemanticIndexObservation {
            project_id: "lineage".into(),
            producer_base: Some(SemanticProducerBase {
                selection_id: "producer:test-checkout".into(),
                expected_generation: i64::MAX,
            }),
            selection_id: "working".into(),
            source_context: "local".into(),
            expected: SemanticSelectionExpectation {
                generation: i64::MAX,
                records: vec![
                    SemanticRecordRef::element("e17"),
                    SemanticRecordRef::artifact("a4"),
                ],
                connection_ids: vec!["occurrence-1".into(), "occurrence-2".into()],
                artifact_edits: [("a4".into(), 3)].into(),
            },
        };
        let encoded = serde_json::to_vec(&observation).unwrap();
        assert_eq!(
            serde_json::from_slice::<SemanticIndexObservation>(&encoded).unwrap(),
            observation
        );
        assert!(serde_json::from_str::<SemanticIndexObservation>(r#"{"project_id":"p","selection_id":"s","source_context":"c","expected":{},"unexpected":true}"#).is_err());
    }
}
