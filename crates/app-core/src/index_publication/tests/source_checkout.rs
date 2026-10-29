//! Real source parsing and durable bases, with a controlled normalized-publication boundary.
use super::*;
use crate::index_publication::IndexSourceCheckout;
use lumvise_project_indexer::{
    FilesystemProjectSource, ProjectIndexer, ScanScope, SemanticIndexProjection,
    TreeSitterProjectParser,
};
use std::{fs, path::Path};

struct SourceCheckoutFixture {
    store: CheckoutFixture,
    root: tempfile::TempDir,
}

impl SourceCheckoutFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.rs"), "fn a() { b(); }\n").unwrap();
        fs::write(root.path().join("b.rs"), "fn b() {}\n").unwrap();
        Self {
            store: CheckoutFixture::new(),
            root,
        }
    }

    fn copy_checkout(&self) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for file in ["a.rs", "b.rs"] {
            fs::copy(self.root.path().join(file), root.path().join(file)).unwrap();
        }
        root
    }

    fn checkout(&self, root: &Path, client: &str) -> IndexSourceCheckout {
        IndexSourceCheckout::open(
            self.store.local.clone(),
            self.store.local.clone(),
            client,
            root,
            ROOT,
            PROVIDER,
            &control(),
        )
        .unwrap()
    }

    fn prepare(
        &self,
        checkout: &IndexSourceCheckout,
        root: &Path,
    ) -> Option<FrozenIndexPublication> {
        let before = checkout.before_scan(&control()).unwrap();
        let batch = self.projected_batch(checkout, root);
        checkout
            .freeze_batch(batch, true, before, &control())
            .unwrap()
    }

    fn projected_batch(
        &self,
        checkout: &IndexSourceCheckout,
        root: &Path,
    ) -> lumvise_contracts::IndexBatchRequest {
        let source = FilesystemProjectSource::open(root).unwrap();
        let mut projection = SemanticIndexProjection::with_source(
            source.root(),
            PROVIDER,
            checkout.source().clone(),
        )
        .unwrap();
        let scan = ProjectIndexer::new(source, TreeSitterProjectParser::default())
            .prepare(ScanScope::Full)
            .unwrap();
        let mut batch = projection.project(&scan).unwrap();
        batch.project_root = ROOT.into();
        batch
    }

    fn publish(&self, checkout: &IndexSourceCheckout) -> SemanticRevisionPublicationReceipt {
        let pending = checkout.pending(&control()).unwrap().unwrap();
        let result = SemanticPersistence::execute(
            &*self.store.local,
            SemanticOperation::PublishRevisions(normalized_request(&pending)),
            &control(),
        )
        .unwrap();
        let SemanticResult::RevisionPublication(outcome) = result else {
            panic!("expected publication")
        };
        let receipt = accepted(outcome);
        checkout.acknowledge(&receipt, &control()).unwrap();
        receipt
    }
}

#[test]
fn a_restarted_full_scan_does_not_publish_unchanged_stale_files() {
    let fixture = SourceCheckoutFixture::new();
    let initial = fixture.checkout(fixture.root.path(), "A");
    let batch = fixture.prepare(&initial, fixture.root.path()).unwrap();
    assert!(batch.batch.replace_paths.is_empty());
    fixture.publish(&initial);
    let reopened = fixture.checkout(fixture.root.path(), "A");
    assert!(fixture.prepare(&reopened, fixture.root.path()).is_none());
    assert!(reopened.pending(&control()).unwrap().is_none());
}

