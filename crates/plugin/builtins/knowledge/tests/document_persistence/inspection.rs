//! The native adapter keeps the existing document owner, exact editing bases and durable space.
use super::*;

fn inspect(
    host: &mut DocumentBoundaryHost,
    export: &str,
    input: Value,
) -> Result<Value, PluginError> {
    host.knowledge(
        "inspect_document_space",
        json!({"selection":{"space_kind":"assistant"},"export_id":export,"input":input}),
    )
}

#[test]
fn native_memory_inspection_persists_across_restart_and_preserves_edit_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inspection.db");
    let (space, original) = {
        let mut host = DocumentBoundaryHost::with_persistence(Arc::new(
            LocalPersistence::open(&path).unwrap(),
        ));
        host.invocation_context = host.invocation_context.clone().with_human_interaction();
        let space = inspect(&mut host, "open", json!({})).unwrap()["space"].clone();
        let original = inspect(&mut host, "create_document", json!({"document_id":"preferences", "document_kind":"context", "title":"Preferences", "content":"Keep explanations short"})).unwrap()["output"]["document"].clone();
        (space, original)
    };
    let mut host =
        DocumentBoundaryHost::with_persistence(Arc::new(LocalPersistence::open(&path).unwrap()));
    host.invocation_context = host.invocation_context.clone().with_human_interaction();
    assert_eq!(
        inspect(&mut host, "open", json!({})).unwrap()["space"],
        space
    );
    let listed = inspect(&mut host, "list_documents", json!({})).unwrap();
    assert_eq!(
        listed["output"]["documents"][0]["content"],
        "Keep explanations short"
    );
    let edit = json!({"project_root":space["project_root"],"document_id":"preferences", "expected_revision":original["revision"],
        "expected_semantic_element_id":original["semantic_element_id"],"content":"Human edit"});
    inspect(&mut host, "update_document", edit.clone()).unwrap();
    let mut stale = edit;
    stale["content"] = json!("Stale edit");
    assert!(inspect(&mut host, "update_document", stale).is_err());
    let latest = inspect(
        &mut host,
        "get_document",
        json!({"document_id":"preferences"}),
    )
    .unwrap();
    assert_eq!(latest["output"]["document"]["content"], "Human edit");
}

#[test]
fn automated_or_cross_space_inspection_never_reaches_document_writes() {
    let mut host = DocumentBoundaryHost::new();
    assert!(inspect(&mut host, "open", json!({})).is_err());
    assert!(host.writes.is_empty());
    host.invocation_context = host.invocation_context.clone().with_human_interaction();
    assert!(
        inspect(
            &mut host,
            "create_document",
            json!({"project_root":"agent-space://other", "document_id":"steal",
        "document_kind":"context","title":"No", "content":"No"})
        )
        .is_err()
    );
    assert!(host.writes.is_empty());
    assert!(inspect(&mut host, "create_knowledge", json!({})).is_err());
    assert!(host.writes.is_empty());
}
