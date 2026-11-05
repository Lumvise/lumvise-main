use super::{InvocationControl, LocalPersistence, SemanticOperation, SemanticPersistence};
use crate::{
    SemanticElement, SemanticPartition, SemanticRelationship, SemanticResult,
    SemanticRevisionMember, SemanticRevisionMembership, SemanticRevisionMetadata,
    SemanticRevisionPublication, SemanticRevisionPublicationOutcome,
    SemanticRevisionSelectionSnapshot, SemanticRevisionState, SemanticRevisionStructure,
    SemanticSelectionExpectation,
};
use grafeo::Value as GrafeoValue;
use serde_json::json;
use tempfile::TempDir;

const ROOT: &str = "/membership-copy";
const FOREIGN_ROOT: &str = "/membership-copy-foreign";
const PROVIDER: &str = "membership-fixture";
const FOREIGN_SELECTION_ID: &str = "foreign-working";
const EDGE_ID: &str =
    "connection:64bdd3be0eb22038b37a10937848a82bee27841baebd4ca89211626a72ba04fc:0";

struct MembershipFixture {
    directory: TempDir,
    persistence: LocalPersistence,
    project_id: String,
    initial_receipt: crate::SemanticRevisionPublicationReceipt,
}

impl MembershipFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let persistence = LocalPersistence::open(directory.path().join("project.db")).unwrap();
        let project_id = match execute(
            &persistence,
            SemanticOperation::ProjectIdentity {
                project_root: ROOT.into(),
            },
        ) {
            SemanticResult::ProjectIdentity(identity) => identity.project_id,
            other => panic!("{other:?}; expected project identity"),
        };
        let result = execute(
            &persistence,
            SemanticOperation::PublishRevisions(publication(
                &project_id,
                SemanticSelectionExpectation::default(),
                "membership-initial",
                vec![calls_edge()],
            )),
        );
        let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            initial_receipt,
        )) = result
        else {
            panic!("{result:?}; expected accepted initial publication")
        };
        assert_eq!(
            initial_receipt.accepted_scope.connection_ids,
            vec![EDGE_ID.to_owned()]
        );
        Self {
            directory,
            persistence,
            project_id,
            initial_receipt,
        }
    }

    fn reopen(self) -> Self {
        let Self {
            directory,
            persistence,
            project_id,
            initial_receipt,
        } = self;
        let path = directory.path().join("project.db");
        drop(persistence);
        Self {
            directory,
            persistence: LocalPersistence::open(path).unwrap(),
            project_id,
            initial_receipt,
        }
    }
}

fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn revision_history(persistence: &LocalPersistence) -> SemanticRevisionState {
    match execute(
        persistence,
        SemanticOperation::RevisionHistory {
            project_root: ROOT.into(),
        },
    ) {
        SemanticResult::RevisionHistory(state) => state,
        other => panic!("{other:?}; expected revision history"),
    }
}

fn revision_selection(persistence: &LocalPersistence) -> SemanticRevisionSelectionSnapshot {
    match execute(
        persistence,
        SemanticOperation::RevisionSelection {
            project_root: ROOT.into(),
            selection_id: "working".into(),
        },
    ) {
        SemanticResult::RevisionSelection(Some(snapshot)) => snapshot,
        other => panic!("{other:?}; expected working revision selection"),
    }
}

fn semantic_commit_version(persistence: &LocalPersistence) -> i64 {
    match execute(persistence, SemanticOperation::SemanticRevision) {
        SemanticResult::SemanticRevision { commit_version } => commit_version,
        other => panic!("{other:?}; expected semantic commit version"),
    }
}

fn publication(
    project_id: &str,
    expected: SemanticSelectionExpectation,
    operation_id: &str,
    relationships: Vec<SemanticRelationship>,
) -> SemanticRevisionPublication {
    SemanticRevisionPublication {
        operation_id: operation_id.into(),
        producer_base: None,
        project_root: ROOT.into(),
        project_id: project_id.into(),
        selection_id: "working".into(),
        source_context: PROVIDER.into(),
        expected,
        structure: Some(SemanticRevisionStructure {
            partition: Some(SemanticPartition {
                project_root: ROOT.into(),
                replace_paths: vec!["a.rs".into(), "b.rs".into()],
                provider_instance_id: Some(PROVIDER.into()),
            }),
            elements: vec![element("a"), element("b")],
            relationships,
        }),
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    }
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: id.into(),
        semantic_source_id: PROVIDER.into(),
        path: format!("{id}.rs"),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: None,
        revision_dependencies: vec![],
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"provider_instance_id":PROVIDER}),
    }
}