#[test]
fn different_checkout_roots_cooperate_without_adopting_untouched_server_revisions() {
    let fixture = SourceCheckoutFixture::new();
    fs::write(fixture.root.path().join("a.rs"), "fn a() {}\n").unwrap();
    let a = fixture.checkout(fixture.root.path(), "A");
    fixture.prepare(&a, fixture.root.path()).unwrap();
    fixture.publish(&a);
    let other_root = fixture.copy_checkout();
    let b = fixture.checkout(other_root.path(), "B");
    assert_eq!(a.source().semantic_source_id, b.source().semantic_source_id);
    fixture.prepare(&b, other_root.path()).unwrap();
    fixture.publish(&b);
    fs::write(
        fixture.root.path().join("a.rs"),
        "fn a() { let changed = 1; }\n",
    )
    .unwrap();
    let edit_a = fixture.prepare(&a, fixture.root.path()).unwrap();
    assert_eq!(edit_a.batch.replace_paths, ["a.rs"]);
    fs::write(
        other_root.path().join("b.rs"),
        "fn b() { let changed = 2; }\n",
    )
    .unwrap();
    let edit_b = fixture.prepare(&b, other_root.path()).unwrap();
    assert_eq!(edit_b.batch.replace_paths, ["b.rs"]);
    fixture.publish(&b);
    fixture.publish(&a);
    fs::write(
        fixture.root.path().join("b.rs"),
        "fn b() { let stale_edit = 3; }\n",
    )
    .unwrap();
    let stale = fixture.prepare(&a, fixture.root.path()).unwrap();
    let result = SemanticPersistence::execute(
        &*fixture.store.local,
        SemanticOperation::PublishRevisions(normalized_request(&stale)),
        &control(),
    )
    .unwrap();
    assert!(matches!(
        result,
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Conflict(_))
    ));
}

#[test]
fn startup_client_identity_can_be_reused_but_cannot_be_replaced() {
    let app = crate::AppCore::in_memory().unwrap();
    assert!(app.install_index_publication_client(" ".into()).is_err());
    app.install_index_publication_client("stable-client".into())
        .unwrap();
    app.install_index_publication_client("stable-client".into())
        .unwrap();
    assert!(
        app.install_index_publication_client("different-client".into())
            .is_err()
    );
    assert_eq!(app.index_publication_client.get().unwrap(), "stable-client");
}

#[test]
fn parser_owned_locators_follow_acknowledged_target_ids() {
    let fixture = SourceCheckoutFixture::new();
    let checkout = fixture.checkout(fixture.root.path(), "A");
    let first = fixture.prepare(&checkout, fixture.root.path()).unwrap();
    let original = first.batch.semantic_relationships[0]
        .target_element_id
        .clone();
    fixture.publish(&checkout);
    fs::write(
        fixture.root.path().join("b.rs"),
        "fn b() { let revision = 2; }\n",
    )
    .unwrap();
    fixture.prepare(&checkout, fixture.root.path()).unwrap();
    fixture.publish(&checkout);
    fs::write(
        fixture.root.path().join("a.rs"),
        "fn a() { b(); let revision = 2; }\n",
    )
    .unwrap();
    let edit = fixture.prepare(&checkout, fixture.root.path()).unwrap();
    let edge = edit
        .batch
        .semantic_relationships
        .iter()
        .find(|edge| edge.relationship_label == "calls")
        .unwrap();
    assert_ne!(edge.target_element_id, original);
    assert_eq!(edge.target_locator, edge.target_element_id);
    assert_eq!(edge.target_label, edge.target_element_id);
}

#[test]
fn reopening_a_source_recovers_preparation_that_never_became_publishable() {
    let fixture = SourceCheckoutFixture::new();
    let interrupted = IndexSourceCheckout::open(
        fixture.store.local.clone(),
        super::failures::fail_next_seal(fixture.store.local.clone()),
        "A",
        fixture.root.path(),
        ROOT,
        PROVIDER,
        &control(),
    )
    .unwrap();
    let before = interrupted.before_scan(&control()).unwrap();
    let batch = fixture.projected_batch(&interrupted, fixture.root.path());
    assert!(
        interrupted
            .freeze_batch(batch, true, before, &control())
            .is_err()
    );
    let reopened = fixture.checkout(fixture.root.path(), "A");
    assert!(reopened.pending(&control()).unwrap().is_none());
    fixture.prepare(&reopened, fixture.root.path()).unwrap();
    fixture.publish(&reopened);
    assert!(reopened.pending(&control()).unwrap().is_none());
}

