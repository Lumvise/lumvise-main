use super::*;

const PATH: &str = "knowledge/decisions/auth.md";

fn source(host: &DocumentBoundaryHost, root: &str, id: &str) {
    host.semantic("ingest_index_batch",json!({"provider_instance_id":"filesystem","project_root":root,"replace_paths":[],"removed_paths":[],
        "semantic_sources":[{"semantic_source_id":format!("source:{id}"),"kind":"repository","name":"Sources","root_path":root,"root_uri":format!("file://{root}")}],
        "semantic_elements":[{"semantic_element_id":id,"semantic_source_id":format!("source:{id}"),"path":PATH,
            "semantic_element_type":"file","semantic_element_name":"auth.md","parent_element_id":null,"start_line":1,"end_line":1,"content_fingerprint":null,"metadata":{}}],
        "semantic_relationships":[],"semantic_artifacts":[]})).unwrap();
}

fn notify(host: &mut DocumentBoundaryHost, root: &str, id: &str) -> Result<Value, PluginError> {
    host.knowledge(
        lumvise_plugin_knowledge::ELEMENT_TRIGGER_EXPORT_ID,
        json!({"project_root":root,"base_revision":1,"target_revision":2,
        "changed":[{"entity_id":id,"entity_kind":"file","disposition":"upserted"}]}),
    )
}

fn list(host: &mut DocumentBoundaryHost, root: &str) -> Value {
    host.knowledge("list_documents", json!({"project_root":root}))
        .unwrap()["documents"]
        .clone()
}

#[test]
fn markdown_sources_seed_one_canonical_context_document_and_never_a_product_artifact() {
    let mut host = DocumentBoundaryHost::new();
    source(&host, "/project", "file:auth");
    notify(&mut host, "/project", "file:auth").unwrap();
    let documents = list(&mut host, "/project");
    assert_eq!(documents.as_array().unwrap().len(), 1);
    let document = &documents[0];
    assert_eq!(document["document_kind"], "context");
    assert_eq!(document["path"], PATH);
    assert_eq!(
        document["references"],
        json!([{"target_kind":"semantic_element","semantic_element_id":"file:auth"}])
    );
    assert_eq!(
        document["metadata"]["generation"]["owner"],
        "knowledge.markdown_seed"
    );
    notify(&mut host, "/project", "file:auth").unwrap();
    assert_eq!(list(&mut host, "/project"), documents);
    assert_eq!(
        host.knowledge("list_knowledge", json!({"project_root":"/project"}))
            .unwrap()["artifacts"],
        json!([])
    );
}

#[test]
fn source_refresh_and_removal_cannot_delete_or_replace_authored_database_markdown() {
    let mut host = DocumentBoundaryHost::new();
    source(&host, "/project", "file:auth");
    notify(&mut host, "/project", "file:auth").unwrap();
    let document = list(&mut host, "/project")[0].clone();
    let authored = host.knowledge("update_document",json!({"project_root":"/project","document_id":document["document_id"],
        "expected_revision":document["revision"],"content":"Database-authored decisions", "title":"My decisions","path":"My group"})).unwrap()["document"].clone();
    notify(&mut host, "/project", "file:auth").unwrap();
    assert_eq!(list(&mut host, "/project")[0], authored);
    host.semantic("ingest_index_batch",json!({"provider_instance_id":"filesystem","project_root":"/project","replace_paths":[],"removed_paths":[PATH],
        "semantic_sources":[],"semantic_elements":[],"semantic_relationships":[],"semantic_artifacts":[]})).unwrap();
    notify(&mut host, "/project", "file:auth").unwrap();
    assert_eq!(list(&mut host, "/project")[0], authored);
}

