use super::*;

fn search_note(system: &PluginSystem) -> serde_json::Value {
    invoke_value(
        system,
        SEARCH_EXPORT_ID,
        serde_json::json!({"query": "note", "project_root": "/work/demo"}),
    )
}

#[test]
fn repeated_search_reads_only_requested_vectors_then_reuses_memory() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.put_knowledge("note", "file:note", "Note");
    let system = start_packaged_knowledge(&workspace, broker.clone());
    let first = search_note(&system);
    assert_eq!(first["mode"], "vector");
    assert_eq!(first, search_note(&system));
    assert_eq!(
        *broker.vector_reads.lock().unwrap(),
        vec![("get_row".to_owned(), "note".to_owned())],
        "search must not reload the global vector table"
    );
    system.stop(PLUGIN_ID).unwrap();
    system.start(PLUGIN_ID).unwrap();
    assert_eq!(first, search_note(&system));
    assert_eq!(*broker.vector_write_sizes.lock().unwrap(), vec![1]);
}

#[test]
fn warm_search_observes_edits_and_rechecks_artifact_access() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.put_knowledge("note", "file:note", "Note");
    let system = start_packaged_knowledge(&workspace, broker.clone());
    search_note(&system);
    broker.put_knowledge("note", "file:note", "Updated note");
    assert_eq!(search_note(&system)["results"][0]["title"], "Updated note");
    assert_eq!(*broker.vector_write_sizes.lock().unwrap(), vec![1, 1]);
    broker.deny("storage.semantic");
    let denied = system
        .invoke(
            PLUGIN_ID,
            SEARCH_EXPORT_ID,
            serde_json::json!({"query": "note", "project_root": "/work/demo"}),
        )
        .unwrap();
    assert!(matches!(denied, WireOutcome::Failed { error } if error.code == "permission_denied"));
}

#[test]
fn rebuilding_vectors_replaces_the_warm_search_cache() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.put_knowledge("note", "file:note", "Note");
    let system = start_packaged_knowledge(&workspace, broker.clone());
    search_note(&system);
    *broker.embedding_vector.lock().unwrap() = Some(vec![0.0, 1.0]);
    let rebuilt = invoke_value(
        &system,
        REBUILD_EXPORT_ID,
        serde_json::json!({"project_root": "/work/demo"}),
    );
    assert_eq!(rebuilt["job"]["vectors_rebuilt"], 1);
    let score = search_note(&system)["results"][0]["score"]
        .as_f64()
        .unwrap();
    assert!(
        (score - 1.0).abs() < 0.00001,
        "expected rebuilt vector score, got {score}"
    );
}

fn create_long_notes(system: &PluginSystem, content: &str) {
    for index in 0..8 {
        invoke_value(
            system,
            CREATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": format!("note-{index}"), "semantic_element_id": "file:note",
                "knowledge_type": "specification", "title": format!("Note {index}"),
                "content": content, "project_root": "/work/demo"
            }),
        );
    }
}

#[test]
fn long_unicode_artifacts_and_large_batches_keep_vector_search() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.ensure_element("file:note", "/work/demo");
    let system = start_packaged_knowledge(&workspace, broker.clone());
    let content = format!("start {} end", "🐼".repeat(8193));
    create_long_notes(&system, &content);
    let result = search_note(&system);
    assert_eq!(result["mode"], "vector");
    assert_eq!(result["results"].as_array().unwrap().len(), 8);
    let batches = broker.embedding_inputs.lock().unwrap();
    let embedded = batches.iter().flatten().cloned().collect::<String>();
    assert_eq!(
        embedded.matches(&content).count(),
        8,
        "long content must not be truncated"
    );
    assert!(
        batches.len() >= 3,
        "query plus at least two bounded artifact batches"
    );
}
