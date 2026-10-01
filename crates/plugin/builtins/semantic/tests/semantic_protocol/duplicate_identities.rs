use super::*;

#[test]
fn duplicate_semantic_ids_fail_before_ingest_can_overwrite_records() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[77; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    for staged in [false, true] {
        let batch = duplicated_batch(staged);
        let error = failure(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch).unwrap());
        assert_eq!(error.code, "invalid_semantic_index_batch");
        assert!(error.message.contains("file:parser"));
        assert_eq!(broker.semantic_counts("/work/demo").0, 0);
    }
    runtime.stop(PLUGIN_ID).unwrap();
}

fn duplicated_batch(staged: bool) -> serde_json::Value {
    let mut batch = index_batch();
    let duplicate = batch["semantic_elements"][0].clone();
    batch["semantic_elements"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    if staged {
        batch["ingestion_job_id"] = serde_json::json!("duplicates");
        batch["ingestion_page_index"] = serde_json::json!(0);
        batch["ingestion_page_count"] = serde_json::json!(1);
    }
    batch
}