#[test]
fn identical_markdown_paths_in_two_projects_keep_distinct_context_identities() {
    let mut host = DocumentBoundaryHost::new();
    source(&host, "/project", "file:one");
    source(&host, "/other", "file:two");
    notify(&mut host, "/project", "file:one").unwrap();
    notify(&mut host, "/other", "file:two").unwrap();
    let first = list(&mut host, "/project")[0].clone();
    let second = list(&mut host, "/other")[0].clone();
    assert_ne!(first["document_id"], second["document_id"]);
    assert_eq!(first["project_root"], "/project");
    assert_eq!(second["project_root"], "/other");
}

#[test]
fn an_explicitly_converted_identity_stays_canonical_and_deletion_is_not_undone_by_source_events() {
    let mut host = DocumentBoundaryHost::new();
    source(&host, "/project", "file:auth");
    host.execute(SemanticOperation::UpsertArtifact { artifact: serde_json::from_value(json!({
        "artifact_id":"original-md-identity","semantic_element_id":"file:auth","artifact_kind":"definition",
        "title":"Original authored note","content":"Keep the original body","dependencies":[],
        "metadata":{"knowledge":{"project_root":"/project","tags":["nucleus","md-nucleus"],
        "metadata":{"nucleus":{"target_path":PATH,"target_element_id":"file:auth"}}}}})).unwrap(),media_type:"text/markdown".into() });
    assert_eq!(
        notify(&mut host, "/project", "file:auth").unwrap_err().code,
        "knowledge_conversion_required"
    );
    assert_eq!(list(&mut host, "/project"), json!([]));
    host.knowledge(
        "convert_legacy_documents",
        json!({"project_root":"/project","preview":false}),
    )
    .unwrap();
    notify(&mut host, "/project", "file:auth").unwrap();
    assert_eq!(list(&mut host, "/project").as_array().unwrap().len(), 1);
    let converted = list(&mut host, "/project")[0].clone();
    assert_eq!(converted["document_id"], "original-md-identity");
    assert_eq!(converted["content"], "Keep the original body");
    host.knowledge(
        "delete_document",
        json!({"project_root":"/project","document_id":"original-md-identity"}),
    )
    .unwrap();
    notify(&mut host, "/project", "file:auth").unwrap();
    assert_eq!(list(&mut host, "/project"), json!([]));
}

#[test]
fn only_markdown_file_elements_below_the_knowledge_root_seed_documents() {
    let mut host = DocumentBoundaryHost::new();
    let elements = [("a-heading",PATH,"section",Some("z-file")),("z-file",PATH,"file",None),
        ("folder","knowledge/folder.md","folder",None),("outside","docs/readme.md","file",None),("code","knowledge/code.rs","file",None)]
        .into_iter().map(|(id,path,kind,parent)|json!({"semantic_element_id":id,"semantic_source_id":"source:mixed","path":path,
            "semantic_element_type":kind,"semantic_element_name":id,"parent_element_id":parent,"start_line":null,"end_line":null,"content_fingerprint":null,"metadata":{}})).collect::<Vec<_>>();
    host.semantic("ingest_index_batch",json!({"provider_instance_id":"filesystem","project_root":"/project","replace_paths":[],"removed_paths":[],
        "semantic_sources":[{"semantic_source_id":"source:mixed","kind":"repository","name":"Sources","root_path":"/project","root_uri":"file:///project"}],
        "semantic_elements":elements,"semantic_relationships":[],"semantic_artifacts":[]})).unwrap();
    let changed = elements.iter().map(|element|json!({"entity_id":element["semantic_element_id"],"entity_kind":element["semantic_element_type"],"disposition":"upserted"})).collect::<Vec<_>>();
    host.knowledge(
        lumvise_plugin_knowledge::ELEMENT_TRIGGER_EXPORT_ID,
        json!({"project_root":"/project","base_revision":1,"target_revision":2,"changed":changed}),
    )
    .unwrap();
    let documents = list(&mut host, "/project");
    assert_eq!(documents.as_array().unwrap().len(), 1);
    assert_eq!(
        documents[0]["references"],
        json!([{"target_kind":"semantic_element","semantic_element_id":"z-file"}])
    );
}