#[test]
fn interrupted_initial_attachment_republishes_the_whole_verified_base_before_pinning() {
    let fixture = SourceCheckoutFixture::new();
    let first = fixture.checkout(fixture.root.path(), "A");
    let original = fixture.prepare(&first, fixture.root.path()).unwrap();
    fixture.publish(&first);
    let second_root = fixture.copy_checkout();
    let interrupted = IndexSourceCheckout::open(
        fixture.store.local.clone(),
        super::failures::fail_next_seal(fixture.store.local.clone()),
        "B",
        second_root.path(),
        ROOT,
        PROVIDER,
        &control(),
    )
    .unwrap();
    let before = interrupted.before_scan(&control()).unwrap();
    let batch = fixture.projected_batch(&interrupted, second_root.path());
    assert!(
        interrupted
            .freeze_batch(batch, true, before, &control())
            .is_err()
    );
    let reopened = fixture.checkout(second_root.path(), "B");
    assert!(reopened.pending(&control()).unwrap().is_none());
    let retried = fixture.prepare(&reopened, second_root.path()).unwrap();
    assert_eq!(
        retried.batch.semantic_elements,
        original.batch.semantic_elements
    );
    assert_eq!(retried.inventory, original.inventory);
    let receipt = fixture.publish(&reopened);
    let pin = receipt.producer_base.unwrap();
    let result = SemanticPersistence::execute(
        &*fixture.store.local,
        SemanticOperation::RevisionSelection {
            project_root: ROOT.into(),
            selection_id: pin.selection_id,
        },
        &control(),
    )
    .unwrap();
    let SemanticResult::RevisionSelection(Some(selected)) = result else {
        panic!("expected the durable producer pin");
    };
    assert_eq!(
        selected.elements.len(),
        original.batch.semantic_elements.len()
    );
    assert_eq!(
        selected.expectation,
        SemanticSelectionExpectation {
            artifact_edits: Default::default(),
            generation: 1,
            ..receipt.accepted_scope
        }
    );
}

#[test]
fn a_changed_target_keeps_the_unchanged_callers_required_reindexing_work() {
    let fixture = SourceCheckoutFixture::new();
    let checkout = fixture.checkout(fixture.root.path(), "A");
    let initial = fixture.prepare(&checkout, fixture.root.path()).unwrap();
    let original = initial.batch.semantic_relationships[0]
        .target_element_id
        .clone()
        .unwrap();
    fixture.publish(&checkout);
    fs::write(
        fixture.root.path().join("b.rs"),
        "fn b() { let revision = 2; }\n",
    )
    .unwrap();
    let update = fixture.prepare(&checkout, fixture.root.path()).unwrap();
    assert!(
        update
            .batch
            .semantic_elements
            .iter()
            .any(|element| element.path == "a.rs")
    );
    let receipt = fixture.publish(&checkout);
    let newer = receipt
        .remaps
        .iter()
        .find(|remap| remap.incoming.record_id == original)
        .unwrap()
        .accepted
        .record_id
        .clone();
    assert_ne!(newer, original);
    let result = SemanticPersistence::execute(
        &*fixture.store.local,
        SemanticOperation::RevisionSelection {
            project_root: ROOT.into(),
            selection_id: "working".into(),
        },
        &control(),
    )
    .unwrap();
    let SemanticResult::RevisionSelection(Some(snapshot)) = result else {
        panic!("expected selection")
    };
    assert!(
        snapshot
            .connections
            .iter()
            .any(|edge| edge.relationship.label == "calls"
                && edge.relationship.target_element_id == newer)
    );
}

