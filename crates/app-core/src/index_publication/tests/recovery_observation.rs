use super::*;
use crate::index_publication::recovery_observation::RecoveredSourceObservation;
use lumvise_contracts::SemanticIndexObservation;
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

#[test]
fn newer_working_generation_with_same_scoped_members_keeps_frozen_input_current() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let partition = source_partition(vec![]);
    let frozen_scope = journal
        .observe_recovered_scope(&partition, &control())
        .unwrap();
    let frozen = frozen_observation(&frozen_scope, &fixture.project_id);

    advance_selection_generation(&fixture, "working");
    let current = journal
        .observe_recovered_scope(&partition, &control())
        .unwrap();

    assert!(current.working.selection.generation > frozen.expected.generation);
    assert!(current.matches_frozen(&frozen));
}

#[test]
fn changed_scoped_members_make_frozen_input_stale() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let partition = source_partition(vec![source_path("a")]);
    let frozen_scope = journal
        .observe_recovered_scope(&partition, &control())
        .unwrap();
    let frozen = frozen_observation(&frozen_scope, &fixture.project_id);
    let edit = fixture.publication(&journal, "recovery-scope-change", &[("a", "A2")], false);
    let mut request = normalized_request(&edit);
    request.producer_base = None;
    accept_publication(&fixture, request);

    let current = journal
        .observe_recovered_scope(&partition, &control())
        .unwrap();

    assert_ne!(current.working.expected.records, frozen.expected.records);
    assert!(!current.matches_frozen(&frozen));
}

#[test]
fn exact_producer_generation_is_required_when_frozen_input_has_a_pin_base() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let partition = source_partition(vec![]);
    let frozen_scope = journal
        .observe_recovered_scope(&partition, &control())
        .unwrap();
    let frozen = frozen_observation(&frozen_scope, &fixture.project_id);
    let base_generation = frozen.producer_base.as_ref().unwrap().expected_generation;
    advance_selection_generation(&fixture, &journal.rows.producer_selection_id());

    let current = journal
        .observe_recovered_scope(&partition, &control())
        .unwrap();

    assert_eq!(
        current.producer.as_ref().unwrap().selection.generation,
        base_generation + 1
    );
    assert!(!current.matches_frozen(&frozen));
}

#[test]
fn frozen_input_without_producer_base_uses_only_working_membership() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let partition = source_partition(vec![]);
    let recovered = journal
        .observe_recovered_scope(&partition, &control())
        .unwrap();
    let mut frozen = frozen_observation(&recovered, &fixture.project_id);
    frozen.producer_base = None;
    advance_selection_generation(&fixture, &journal.rows.producer_selection_id());

    let current = journal
        .observe_recovered_scope(&partition, &control())
        .unwrap();

    assert!(current.matches_frozen(&frozen));
}

#[test]
fn provider_and_path_scopes_exclude_documents_and_unrelated_source_members() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    publish_document_member(&fixture);
    let full_source = journal
        .observe_recovered_scope(&source_partition(vec![]), &control())
        .unwrap();
    let full_ids = record_ids(&full_source.working.expected.records);
    assert_eq!(
        full_ids,
        ["a", "b", "directory"]
            .map(str::to_owned)
            .into_iter()
            .collect()
    );

    let one_path = journal
        .observe_recovered_scope(&source_partition(vec![source_path("a")]), &control())
        .unwrap();
    assert_eq!(
        record_ids(&one_path.working.expected.records),
        ["a".to_owned()].into_iter().collect()
    );

    let pin = journal
        .producer_members_at(
            full_source.producer.as_ref().unwrap().selection.generation,
            &control(),
        )
        .unwrap();
    assert_eq!(
        record_ids(&pin.records),
        ["a", "b", "directory"]
            .map(str::to_owned)
            .into_iter()
            .collect()
    );
}

