use super::*;
use lumvise_plugin_runtime::PluginSystem;
use serde_json::{Value, json};

fn provider_record(provider: &str, id: &str, path: &str, revision: u64) -> Value {
    json!({"provider_instance_id":provider,"project_root":"/work/demo",
        "semantic_sources":[{"semantic_source_id":provider,"kind":"repository",
            "name":provider,"root_path":"/work/demo","root_uri":"file:///work/demo"}],
        "semantic_elements":[{"semantic_source_id":provider,"semantic_element_id":id,
            "path":path,"semantic_element_type":"file","semantic_element_name":id,
            "metadata":{"document_revision":revision}}],
        "semantic_relationships":[],"replace_paths":[path]})
}

fn source_page(index: u32) -> Value {
    let mut request = provider_record(
        "source-indexer",
        &format!("file:{index}"),
        &format!("src/{index}.rs"),
        1,
    );
    request["replace_paths"] = json!([]);
    request["ingestion_job_id"] = json!("source-refresh");
    request["ingestion_page_index"] = json!(index);
    request["ingestion_page_count"] = json!(2);
    request
}

fn published_document_revision(runtime: &PluginSystem, element_id: &str) -> Value {
    let result = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                json!({"project_root":"/work/demo","root_element_id":element_id,
            "record_kind":"elements","include_descendants":false}),
            )
            .expect("read document"),
    );
    result["elements"][0]["metadata"]["indexer_metadata"]["document_revision"].clone()
}

#[test]
fn staged_provider_refresh_preserves_a_newer_foreign_document_revision() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[97; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut document_id = String::new();
    for mut request in [
        provider_record(
            "knowledge.documents",
            "document:board",
            "@documents/board",
            1,
        ),
        source_page(0),
        provider_record(
            "knowledge.documents",
            "document:board",
            "@documents/board",
            2,
        ),
    ] {
        if request["ingestion_job_id"].is_null() {
            request = broker.observe_fixture(request);
        }
        if request["ingestion_page_index"] == 0 {
            request["revision_observation"] =
                broker.revision_observation("/work/demo", "source-indexer");
        }
        let is_document = request["provider_instance_id"] == "knowledge.documents";
        let published = success(
            runtime
                .invoke(PLUGIN_ID, INGEST_EXPORT_ID, request)
                .expect("publish"),
        );
        if is_document {
            document_id = accepted_element_id(&published, "document:board");
        }
    }
    assert_eq!(published_document_revision(&runtime, &document_id), 2);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, source_page(1))
            .expect("complete refresh"),
    );
    assert_eq!(
        published_document_revision(&runtime, &document_id),
        2,
        "a source refresh must not restore the foreign document captured on its first page"
    );
}
