//! Explicit fixture publication through native storage; transports never invent caller bases.
use lumvise_contracts::SemanticIndexObservation;
use lumvise_db_core::{
    SemanticArtifact, SemanticArtifactRevisionWrite, SemanticOperation, SemanticPartition,
    SemanticPersistence, SemanticResult, SemanticRevisionPublication,
    SemanticRevisionPublicationOutcome, SemanticRevisionSelectionQuery,
    SemanticRevisionStructureCoverage,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

/// Captures native membership before a source fixture is published.
/// Example: `observe_fresh_source_input(persistence, source_batch())` at setup only.
pub(crate) fn observe_fresh_source_input(
    persistence: &dyn SemanticPersistence,
    mut batch: Value,
) -> Value {
    assert!(
        batch["revision_observation"].is_null(),
        "frozen requests keep their original base"
    );
    assert!(
        batch["ingestion_job_id"].is_null(),
        "staged requests keep their original base"
    );
    batch["revision_observation"] = json!(observe_scope(persistence, source_query(&batch)));
    batch
}

fn observe_scope(
    persistence: &dyn SemanticPersistence,
    query: SemanticRevisionSelectionQuery,
) -> SemanticIndexObservation {
    let SemanticResult::ProjectIdentity(identity) = execute(
        persistence,
        SemanticOperation::ProjectIdentity {
            project_root: query.project_root.clone(),
        },
    ) else {
        panic!("expected native fixture lineage")
    };
    let SemanticResult::RevisionObservation(members) = execute(
        persistence,
        SemanticOperation::RevisionSelectionMembers(query),
    ) else {
        panic!("expected native source membership")
    };
    SemanticIndexObservation {
        project_id: identity.project_id,
        selection_id: "working".into(),
        source_context: "local".into(),
        producer_base: None,
        expected: members.map_or_else(Default::default, |members| members.expected),
    }
}

fn source_query(batch: &Value) -> SemanticRevisionSelectionQuery {
    assert!(
        batch["semantic_artifacts"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    assert!(
        batch["removed_artifact_ids"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    let root = batch["project_root"].as_str().unwrap();
    let provider = batch["provider_instance_id"].as_str().unwrap();
    let paths = ["replace_paths", "removed_paths"]
        .into_iter()
        .flat_map(|field| batch[field].as_array().into_iter().flatten())
        .map(|path| path.as_str().unwrap().into())
        .collect();
    SemanticRevisionSelectionQuery {
        project_root: root.into(),
        selection_id: "working".into(),
        structure_coverage: SemanticRevisionStructureCoverage::Partition(SemanticPartition {
            project_root: root.into(),
            provider_instance_id: Some(provider.into()),
            replace_paths: paths,
        }),
        artifact_ids: vec![],
    }
}

/// Seeds an unused exact artifact ID through conditional native publication.
/// Example: create a legacy-format fixture after source ingestion has adopted its project.
pub(crate) fn publish_initial_artifact(
    persistence: &dyn SemanticPersistence,
    root: &str,
    artifact: SemanticArtifact,
    media_type: &str,
) {
    let observed = observe_scope(
        persistence,
        SemanticRevisionSelectionQuery {
            project_root: root.into(),
            selection_id: "working".into(),
            structure_coverage: SemanticRevisionStructureCoverage::None,
            artifact_ids: vec![artifact.artifact_id.clone()],
        },
    );
    assert!(
        observed.expected.records.is_empty(),
        "initial fixtures must not overwrite an existing revision"
    );
    let publication = SemanticRevisionPublication {
        operation_id: format!("fixture-initial-artifact:{}", artifact.artifact_id),
        project_root: root.into(),
        project_id: observed.project_id,
        selection_id: observed.selection_id,
        source_context: observed.source_context,
        producer_base: None,
        expected: observed.expected,
        structure: None,
        artifacts: vec![SemanticArtifactRevisionWrite {
            checkpoint: false,
            artifact,
            media_type: media_type.into(),
        }],
        removed_artifact_ids: vec![],
        source_anchor: None,
    };
    let result = execute(
        persistence,
        SemanticOperation::PublishRevisions(publication),
    );
    assert!(
        matches!(
            result,
            SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(_))
        ),
        "{result:?}"
    );
}

fn execute(persistence: &dyn SemanticPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

#[test]
#[should_panic(expected = "frozen requests keep their original base")]
fn fresh_fixture_observation_refuses_to_rebase_a_frozen_request() {
    let persistence = lumvise_db_core::LocalPersistence::in_memory().unwrap();
    observe_fresh_source_input(&persistence, json!({"revision_observation":{}}));
}