fn calls_edge() -> SemanticRelationship {
    SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: "a".into(),
        target_element_id: "b".into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    }
}

fn revision_row_ids(
    persistence: &LocalPersistence,
    matches: impl Fn(&SemanticRevisionMetadata) -> bool,
) -> Vec<grafeo::NodeId> {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .read(|graph| {
            graph
                .find_nodes_by_property("revision_project_root", &GrafeoValue::from(ROOT))
                .unwrap()
                .into_iter()
                .filter_map(|id| {
                    let node = graph
                        .get_node(id)
                        .unwrap()
                        .filter(|node| node.has_label("SemanticRevisionRow"))?;
                    let payload = node.get_property("revision_row_json")?.as_str()?;
                    let row: SemanticRevisionMetadata =
                        serde_json::from_str(payload).expect("valid revision metadata payload");
                    matches(&row).then_some(id)
                })
                .collect()
        })
}

fn connection_membership_nodes(
    persistence: &LocalPersistence,
    connection_id: &str,
) -> Vec<grafeo::NodeId> {
    revision_row_ids(persistence, |row| {
        matches!(row, SemanticRevisionMetadata::Membership(member)
            if member.selection_id == "working"
                && member.connection_id() == Some(connection_id))
    })
}

fn working_selection_nodes(persistence: &LocalPersistence) -> Vec<grafeo::NodeId> {
    revision_row_ids(persistence, |row| {
        matches!(row, SemanticRevisionMetadata::Selection(selection)
            if selection.selection_id == "working")
    })
}

fn revision_row_payload(persistence: &LocalPersistence, id: grafeo::NodeId) -> String {
    revision_row_property(persistence, id, "revision_row_json")
}

fn revision_row_key(persistence: &LocalPersistence, id: grafeo::NodeId) -> String {
    revision_row_property(persistence, id, "revision_row_key")
}

fn revision_row_property(
    persistence: &LocalPersistence,
    id: grafeo::NodeId,
    property: &str,
) -> String {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .read(|graph| {
            graph
                .get_node(id)
                .unwrap()
                .expect("source revision row")
                .get_property(property)
                .and_then(GrafeoValue::as_str)
                .expect("revision row string property")
                .to_owned()
        })
}

fn revision_row_string_properties(
    persistence: &LocalPersistence,
    id: grafeo::NodeId,
) -> std::collections::BTreeMap<String, String> {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .read(|graph| {
            graph
                .get_node(id)
                .unwrap()
                .expect("source revision row")
                .properties
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.as_str().to_owned(), value.to_owned()))
                })
                .collect()
        })
}

fn revision_rows_with_key(persistence: &LocalPersistence, key: &str) -> Vec<grafeo::NodeId> {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .read(|graph| {
            graph
                .find_nodes_by_property("revision_row_key", &GrafeoValue::from(key))
                .unwrap()
                .into_iter()
                .filter(|id| {
                    graph
                        .get_node(*id)
                        .unwrap()
                        .is_some_and(|node| node.has_label("SemanticRevisionRow"))
                })
                .collect()
        })
}

fn clone_revision_row(
    persistence: &LocalPersistence,
    source_id: grafeo::NodeId,
    payload_override: Option<String>,
) -> grafeo::NodeId {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| {
            let source = graph
                .get_node(source_id)
                .unwrap()
                .expect("source revision row");
            let labels = source
                .labels
                .iter()
                .map(|label| label.as_str())
                .collect::<Vec<_>>();
            let properties = source
                .properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect::<Vec<_>>();
            let copy = graph.create_node_with_props(&labels, properties)?;
            if let Some(payload) = payload_override {
                graph.set_node_property(copy, "revision_row_json", GrafeoValue::from(payload))?;
            }
            Ok(copy)
        })
        .unwrap()
}

fn corrupt_revision_row_property(
    persistence: &LocalPersistence,
    id: grafeo::NodeId,
    property: &str,
    value: &str,
) {
    persistence
        .core
        .storage_manager()
        .semantic_storage()
        .graph
        .write(|graph| graph.set_node_property(id, property, GrafeoValue::from(value)))
        .unwrap();
}

