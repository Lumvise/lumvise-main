use lumvise_db_core::{ContentFingerprintParts, SemanticElement, SemanticStructureReconciliation};

#[test]
fn portable_hook_records_keep_generation_in_delivery_identity() {
    use lumvise_db_core::{
        ChangeBatch, ChangeDisposition, ChangeHookRegistration, ChangeHookScope, ChangedElement,
    };
    let registration = ChangeHookRegistration::from_persisted(
        "hook".into(),
        ChangeHookScope {
            project_root: "/repo".into(),
            ..Default::default()
        },
        3,
        2,
    );
    assert_eq!(registration.generation(), 2);
    let changed = ChangedElement {
        element_id: "e".into(),
        entity_kind: "file".into(),
        revision: 4,
        disposition: ChangeDisposition::Upserted,
    };
    let batch = ChangeBatch::for_hook(
        registration.hook_name.clone(),
        registration.generation(),
        registration.watermark,
        4,
        vec![changed.clone()],
    );
    assert_eq!(batch.hook_name(), Some("hook"));
    assert_eq!(batch.registration_generation(), 2);
    assert_eq!(batch.delivery_key(&changed).unwrap(), "hook:2:e:4:Upserted");
    let round_trip: ChangeBatch =
        serde_json::from_slice(&serde_json::to_vec(&batch).unwrap()).unwrap();
    assert_eq!(round_trip, batch);
}

#[test]
fn portable_fingerprint_record_preserves_matching_rules() {
    let left = ContentFingerprintParts::parse("fp1:0000000000000001:a").unwrap();
    let right = ContentFingerprintParts::parse("fp1:0000000000000003:a").unwrap();
    assert!(left.matches_exact(&right));
    assert_eq!(left.hamming_distance(&right), Some(1));
    assert!(ContentFingerprintParts::parse("unversioned").is_none());
    assert!(!left.matches_exact(&ContentFingerprintParts {
        simhash: None,
        exact_hash: "b".into()
    }));
    assert_eq!(
        left.hamming_distance(&ContentFingerprintParts {
            simhash: None,
            exact_hash: "a".into()
        }),
        None
    );
}

#[test]
fn portable_reconciliation_reserves_incoming_identity_owners() {
    let existing = element("old");
    let incoming = vec![element("new"), element("old")];
    let reconciled = SemanticStructureReconciliation::between(&[existing.clone()], &incoming);
    assert!(reconciled.remaps.is_empty());
    assert_eq!(reconciled.active_elements.len(), 2);
    let moved = SemanticStructureReconciliation::between(&[existing], &[element("new")]);
    assert_eq!(moved.active_elements[0].semantic_element_id, "old");
    assert_eq!(moved.remaps[0].incoming_id, "new");
    assert_eq!(moved.remaps[0].resolved_id, "old");
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: "file.rs".into(),
        element_kind: "file".into(),
        name: "file.rs".into(),
        parent_element_id: None,
        content_fingerprint: Some("fp1:0000000000000001:a".into()),
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: serde_json::json!({}),
    }
}