#[test]
fn unavailable_members_are_observed_without_requesting_revision_bodies() {
    let fixture = CheckoutFixture::new();
    fixture.bootstrap();
    let semantic = Arc::new(MembershipOnlyPersistence::new(fixture.local.clone()));
    let journal = IndexPublicationJournal::new(
        semantic.clone(),
        fixture.local.clone(),
        fixture.binding("A"),
        ROOT.into(),
    )
    .unwrap();

    let observed = journal
        .observe_recovered_scope(&source_partition(vec![]), &control())
        .unwrap();

    let unavailable = SemanticRecordRef::element("unavailable:without-body");
    assert!(observed.working.expected.records.contains(&unavailable));
    assert!(
        observed
            .producer
            .as_ref()
            .unwrap()
            .expected
            .records
            .contains(&unavailable)
    );
    assert_eq!(
        semantic.operations(),
        vec![
            "ProjectIdentity",
            "RevisionSelectionMembers",
            "RevisionSelectionMembers",
            "RevisionSelectionMembers",
            "RevisionSelectionMembers",
            "ProjectIdentity",
        ]
    );
}

fn source_partition(replace_paths: Vec<String>) -> SemanticPartition {
    SemanticPartition {
        project_root: ROOT.into(),
        provider_instance_id: Some(PROVIDER.into()),
        replace_paths,
    }
}

fn frozen_observation(
    recovered: &RecoveredSourceObservation,
    project_id: &str,
) -> SemanticIndexObservation {
    SemanticIndexObservation {
        project_id: project_id.into(),
        selection_id: recovered.working.selection.selection_id.clone(),
        source_context: recovered.working.selection.source_context.clone(),
        producer_base: recovered
            .producer
            .as_ref()
            .map(|producer| SemanticProducerBase {
                selection_id: producer.selection.selection_id.clone(),
                expected_generation: producer.selection.generation,
            }),
        expected: recovered.working.expected.clone(),
    }
}

fn advance_selection_generation(fixture: &CheckoutFixture, selection_id: &str) {
    let observation = current_selection_members(fixture, selection_id);
    let update = reselection_update(fixture, selection_id, observation);
    accept_selection_update(fixture, update);
}

fn current_selection_members(
    fixture: &CheckoutFixture,
    selection_id: &str,
) -> SemanticRevisionSelectionObservation {
    let result = SemanticPersistence::execute(
        &*fixture.local,
        SemanticOperation::RevisionSelectionMembers(SemanticRevisionSelectionQuery {
            project_root: ROOT.into(),
            selection_id: selection_id.into(),
            structure_coverage: SemanticRevisionStructureCoverage::Project,
            artifact_ids: vec![],
        }),
        &control(),
    )
    .unwrap();
    let SemanticResult::RevisionObservation(Some(observation)) = result else {
        panic!("expected complete selection membership");
    };
    observation
}

fn reselection_update(
    fixture: &CheckoutFixture,
    selection_id: &str,
    observation: SemanticRevisionSelectionObservation,
) -> SemanticRevisionSelectionUpdate {
    SemanticRevisionSelectionUpdate {
        operation_id: format!("advance-{selection_id}"),
        project_root: ROOT.into(),
        project_id: fixture.project_id.clone(),
        selection_id: selection_id.into(),
        source_context: observation.selection.source_context.clone(),
        expected_generation: observation.selection.generation,
        artifact_edits: observation.expected.artifact_edits,
        mode: observation.selection.mode,
        records: observation.expected.records,
        connection_ids: observation.expected.connection_ids,
        source_anchor: observation.selection.source_anchor,
    }
}

fn accept_selection_update(fixture: &CheckoutFixture, update: SemanticRevisionSelectionUpdate) {
    let result = SemanticPersistence::execute(
        &*fixture.local,
        SemanticOperation::SelectRevisions(update),
        &control(),
    )
    .unwrap();
    assert!(matches!(
        result,
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(_))
    ));
}

fn accept_publication(fixture: &CheckoutFixture, request: SemanticRevisionPublication) {
    let result = SemanticPersistence::execute(
        &*fixture.local,
        SemanticOperation::PublishRevisions(request),
        &control(),
    )
    .unwrap();
    assert!(matches!(
        result,
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(_))
    ));
}

fn publish_document_member(fixture: &CheckoutFixture) {
    let observation = document_selection_members(fixture);
    let request = document_publication(fixture, observation);
    accept_publication(fixture, request);
}