fn clone_foreign_connection_membership(
    persistence: &LocalPersistence,
    source_id: grafeo::NodeId,
) -> (grafeo::NodeId, String) {
    let metadata = SemanticRevisionMetadata::Membership(SemanticRevisionMembership {
        selection_id: FOREIGN_SELECTION_ID.into(),
        member: SemanticRevisionMember::Connection(EDGE_ID.into()),
    });
    let payload = serde_json::to_string(&metadata).unwrap();
    let key = serde_json::to_string(&(FOREIGN_ROOT, metadata.kind(), metadata.identity().unwrap()))
        .unwrap();
    let copy = clone_revision_row(persistence, source_id, Some(payload));
    let selection_key = serde_json::to_string(&(FOREIGN_ROOT, FOREIGN_SELECTION_ID)).unwrap();
    corrupt_revision_row_property(persistence, copy, "revision_project_root", FOREIGN_ROOT);
    corrupt_revision_row_property(persistence, copy, "revision_row_key", &key);
    corrupt_revision_row_property(persistence, copy, "revision_selection_key", &selection_key);
    (copy, key)
}

#[test]
fn identical_connection_membership_copies_read_once_coalesce_on_publish_and_remove() {
    let fixture = MembershipFixture::new();
    let before_history = revision_history(&fixture.persistence);
    let before_selection = revision_selection(&fixture.persistence);
    assert_eq!(before_selection.selection.generation, 1);
    assert_eq!(
        before_selection.expectation.connection_ids,
        vec![EDGE_ID.to_owned()]
    );
    assert_eq!(before_selection.connections.len(), 1);
    let edge = &before_selection.connections[0];
    assert_eq!(edge.connection_id, EDGE_ID);
    assert_eq!(edge.relationship.source_element_id, "a");
    assert_eq!(edge.relationship.target_element_id, "b");
    assert_eq!(edge.relationship.relationship_kind, "calls");
    assert_eq!(edge.relationship.label, "calls");
    assert_eq!(edge.relationship.metadata, json!({}));
    assert_eq!(
        before_history.accepted_operations[0].receipt,
        fixture.initial_receipt
    );

    let membership = connection_membership_nodes(&fixture.persistence, EDGE_ID);
    assert_eq!(membership.len(), 1);
    clone_revision_row(&fixture.persistence, membership[0], None);
    assert_eq!(
        connection_membership_nodes(&fixture.persistence, EDGE_ID).len(),
        2
    );

    let after_history = revision_history(&fixture.persistence);
    let after_selection = revision_selection(&fixture.persistence);
    assert_eq!(after_history, before_history);
    assert_eq!(after_selection, before_selection);
    assert_eq!(after_selection.connections.len(), 1);
    assert_eq!(after_selection.connections[0].connection_id, EDGE_ID);

    let repeated = execute(
        &fixture.persistence,
        SemanticOperation::PublishRevisions(publication(
            &fixture.project_id,
            after_selection.expectation,
            "membership-repeat-edge",
            vec![calls_edge()],
        )),
    );
    let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
        repeated_receipt,
    )) = repeated
    else {
        panic!("{repeated:?}; expected accepted repeated edge publication")
    };
    assert_eq!(
        repeated_receipt.accepted_scope.connection_ids,
        vec![EDGE_ID.to_owned()]
    );
    assert_eq!(
        connection_membership_nodes(&fixture.persistence, EDGE_ID).len(),
        1
    );

    let membership = connection_membership_nodes(&fixture.persistence, EDGE_ID);
    clone_revision_row(&fixture.persistence, membership[0], None);
    assert_eq!(
        connection_membership_nodes(&fixture.persistence, EDGE_ID).len(),
        2
    );

    let current = revision_selection(&fixture.persistence);
    let removed = execute(
        &fixture.persistence,
        SemanticOperation::PublishRevisions(publication(
            &fixture.project_id,
            current.expectation,
            "membership-remove-edge",
            vec![],
        )),
    );
    let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
        removal_receipt,
    )) = removed
    else {
        panic!("{removed:?}; expected accepted edge removal")
    };
    assert!(removal_receipt.accepted_scope.connection_ids.is_empty());
    assert!(
        revision_selection(&fixture.persistence)
            .connections
            .is_empty()
    );
    assert!(connection_membership_nodes(&fixture.persistence, EDGE_ID).is_empty());

    let fixture = fixture.reopen();
    assert!(
        revision_selection(&fixture.persistence)
            .connections
            .is_empty()
    );
    assert!(
        revision_history(&fixture.persistence)
            .selection("working")
            .unwrap()
            .connection_ids
            .is_empty()
    );
    assert!(connection_membership_nodes(&fixture.persistence, EDGE_ID).is_empty());
}

