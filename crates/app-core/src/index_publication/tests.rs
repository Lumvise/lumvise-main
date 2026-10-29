//! Cooperating checkout bases through the public journal boundary and real in-memory persistence.
use super::*;
use lumvise_contracts::{IndexBatchRequest, SemanticElementUpsert, SemanticRelationshipUpsert};
use lumvise_db_core::*;
use serde_json::json;
use sha2::{Digest, Sha256};

mod acknowledgement;
mod failures;
mod frozen_batch;
mod source_checkout;

const ROOT: &str = "/shared-index";
const PROVIDER: &str = "source-index";

pub(super) fn established_source_store() -> Arc<LocalPersistence> {
    let fixture = CheckoutFixture::new();
    fixture.bootstrap();
    fixture.local
}

struct CheckoutFixture {
    local: Arc<LocalPersistence>,
    project_id: String,
}

impl CheckoutFixture {
    fn new() -> Self {
        Self::from_local(LocalPersistence::in_memory().unwrap())
    }

    fn from_local(local: LocalPersistence) -> Self {
        let local = Arc::new(local);
        let result = SemanticPersistence::execute(
            &*local,
            SemanticOperation::ProjectIdentity {
                project_root: ROOT.into(),
            },
            &control(),
        )
        .unwrap();
        let SemanticResult::ProjectIdentity(identity) = result else {
            panic!("expected lineage")
        };
        Self {
            local,
            project_id: identity.project_id,
        }
    }

    fn binding(&self, client: &str) -> IndexCheckoutBinding {
        IndexCheckoutBinding {
            client_instance_id: client.into(),
            checkout_id: "checkout".into(),
            project_id: self.project_id.clone(),
            provider_instance_id: PROVIDER.into(),
            semantic_source_id: "logical-source".into(),
            selection_id: "working".into(),
            source_context: "local".into(),
        }
    }

    fn journal(&self, client: &str) -> IndexPublicationJournal {
        IndexPublicationJournal::new(
            self.local.clone(),
            self.local.clone(),
            self.binding(client),
            ROOT.into(),
        )
        .unwrap()
    }

    fn bootstrap(&self) -> IndexPublicationJournal {
        let journal = self.journal("A");
        journal.attach(&initial_inventory(), &control()).unwrap();
        let first = self.publication(
            &journal,
            "initial",
            &[("directory", ""), ("a", "A1"), ("b", "B1")],
            true,
        );
        journal.freeze(&first, &control()).unwrap();
        let receipt = accepted(self.publish(&journal).unwrap());
        journal.acknowledge(&receipt, &control()).unwrap();
        journal
    }

    fn publication(
        &self,
        journal: &IndexPublicationJournal,
        id: &str,
        files: &[(&str, &str)],
        full: bool,
    ) -> FrozenIndexPublication {
        let partition = SemanticPartition {
            project_root: ROOT.into(),
            provider_instance_id: Some(PROVIDER.into()),
            replace_paths: if full {
                vec![]
            } else {
                files.iter().map(|(id, _)| source_path(id)).collect()
            },
        };
        let observed = journal.observation(&partition, &control()).unwrap();
        let mut relationships = Vec::new();
        for (file, _) in files.iter().filter(|(file, _)| *file != "directory") {
            relationships.push(edge("directory", file, "contains"));
        }
        if files.iter().any(|(id, _)| *id == "a") {
            let target = journal
                .accepted_element("b", &control())
                .unwrap()
                .unwrap_or_else(|| "b".into());
            relationships.extend([edge("a", &target, "calls"), edge("a", &target, "calls")]);
        }
        FrozenIndexPublication {
            operation_id: id.into(),
            batch: IndexBatchRequest {
                provider_instance_id: PROVIDER.into(),
                project_root: ROOT.into(),
                revision_observation: Some(observed),
                replace_paths: partition.replace_paths,
                removed_paths: vec![],
                semantic_sources: vec![],
                semantic_elements: files.iter().map(|(id, text)| source(id, text)).collect(),
                semantic_relationships: relationships,
            },
            inventory: files.iter().map(|(id, text)| digest(id, text)).collect(),
        }
    }

    fn publish(
        &self,
        journal: &IndexPublicationJournal,
    ) -> crate::Result<SemanticRevisionPublicationOutcome> {
        let pending = journal
            .pending(&control())?
            .ok_or_else(|| invalid_checkpoint("missing pending", "a frozen source batch"))?;
        let result = SemanticPersistence::execute(
            &*self.local,
            SemanticOperation::PublishRevisions(normalized_request(&pending)),
            &control(),
        )?;
        let SemanticResult::RevisionPublication(outcome) = result else {
            return Err(invalid_checkpoint(
                "publication result",
                "a receipt or typed conflict",
            ));
        };
        Ok(outcome)
    }
}

