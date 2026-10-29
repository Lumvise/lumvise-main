use super::*;

fn claim_request(semantic: &dyn SemanticPersistence) -> SemanticCitationRetentionAcquire {
    SemanticCitationRetentionAcquire {
        project_root: ROOT.into(),
        project_id: project_lineage_id(semantic),
        source_context: "git:main".into(),
        intent: SemanticCitationRetentionIntent {
            space_id: "assistant-space".into(),
            document_id: "decision".into(),
            publication_id: "save-decision".into(),
        },
        records: vec![SemanticRecordRef::element("a")],
        artifact_edits: Default::default(),
    }
}

fn accepted_claim(
    semantic: &dyn SemanticPersistence,
    operation: SemanticOperation,
) -> SemanticRevisionPublicationReceipt {
    match apply(semantic, operation) {
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) => receipt,
        other => panic!("expected accepted citation claim, got {other:?}"),
    }
}

fn release_request(request: &SemanticCitationRetentionAcquire) -> SemanticCitationRetentionRelease {
    SemanticCitationRetentionRelease {
        project_root: request.project_root.clone(),
        project_id: request.project_id.clone(),
        intent: request.intent.clone(),
        expected_generation: 1,
    }
}

#[test]
fn source_claim_and_release_replay_through_resource_codec_and_disk_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("citation.db");
    let (request, receipt) = {
        let local = Arc::new(LocalPersistence::open(&path).unwrap());
        let client = CentralizedPersistence::new(Arc::new(LocalPublicationServer(local)), "reader");
        publish(
            &client,
            edit(&client, "initial", vec![element("a", "a.rs", 1)]),
        );
        let request = claim_request(&client);
        let receipt = accepted_claim(
            &client,
            SemanticOperation::AcquireCitationRetention(request.clone()),
        );
        assert_eq!(receipt.selection.mode, SemanticSelectionMode::Pinned);
        assert_eq!(receipt.selection.generation, 1);
        publish(
            &client,
            edit(&client, "source-advance", vec![element("a", "a.rs", 2)]),
        );
        (request, receipt)
    };
    let local = LocalPersistence::open(&path).unwrap();
    assert_eq!(
        accepted_claim(
            &local,
            SemanticOperation::AcquireCitationRetention(request.clone())
        ),
        receipt
    );
    let exact = apply(
        &local,
        SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record: SemanticRecordRef::element("a"),
        },
    );
    let SemanticResult::RevisionResolution(Some(exact)) = exact else {
        panic!("retained original element")
    };
    assert!(
        matches!(exact.body, SemanticRevisionBody::Element(element) if element.metadata["revision"] == 1)
    );
    let release = SemanticOperation::ReleaseCitationRetention(release_request(&request));
    let released = accepted_claim(&local, release.clone());
    assert!(released.accepted_scope.records.is_empty());
    assert_eq!(released.selection.generation, 2);
    assert_eq!(accepted_claim(&local, release), released);
    assert!(
        SemanticPersistence::execute(
            &local,
            SemanticOperation::AcquireCitationRetention(request),
            &InvocationControl::sixty_seconds()
        )
        .is_err()
    );
}

#[test]
fn unknown_source_claim_release_does_not_create_revision_selections() {
    let local = LocalPersistence::in_memory().unwrap();
    let request = claim_request(&local);
    let before = apply(
        &local,
        SemanticOperation::RevisionHistory {
            project_root: ROOT.into(),
        },
    );
    assert!(
        SemanticPersistence::execute(
            &local,
            SemanticOperation::ReleaseCitationRetention(release_request(&request)),
            &InvocationControl::sixty_seconds()
        )
        .is_err()
    );
    let after = apply(
        &local,
        SemanticOperation::RevisionHistory {
            project_root: ROOT.into(),
        },
    );
    assert_eq!(
        serde_json::to_value(after).unwrap(),
        serde_json::to_value(before).unwrap()
    );
}