#[test]
fn conflicting_same_key_membership_payload_fails_before_publication_commit() {
    let fixture = MembershipFixture::new();
    let before = revision_selection(&fixture.persistence);
    let commit_before = semantic_commit_version(&fixture.persistence);
    let membership = connection_membership_nodes(&fixture.persistence, EDGE_ID);
    assert_eq!(membership.len(), 1);
    let payload = revision_row_payload(&fixture.persistence, membership[0]);
    clone_revision_row(
        &fixture.persistence,
        membership[0],
        Some(format!(" {payload}")),
    );
    assert_eq!(
        connection_membership_nodes(&fixture.persistence, EDGE_ID).len(),
        2
    );

    let error = fixture
        .persistence
        .execute(
            SemanticOperation::PublishRevisions(publication(
                &fixture.project_id,
                before.expectation,
                "membership-conflicting-copy",
                vec![calls_edge()],
            )),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("byte-identical canonical membership copies"),
        "{error}"
    );
    assert_eq!(semantic_commit_version(&fixture.persistence), commit_before);
    assert_eq!(
        connection_membership_nodes(&fixture.persistence, EDGE_ID).len(),
        2
    );
}

#[test]
fn differing_same_key_membership_identity_fails_before_publication_commit() {
    let fixture = MembershipFixture::new();
    let before = revision_selection(&fixture.persistence);
    let commit_before = semantic_commit_version(&fixture.persistence);
    let membership = connection_membership_nodes(&fixture.persistence, EDGE_ID);
    assert_eq!(membership.len(), 1);
    let original_payload = revision_row_payload(&fixture.persistence, membership[0]);
    let mut conflict: serde_json::Value = serde_json::from_str(&original_payload).unwrap();
    conflict["value"]["member"]["value"] = json!("connection:conflicting-copy");
    let conflicting_payload = serde_json::to_string(&conflict).unwrap();
    let conflict_id = clone_revision_row(
        &fixture.persistence,
        membership[0],
        Some(conflicting_payload.clone()),
    );
    let row_key = revision_row_key(&fixture.persistence, membership[0]);
    assert_eq!(revision_row_key(&fixture.persistence, conflict_id), row_key);
    let selection_key = serde_json::to_string(&(ROOT, "working")).unwrap();
    for id in [membership[0], conflict_id] {
        assert_eq!(
            revision_row_property(&fixture.persistence, id, "revision_selection_key"),
            selection_key
        );
        assert_eq!(
            revision_row_property(&fixture.persistence, id, "revision_membership_connection"),
            EDGE_ID
        );
    }
    assert_eq!(
        revision_rows_with_key(&fixture.persistence, &row_key).len(),
        2
    );

    let error = fixture
        .persistence
        .execute(
            SemanticOperation::PublishRevisions(publication(
                &fixture.project_id,
                before.expectation,
                "membership-differing-copy",
                vec![calls_edge()],
            )),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("a nonempty membership matching its canonical key"),
        "{error}"
    );
    assert_eq!(semantic_commit_version(&fixture.persistence), commit_before);
    let payloads = revision_rows_with_key(&fixture.persistence, &row_key)
        .into_iter()
        .map(|id| revision_row_payload(&fixture.persistence, id))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        payloads,
        [original_payload, conflicting_payload]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
}

#[test]
fn lookup_index_corruption_in_any_membership_copy_blocks_publication_without_mutation() {
    let fixture = MembershipFixture::new();
    let before = revision_selection(&fixture.persistence);
    let commit_before = semantic_commit_version(&fixture.persistence);
    let membership = connection_membership_nodes(&fixture.persistence, EDGE_ID);
    assert_eq!(membership.len(), 1);
    let original = membership[0];
    let copy = clone_revision_row(&fixture.persistence, original, None);
    let row_key = revision_row_key(&fixture.persistence, original);
    assert_eq!(revision_row_key(&fixture.persistence, copy), row_key);
    assert_eq!(
        revision_row_payload(&fixture.persistence, original),
        revision_row_payload(&fixture.persistence, copy)
    );

    let corrupt_selection_key = serde_json::to_string(&(ROOT, "other-selection")).unwrap();
    corrupt_revision_row_property(
        &fixture.persistence,
        original,
        "revision_selection_key",
        &corrupt_selection_key,
    );
    let original_before = revision_row_string_properties(&fixture.persistence, original);
    let copy_before = revision_row_string_properties(&fixture.persistence, copy);
    assert_eq!(
        original_before.get("revision_selection_key"),
        Some(&corrupt_selection_key)
    );
    let expected_selection_key = serde_json::to_string(&(ROOT, "working")).unwrap();
    assert_eq!(
        copy_before.get("revision_selection_key"),
        Some(&expected_selection_key)
    );

    let error = fixture
        .persistence
        .execute(
            SemanticOperation::PublishRevisions(publication(
                &fixture.project_id,
                before.expectation,
                "membership-invalid-lookup-copy",
                vec![calls_edge()],
            )),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("canonical membership lookup property revision_selection_key"),
        "{error}"
    );
    assert_eq!(semantic_commit_version(&fixture.persistence), commit_before);
    assert_eq!(
        revision_rows_with_key(&fixture.persistence, &row_key).len(),
        2
    );
    assert_eq!(
        revision_row_string_properties(&fixture.persistence, original),
        original_before
    );
    assert_eq!(
        revision_row_string_properties(&fixture.persistence, copy),
        copy_before
    );
}

