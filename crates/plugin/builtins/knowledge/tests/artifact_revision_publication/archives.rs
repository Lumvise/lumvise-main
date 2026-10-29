//! Route changes preserve canonical bodies while Knowledge reads use the current owning project.
use super::*;

fn import_at_route(host: &ArtifactBoundaryHost, root: &str) -> ArtifactBoundaryHost {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("artifact.pz");
    let SemanticResult::ProjectArchive(archive) = host.execute(SemanticOperation::ProjectArchive {
        project_root: "/artifact-project".into(),
    }) else {
        panic!("expected artifact archive")
    };
    archive
        .write(&path, &InvocationControl::sixty_seconds())
        .unwrap();
    let imported =
        ArtifactBoundaryHost::with_persistence(Arc::new(LocalPersistence::in_memory().unwrap()));
    imported.execute(SemanticOperation::ImportPzSnapshot {
        project_root: root.into(),
        input_path: path.to_string_lossy().into(),
    });
    imported
}

#[test]
fn relocated_archives_preserve_bodies_and_expose_the_current_route_and_edit_token() {
    let mut host = ArtifactBoundaryHost::new();
    let mut create = create_request();
    create["metadata"] = json!({"human_text":"Keep /artifact-project as a literal example"});
    let created = host.knowledge("create_knowledge", create).unwrap()["artifact"].clone();
    let original = host.exact_artifact("spec");
    let mut relocated = import_at_route(&host, "/relocated");
    let read = relocated
        .knowledge("get_knowledge", json!({"artifact_id":"spec"}))
        .unwrap()["artifact"]
        .clone();
    assert_eq!(read["artifact_id"], created["artifact_id"]);
    assert_eq!(read["project_root"], "/relocated");
    assert_eq!(read["edit_version"], created["edit_version"]);
    for scope in [
        json!({"project_root":"/relocated"}),
        json!({"semantic_element_ids":["owner"]}),
    ] {
        let listed = relocated.knowledge("list_knowledge", scope).unwrap();
        assert_eq!(listed["artifacts"][0]["project_root"], "/relocated");
        assert_eq!(listed["artifacts"][0]["metadata"], read["metadata"]);
    }
    assert_eq!(relocated.exact_artifact("spec"), original);
    let mut edit = update_request(&read, "Edited after relocation");
    edit["project_root"] = json!("/relocated");
    let accepted = relocated.knowledge("update_knowledge", edit).unwrap()["artifact"].clone();
    assert_eq!(accepted["artifact_id"], read["artifact_id"]);
    assert_eq!(accepted["project_root"], "/relocated");
    assert_eq!(accepted["edit_version"], 2);
    assert_eq!(accepted["metadata"], read["metadata"]);
}