#[test]
fn an_unchanged_callers_retargeting_conflicts_with_its_concurrent_edit() {
    let fixture = SourceCheckoutFixture::new();
    let a = fixture.checkout(fixture.root.path(), "A");
    fixture.prepare(&a, fixture.root.path()).unwrap();
    fixture.publish(&a);
    let root_b = fixture.copy_checkout();
    let b = fixture.checkout(root_b.path(), "B");
    fixture.prepare(&b, root_b.path()).unwrap();
    fixture.publish(&b);
    fs::write(
        fixture.root.path().join("a.rs"),
        "fn a() { b(); let local = 1; }\n",
    )
    .unwrap();
    let pending = fixture.prepare(&a, fixture.root.path()).unwrap();
    fs::write(root_b.path().join("b.rs"), "fn b() { let remote = 2; }\n").unwrap();
    fixture.prepare(&b, root_b.path()).unwrap();
    fixture.publish(&b);
    let result = SemanticPersistence::execute(
        &*fixture.store.local,
        SemanticOperation::PublishRevisions(normalized_request(&pending)),
        &control(),
    )
    .unwrap();
    assert!(matches!(
        result,
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Conflict(_))
    ));
    assert_eq!(a.pending(&control()).unwrap().unwrap(), pending);
}

fn dense_synthetic_batch(
    fixture: &SourceCheckoutFixture,
    checkout: &IndexSourceCheckout,
) -> IndexBatchRequest {
    let mut batch = fixture.projected_batch(checkout, fixture.root.path());
    let file = batch
        .semantic_elements
        .iter()
        .find(|element| {
            element.path == "a.rs"
                && element
                    .metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata["source_digest_sha256"].is_string())
        })
        .unwrap()
        .clone();
    for index in 0..10_000 {
        let mut declaration = file.clone();
        declaration.semantic_element_id = format!("synthetic-declaration-{index}");
        declaration.semantic_element_name = format!("synthetic_{index}");
        declaration.semantic_element_type = "function".into();
        declaration.parent_element_id = Some(file.semantic_element_id.clone());
        declaration.metadata =
            Some(json!({"kind":"function", "index":index,"synthetic_metadata":"x".repeat(256)}));
        batch.semantic_relationships.push(edge(
            &file.semantic_element_id,
            &declaration.semantic_element_id,
            "contains",
        ));
        batch.semantic_elements.push(declaration);
    }
    batch
}

#[test]
#[ignore = "synthetic 10k producer freeze diagnostic; normal 60s invocation, no timing guarantee"]
fn dense_source_freeze_and_pending_replay_measurement() {
    let fixture = SourceCheckoutFixture::new();
    let checkout = fixture.checkout(fixture.root.path(), "dense-probe");
    let batch = dense_synthetic_batch(&fixture, &checkout);
    let mut original = batch.clone();
    let invocation = control();
    let before = checkout.before_scan(&invocation).unwrap();
    let started = std::time::Instant::now();
    let frozen = checkout
        .freeze_batch(batch, true, before, &invocation)
        .unwrap()
        .unwrap();
    let freeze_us = started.elapsed().as_micros();
    original.revision_observation = frozen.batch.revision_observation.clone();
    original.replace_paths.clear();
    original.removed_paths.clear();
    assert_eq!(
        frozen.batch, original,
        "freeze must preserve original producer records and only attach its observed scope"
    );
    let started = std::time::Instant::now();
    let recovered = checkout.pending(&invocation).unwrap().unwrap();
    let replay_us = started.elapsed().as_micros();
    assert_eq!(recovered.operation_id, frozen.operation_id);
    assert_eq!(
        recovered, frozen,
        "replay must recover the exact frozen producer input"
    );
    assert!(
        !invocation.is_expired(),
        "dense diagnostic exceeded the unchanged normal invocation deadline"
    );
    println!(
        "{}",
        json!({"workload":"dense_synthetic_source_freeze", "declarations":10_000,
        "metadata_padding_bytes_per_declaration":256,"elements":frozen.batch.semantic_elements.len(),
        "relationships":frozen.batch.semantic_relationships.len(),"inventory_paths":frozen.inventory.len(),
        "serialized_bytes":serde_json::to_vec(&frozen).unwrap().len(),"freeze_us":freeze_us,"pending_replay_us":replay_us,
        "producer_batch_equal":true,"frozen_operation_equal":true,"normal_deadline_seconds":60})
    );
}