#[test]
fn foreign_project_membership_index_hits_do_not_poison_scoped_publication() {
    let fixture = MembershipFixture::new();
    let before = revision_selection(&fixture.persistence);
    let membership = connection_membership_nodes(&fixture.persistence, EDGE_ID);
    assert_eq!(membership.len(), 1);

    // These orphaned B membership rows isolate index scoping without creating a B project.
    let (foreign_one, foreign_key) =
        clone_foreign_connection_membership(&fixture.persistence, membership[0]);
    let (foreign_two, duplicate_key) =
        clone_foreign_connection_membership(&fixture.persistence, membership[0]);
    assert_eq!(foreign_key, duplicate_key);
    assert_ne!(
        foreign_key,
        revision_row_key(&fixture.persistence, membership[0])
    );
    assert_eq!(
        revision_rows_with_key(&fixture.persistence, &foreign_key).len(),
        2
    );
    let expected_selection_key =
        serde_json::to_string(&(FOREIGN_ROOT, FOREIGN_SELECTION_ID)).unwrap();
    for id in [foreign_one, foreign_two] {
        assert_eq!(
            revision_row_property(&fixture.persistence, id, "revision_project_root"),
            FOREIGN_ROOT
        );
        assert_eq!(revision_row_key(&fixture.persistence, id), foreign_key);
        assert_eq!(
            revision_row_property(&fixture.persistence, id, "revision_selection_key"),
            expected_selection_key
        );
        assert_eq!(
            revision_row_property(&fixture.persistence, id, "revision_membership_connection"),
            EDGE_ID
        );
    }
    let foreign_one_before = revision_row_string_properties(&fixture.persistence, foreign_one);
    let foreign_two_before = revision_row_string_properties(&fixture.persistence, foreign_two);

    let result = execute(
        &fixture.persistence,
        SemanticOperation::PublishRevisions(publication(
            &fixture.project_id,
            before.expectation,
            "membership-foreign-project-index-isolation",
            vec![calls_edge()],
        )),
    );
    let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(receipt)) =
        result
    else {
        panic!("{result:?}; expected A scoped publication to succeed")
    };
    assert_eq!(
        receipt.accepted_scope.connection_ids,
        vec![EDGE_ID.to_owned()]
    );
    let after = revision_selection(&fixture.persistence);
    assert_eq!(after.selection.generation, 2);
    assert_eq!(after.connections.len(), 1);
    assert_eq!(after.connections[0].connection_id, EDGE_ID);
    assert_eq!(after.connections[0].relationship.source_element_id, "a");
    assert_eq!(after.connections[0].relationship.target_element_id, "b");
    assert_eq!(after.connections[0].relationship.relationship_kind, "calls");
    assert_eq!(after.connections[0].relationship.label, "calls");
    assert_eq!(after.connections[0].relationship.metadata, json!({}));
    assert_eq!(
        connection_membership_nodes(&fixture.persistence, EDGE_ID).len(),
        1
    );

    assert_eq!(
        revision_rows_with_key(&fixture.persistence, &foreign_key).len(),
        2
    );
    assert_eq!(
        revision_row_string_properties(&fixture.persistence, foreign_one),
        foreign_one_before
    );
    assert_eq!(
        revision_row_string_properties(&fixture.persistence, foreign_two),
        foreign_two_before
    );
}

#[test]
fn duplicate_nonmembership_metadata_remains_rejected() {
    let fixture = MembershipFixture::new();
    let commit_before = semantic_commit_version(&fixture.persistence);
    let selections = working_selection_nodes(&fixture.persistence);
    assert_eq!(selections.len(), 1);
    clone_revision_row(&fixture.persistence, selections[0], None);
    assert_eq!(working_selection_nodes(&fixture.persistence).len(), 2);

    let error = fixture
        .persistence
        .execute(
            SemanticOperation::RevisionHistory {
                project_root: ROOT.into(),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("one stored row per non-membership revision key"),
        "{error}"
    );
    assert_eq!(semantic_commit_version(&fixture.persistence), commit_before);
    assert_eq!(working_selection_nodes(&fixture.persistence).len(), 2);
}