fn document_selection_members(fixture: &CheckoutFixture) -> SemanticRevisionSelectionObservation {
    let selection = SemanticPersistence::execute(
        &*fixture.local,
        SemanticOperation::RevisionSelectionMembers(SemanticRevisionSelectionQuery {
            project_root: ROOT.into(),
            selection_id: "working".into(),
            structure_coverage: SemanticRevisionStructureCoverage::Partition(SemanticPartition {
                project_root: ROOT.into(),
                provider_instance_id: Some("documents".into()),
                replace_paths: vec![],
            }),
            artifact_ids: vec![],
        }),
        &control(),
    )
    .unwrap();
    let SemanticResult::RevisionObservation(Some(observation)) = selection else {
        panic!("expected working membership for document scope");
    };
    observation
}

fn document_publication(
    fixture: &CheckoutFixture,
    observation: SemanticRevisionSelectionObservation,
) -> SemanticRevisionPublication {
    SemanticRevisionPublication {
        operation_id: "publish-document-member".into(),
        project_root: ROOT.into(),
        project_id: fixture.project_id.clone(),
        selection_id: "working".into(),
        source_context: observation.selection.source_context,
        producer_base: None,
        expected: observation.expected,
        structure: Some(document_structure()),
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    }
}

fn document_element() -> SemanticElement {
    let mut document = normalized_element(&source("notes", "Documentation"));
    document.path = "@documents/notes.md".into();
    document.parent_element_id = None;
    document.semantic_source_id = "document-source".into();
    let indexer_metadata = document.metadata.clone();
    document.metadata = serde_json::json!({
        "provider_instance_id": "documents",
        "producer_element_id": "notes",
        "indexer_metadata": indexer_metadata,
    });
    document
}

fn document_partition() -> SemanticPartition {
    SemanticPartition {
        project_root: ROOT.into(),
        provider_instance_id: Some("documents".into()),
        replace_paths: vec![],
    }
}

fn document_structure() -> SemanticRevisionStructure {
    SemanticRevisionStructure {
        partition: Some(document_partition()),
        elements: vec![document_element()],
        relationships: vec![],
    }
}

fn record_ids(records: &[SemanticRecordRef]) -> BTreeSet<String> {
    records
        .iter()
        .map(|record| record.record_id.clone())
        .collect()
}

struct MembershipOnlyPersistence {
    local: Arc<LocalPersistence>,
    operations: Mutex<Vec<&'static str>>,
}

impl MembershipOnlyPersistence {
    fn new(local: Arc<LocalPersistence>) -> Self {
        Self {
            local,
            operations: Mutex::new(Vec::new()),
        }
    }

    fn operations(&self) -> Vec<&'static str> {
        self.operations.lock().unwrap().clone()
    }
}

impl SemanticPersistence for MembershipOnlyPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        let operation_name = match &operation {
            SemanticOperation::ProjectIdentity { .. } => "ProjectIdentity",
            SemanticOperation::RevisionSelectionMembers(_) => "RevisionSelectionMembers",
            SemanticOperation::RevisionSelection { .. } => "RevisionSelection",
            _ => "UnexpectedOperation",
        };
        self.operations.lock().unwrap().push(operation_name);
        match operation {
            SemanticOperation::RevisionSelectionMembers(query) => {
                self.read_membership(query, control)
            }
            SemanticOperation::RevisionSelection { .. } => self.reject_body_snapshot(),
            operation => SemanticPersistence::execute(&*self.local, operation, control),
        }
    }

    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        SemanticPersistence::readiness(&*self.local)
    }
}

impl MembershipOnlyPersistence {
    fn read_membership(
        &self,
        query: SemanticRevisionSelectionQuery,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        let result = SemanticPersistence::execute(
            &*self.local,
            SemanticOperation::RevisionSelectionMembers(query),
            control,
        )?;
        let SemanticResult::RevisionObservation(Some(observation)) = result else {
            return Err(DbError::invalid_value(
                "membership result",
                "an existing selection observation",
            ));
        };
        Ok(unavailable_membership_result(observation))
    }

    fn reject_body_snapshot(&self) -> PersistenceResult<SemanticResult> {
        Err(DbError::invalid_value(
            "body snapshot",
            "a membership-only recovery observation",
        ))
    }
}

fn unavailable_membership_result(
    mut observation: SemanticRevisionSelectionObservation,
) -> SemanticResult {
    observation
        .expected
        .records
        .push(SemanticRecordRef::element("unavailable:without-body"));
    SemanticResult::RevisionObservation(Some(observation))
}