fn control() -> InvocationControl {
    InvocationControl::sixty_seconds()
}
fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn source_path(id: &str) -> String {
    if id == "directory" {
        "src".into()
    } else {
        format!("src/{id}.rs")
    }
}
fn digest(id: &str, text: &str) -> IndexPathDigest {
    IndexPathDigest {
        path: source_path(id),
        directory: id == "directory",
        source_sha256: (id != "directory").then(|| sha(text)),
        projection_sha256: Some(sha(text)),
    }
}
fn initial_inventory() -> Vec<IndexPathDigest> {
    vec![
        digest("directory", ""),
        digest("a", "A1"),
        digest("b", "B1"),
    ]
}

fn source(id: &str, text: &str) -> SemanticElementUpsert {
    SemanticElementUpsert {
        revision_dependencies: Vec::new(),
        semantic_element_id: id.into(),
        semantic_source_id: "logical-source".into(),
        path: source_path(id),
        semantic_element_name: id.into(),
        semantic_element_type: if id == "directory" { "folder" } else { "file" }.into(),
        parent_element_id: (id != "directory").then(|| "directory".into()),
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        metadata: Some(
            json!({"kind":if id == "directory" { "directory" } else { "file" },"source_digest_sha256":if id == "directory" { None } else { Some(sha(text)) }}),
        ),
    }
}

fn edge(from: &str, to: &str, kind: &str) -> SemanticRelationshipUpsert {
    SemanticRelationshipUpsert {
        source_element_id: from.into(),
        target_element_id: Some(to.into()),
        relationship_kind: kind.into(),
        relationship_label: kind.into(),
        target_label: None,
        target_locator: None,
    }
}

// This controlled fixture supplies normalized storage facts; production must use Semantic.
fn normalized_request(pending: &FrozenIndexPublication) -> SemanticRevisionPublication {
    let observation = pending.observation().unwrap();
    SemanticRevisionPublication {
        operation_id: pending.operation_id.clone(),
        producer_base: observation.producer_base.clone(),
        project_root: pending.batch.project_root.clone(),
        project_id: observation.project_id.clone(),
        selection_id: observation.selection_id.clone(),
        source_context: observation.source_context.clone(),
        expected: observation.expected.clone(),
        structure: Some(SemanticRevisionStructure {
            partition: Some(pending.partition()),
            elements: pending
                .batch
                .semantic_elements
                .iter()
                .map(normalized_element)
                .collect(),
            relationships: pending
                .batch
                .semantic_relationships
                .iter()
                .map(|edge| SemanticRelationship {
                    project_root: pending.batch.project_root.clone(),
                    source_element_id: edge.source_element_id.clone(),
                    target_element_id: edge.target_element_id.clone().unwrap(),
                    relationship_kind: edge.relationship_kind.clone(),
                    label: edge.relationship_label.clone(),
                    metadata: json!({}),
                })
                .collect(),
        }),
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    }
}

fn normalized_element(item: &SemanticElementUpsert) -> SemanticElement {
    SemanticElement {
        revision_dependencies: item.revision_dependencies.clone(),
        project_root: ROOT.into(),
        semantic_element_id: item.semantic_element_id.clone(),
        semantic_source_id: item.semantic_source_id.clone(),
        path: item.path.clone(),
        name: item.semantic_element_name.clone(),
        element_kind: item.semantic_element_type.clone(),
        parent_element_id: item.parent_element_id.clone(),
        content_fingerprint: item.content_fingerprint.clone(),
        start_line: item.start_line,
        end_line: item.end_line,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"provider_instance_id":PROVIDER,"producer_element_id":item.semantic_element_id,"indexer_metadata":item.metadata}),
    }
}

fn accepted(outcome: SemanticRevisionPublicationOutcome) -> SemanticRevisionPublicationReceipt {
    let SemanticRevisionPublicationOutcome::Accepted(receipt) = outcome else {
        panic!("expected accepted publication: {outcome:?}")
    };
    receipt
}

fn expected(journal: &IndexPublicationJournal, paths: &[&str]) -> SemanticSelectionExpectation {
    journal
        .observation(
            &SemanticPartition {
                project_root: ROOT.into(),
                provider_instance_id: Some(PROVIDER.into()),
                replace_paths: paths.iter().map(|path| (*path).into()).collect(),
            },
            &control(),
        )
        .unwrap()
        .expected
}

