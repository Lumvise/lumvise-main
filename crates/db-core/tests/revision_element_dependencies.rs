//! Exact payload requirements survive native, resource and archive boundaries.
mod common;
use common::{LocalPublicationServer, RevisionTestClient};
use lumvise_db_core::*;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::sync::Arc;

const ROOT: &str = "/element-payloads";

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "documents".into(),
        path: format!("{id}.md"),
        element_kind: "file".into(),
        name: id.into(),
        parent_element_id: None,
        revision_dependencies: Vec::new(),
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn execute(store: &dyn SemanticPersistence, operation: SemanticOperation) -> SemanticResult {
    store
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn publish(store: &dyn SemanticPersistence, request: SemanticRevisionPublication) {
    let result = execute(store, SemanticOperation::PublishRevisions(request));
    assert!(
        matches!(
            result,
            SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(_))
        ),
        "{result:?}"
    );
}

fn snapshot(store: &dyn SemanticPersistence) -> SemanticRevisionSelectionSnapshot {
    let result = execute(
        store,
        SemanticOperation::RevisionSelection {
            project_root: ROOT.into(),
            selection_id: "working".into(),
        },
    );
    let SemanticResult::RevisionSelection(Some(selected)) = result else {
        panic!("{result:?}; expected the working selection");
    };
    selected
}

fn publish_document(store: &dyn SemanticPersistence) -> SemanticElement {
    let mut locator = element("note");
    locator.revision_dependencies = vec![SemanticRecordRef::artifact("payload")];
    let mut request = RevisionTestClient::new(store, ROOT, "local")
        .publication("initial", vec![element("root"), locator.clone()]);
    request.artifacts = vec![SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: SemanticArtifact {
            artifact_id: "payload".into(),
            semantic_element_id: "root".into(),
            artifact_kind: "document_content".into(),
            title: "Note".into(),
            content: Some("exact original contents".into()),
            content_ref: None,
            searchable_text: None,
            content_size_bytes: None,
            dependencies: vec![],
            metadata: json!({}),
        },
        media_type: "text/plain".into(),
    }];
    publish(store, request);
    locator
}

fn detach_and_update(store: &dyn SemanticPersistence, mut locator: SemanticElement) {
    let client = RevisionTestClient::new(store, ROOT, "local");
    let mut detach = client.publication("detach-payload", vec![]);
    detach.structure = None;
    detach.removed_artifact_ids = vec!["payload".into()];
    publish(store, detach);
    locator.metadata = json!({"title":"Renamed note"});
    let mut update = client.publication("update-locator", vec![locator]);
    update.structure.as_mut().unwrap().partition = Some(SemanticPartition {
        project_root: ROOT.into(),
        provider_instance_id: None,
        replace_paths: vec!["note.md".into()],
    });
    publish(store, update);
    let selected = snapshot(store);
    assert!(selected.artifacts.is_empty());
    assert!(
        selected
            .required_records
            .iter()
            .any(|stored| stored.record == SemanticRecordRef::artifact("payload"))
    );
    assert_eq!(
        selected
            .elements
            .iter()
            .find(|e| e.path == "note.md")
            .unwrap()
            .revision_dependencies,
        [SemanticRecordRef::artifact("payload")]
    );
}

#[test]
fn exact_element_payloads_survive_scoped_publication_reopen_resource_and_pz() {
    for remote in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let storage_path = directory.path().join("storage");
        let original = {
            let local = Arc::new(LocalPersistence::open(&storage_path).unwrap());
            let store: Arc<dyn SemanticPersistence> = if remote {
                Arc::new(CentralizedPersistence::new(
                    Arc::new(LocalPublicationServer(local)),
                    "checkout",
                ))
            } else {
                local
            };
            let locator = publish_document(&*store);
            detach_and_update(&*store, locator);
            let SemanticResult::ProjectArchive(archive) = execute(
                &*store,
                SemanticOperation::ProjectArchive {
                    project_root: ROOT.into(),
                },
            ) else {
                panic!("expected the complete selected archive");
            };
            let path = directory.path().join("payloads.pz");
            archive
                .write(&path, &InvocationControl::sixty_seconds())
                .unwrap();
            let restored =
                SemanticArchive::read(&path, ROOT, &InvocationControl::sixty_seconds()).unwrap();
            assert_eq!(restored.revision_state, archive.revision_state);
            snapshot(&*store)
        };
        let reopened = LocalPersistence::open(&storage_path).unwrap();
        assert_eq!(snapshot(&reopened), original);
    }
}
