use super::*;
use lumvise_contracts::SemanticSourceUpsert;

#[test]
fn frozen_pages_preserve_the_original_producer_batch_losslessly() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let mut edit = fixture.publication(&journal, "large-edit", &[("a", "A2")], false);
    edit.batch.semantic_sources.push(SemanticSourceUpsert {
        semantic_source_id: "logical-source".into(),
        kind: "filesystem".into(),
        name: "Shared source".into(),
        root_path: None,
        root_uri: "source://lineage/main".into(),
    });
    for index in 0..1001 {
        let mut declaration = source("a", "A2");
        declaration.semantic_element_id = format!("declaration-{index}");
        declaration.parent_element_id = Some("a".into());
        declaration.metadata =
            Some(json!({"index":index,"exact":9_007_199_254_740_993u64,"kind":"function"}));
        edit.batch.semantic_elements.push(declaration);
    }
    let original = serde_json::to_vec(&edit).unwrap();
    journal.freeze(&edit, &control()).unwrap();
    let restored = journal.pending(&control()).unwrap().unwrap();
    assert_eq!(serde_json::to_vec(&restored).unwrap(), original);
    assert_eq!(restored.batch.semantic_relationships.len(), 3);
}

#[test]
fn a_missing_frozen_page_is_never_returned_for_publication() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let edit = fixture.publication(&journal, "missing-page", &[("a", "A2")], false);
    journal.freeze(&edit, &control()).unwrap();
    let identity = journal.header(&control()).unwrap().pending.unwrap();
    journal
        .rows
        .apply(
            vec![journal.rows.delete(&storage::pending_key(
                &identity.digest,
                "elements/0000000000000000",
            ))],
            &control(),
        )
        .unwrap();
    assert!(
        journal
            .pending(&control())
            .unwrap_err()
            .to_string()
            .contains("every frozen request page")
    );
    assert_eq!(journal.inventory(&control()).unwrap(), initial_inventory());
}

#[test]
fn changed_frozen_content_fails_the_complete_request_digest() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let edit = fixture.publication(&journal, "changed-page", &[("a", "A2")], false);
    journal.freeze(&edit, &control()).unwrap();
    let identity = journal.header(&control()).unwrap().pending.unwrap();
    let mut changed = edit.batch.semantic_elements.clone();
    changed[0].semantic_element_name = "mutated after preparation".into();
    journal
        .rows
        .apply(
            vec![
                journal
                    .rows
                    .put(
                        &storage::pending_key(&identity.digest, "elements/0000000000000000"),
                        &changed,
                    )
                    .unwrap(),
            ],
            &control(),
        )
        .unwrap();
    assert!(
        journal
            .pending(&control())
            .unwrap_err()
            .to_string()
            .contains("an intact frozen request")
    );
}

#[test]
fn wrong_source_and_missing_observation_never_claim_pending_work() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let original = fixture.publication(&journal, "bad-source", &[("a", "A2")], false);
    let mut wrong = original.clone();
    wrong.batch.semantic_elements[0].semantic_source_id = "another source".into();
    assert!(journal.freeze(&wrong, &control()).is_err());
    let mut unbound = original;
    unbound.batch.revision_observation = None;
    assert!(journal.freeze(&unbound, &control()).is_err());
    assert!(journal.pending(&control()).unwrap().is_none());
}