#[test]
fn accepting_one_scope_does_not_adopt_another_checkouts_untouched_content() {
    let fixture = CheckoutFixture::new();
    let a = fixture.bootstrap();
    let b = fixture.journal("B");
    b.attach(&initial_inventory(), &control()).unwrap();
    let a_before = expected(&a, &["src/b.rs"]);
    let a_edit = fixture.publication(&a, "A-edit", &[("a", "A2")], false);
    a.freeze(&a_edit, &control()).unwrap();
    let b_edit = fixture.publication(&b, "B-edit", &[("b", "B2")], false);
    b.freeze(&b_edit, &control()).unwrap();
    let b_receipt = accepted(fixture.publish(&b).unwrap());
    b.acknowledge(&b_receipt, &control()).unwrap();
    let a_receipt = accepted(fixture.publish(&a).unwrap());
    a.acknowledge(&a_receipt, &control()).unwrap();
    assert_eq!(expected(&a, &["src/b.rs"]).records, a_before.records);
    assert_eq!(
        expected(&a, &["src/b.rs"]).connection_ids,
        a_before.connection_ids
    );
    assert_ne!(
        expected(&a, &["src/b.rs"]).records,
        expected(&b, &["src/b.rs"]).records
    );
    let stale = fixture.publication(&a, "A-later-stale-B", &[("b", "B3")], false);
    a.freeze(&stale, &control()).unwrap();
    assert!(matches!(
        fixture.publish(&a).unwrap(),
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    assert_eq!(a.pending(&control()).unwrap().unwrap(), stale);
}

#[test]
fn restart_replays_frozen_content_and_old_receipt_after_another_publication() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let (frozen, receipt, binding) = {
        let fixture = CheckoutFixture::from_local(LocalPersistence::open(&path).unwrap());
        let journal = fixture.bootstrap();
        let other = fixture.journal("B");
        other.attach(&initial_inventory(), &control()).unwrap();
        let frozen = fixture.publication(&journal, "lost-ack", &[("a", "A2")], false);
        journal.freeze(&frozen, &control()).unwrap();
        let receipt = accepted(fixture.publish(&journal).unwrap());
        let intervening = fixture.publication(&other, "other-ack", &[("b", "B2")], false);
        other.freeze(&intervening, &control()).unwrap();
        let newer = accepted(fixture.publish(&other).unwrap());
        other.acknowledge(&newer, &control()).unwrap();
        (frozen, receipt, fixture.binding("A"))
    };
    let fixture = CheckoutFixture::from_local(LocalPersistence::open(&path).unwrap());
    let restored = IndexPublicationJournal::new(
        fixture.local.clone(),
        fixture.local.clone(),
        binding,
        ROOT.into(),
    )
    .unwrap();
    assert_eq!(restored.pending(&control()).unwrap().unwrap(), frozen);
    assert_eq!(accepted(fixture.publish(&restored).unwrap()), receipt);
    restored.acknowledge(&receipt, &control()).unwrap();
    restored.acknowledge(&receipt, &control()).unwrap();
    assert_eq!(
        expected(&restored, &["src/b.rs"]).records,
        vec![SemanticRecordRef::element("b")]
    );
    assert!(restored.pending(&control()).unwrap().is_none());
    assert_eq!(
        restored
            .inventory(&control())
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == source_path("a"))
            .unwrap(),
        digest("a", "A2")
    );
    let a_id = receipt
        .remaps
        .iter()
        .find(|map| map.incoming.record_id == "a")
        .unwrap()
        .accepted
        .record_id
        .clone();
    assert_eq!(
        restored.accepted_element("a", &control()).unwrap(),
        Some(a_id)
    );
}

#[test]
fn first_attachment_refuses_different_files_but_existing_base_survives_offline_edits() {
    let fixture = CheckoutFixture::new();
    let a = fixture.bootstrap();
    let different = vec![
        digest("directory", ""),
        digest("a", "other bytes"),
        digest("b", "B1"),
    ];
    let unknown = fixture.journal("unknown");
    assert!(
        unknown
            .attach(&different, &control())
            .unwrap_err()
            .to_string()
            .contains("local bytes")
    );
    assert!(
        unknown
            .rows
            .get::<CheckpointHeader>("header", &control())
            .unwrap()
            .is_none()
    );
    let original = a.inventory(&control()).unwrap();
    a.attach(&different, &control()).unwrap();
    assert_eq!(a.inventory(&control()).unwrap(), original);
}

#[test]
fn frozen_operation_cannot_be_replaced_and_deletion_clears_only_its_scope() {
    let fixture = CheckoutFixture::new();
    let a = fixture.bootstrap();
    let mut deletion = fixture.publication(&a, "delete-a", &[("a", "unused")], false);
    deletion.batch.semantic_elements.clear();
    deletion.batch.semantic_relationships.clear();
    deletion.inventory.clear();
    a.freeze(&deletion, &control()).unwrap();
    let mut changed = deletion.clone();
    changed.operation_id = "replacement".into();
    assert!(a.freeze(&changed, &control()).is_err());
    assert!(a.discard_unpublished_draft(&control()).is_err());
    let receipt = accepted(fixture.publish(&a).unwrap());
    a.acknowledge(&receipt, &control()).unwrap();
    assert!(expected(&a, &["src/a.rs"]).records.is_empty());
    assert!(a.accepted_element("a", &control()).unwrap().is_none());
    assert_eq!(
        expected(&a, &["src/b.rs"]).records,
        vec![SemanticRecordRef::element("b")]
    );
    assert_eq!(expected(&a, &["src"]).connection_ids.len(), 1);
}
