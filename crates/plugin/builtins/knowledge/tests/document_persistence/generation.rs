//! Real document/generation persistence; only source-summary execution is scripted.

use super::*;
use std::collections::BTreeMap;

#[path = "generation_cooperation.rs"]
mod cooperation;

const ROOT: &str = "/project";
const TARGET: &str = "folder:src";
const FILE: &str = "file:pipeline";

pub(super) struct FakeFunctionalJobs {
    results: BTreeMap<String, Value>,
    client_instance_id: String,
    submissions: Vec<Value>,
    status_calls: usize,
    idempotency: BTreeMap<String, String>,
    lose_submit_reply: bool,
}

impl Default for FakeFunctionalJobs {
    fn default() -> Self {
        Self {
            results: BTreeMap::new(),
            client_instance_id: "client-a".into(),
            submissions: Vec::new(),
            status_calls: 0,
            idempotency: BTreeMap::new(),
            lose_submit_reply: false,
        }
    }
}

impl FakeFunctionalJobs {
    pub(super) fn invoke(&mut self, input: Value) -> Result<Value, PluginError> {
        if input["operation"] == "scope" {
            return Ok(json!({"client_instance_id": self.client_instance_id}));
        }
        if input["operation"] == "submit" {
            return self.submit(input);
        }
        self.status_calls += 1;
        self.results
            .get(input["job_id"].as_str().unwrap_or_default())
            .cloned()
            .ok_or_else(|| {
                PluginError::new(
                    "host_capability_unavailable",
                    format!(
                        "Unexpected job request `{input}`; expected a submitted source-summary job"
                    ),
                    true,
                )
            })
    }

    fn submit(&mut self, input: Value) -> Result<Value, PluginError> {
        self.submissions.push(input.clone());
        let key = input["idempotency_key"].as_str().unwrap().to_string();
        let id = match self.idempotency.get(&key) {
            Some(id) => id.clone(),
            None => self.accept_submission(&input, key),
        };
        if std::mem::take(&mut self.lose_submit_reply) {
            return Err(PluginError::new(
                "host_capability_unavailable",
                "submit reply lost",
                true,
            ));
        }
        Ok(json!({"job_id":id}))
    }

    fn accept_submission(&mut self, input: &Value, key: String) -> String {
        let source = &input["input"];
        let id = format!("source-summary-{}", self.results.len());
        self.results.insert(id.clone(), json!({"status":"succeeded","output":{
            "semantic_element_id":source["semantic_element_id"],"content_fingerprint":source["content_fingerprint"],
            "job":format!("Parses pipeline input for {} at {}.",self.client_instance_id,
                source["content_fingerprint"].as_str().unwrap()),
            "source_interface":"fn parse(input: &str)","receives":["Source input"],
            "outcome":"Structured knowledge","effects":["Updates project context"]}}));
        self.idempotency.insert(key, id.clone());
        id
    }
}

fn seeded() -> DocumentBoundaryHost {
    let mut host = DocumentBoundaryHost::new();
    host.generation = Some(FakeFunctionalJobs::default());
    sources(&host, "fp1:0000000000000001:original");
    host
}

fn sources(host: &DocumentBoundaryHost, fingerprint: &str) {
    source_publication::publish(host, source_batch(fingerprint));
}

fn source_batch(fingerprint: &str) -> Value {
    json!({"provider_instance_id":"filesystem","project_root":ROOT,
        "replace_paths":["src","src/pipeline.rs"],"removed_paths":[],
        "semantic_sources":[{"semantic_source_id":"source:generation","kind":"repository","name":"Source",
            "root_path":ROOT,"root_uri":"file:///project"}],
        "semantic_elements":[
            {"semantic_element_id":TARGET,"semantic_source_id":"source:generation","path":"src",
                "semantic_element_type":"folder","semantic_element_name":"src","parent_element_id":null,
                "start_line":null,"end_line":null,"content_fingerprint":fingerprint,"metadata":{}},
            {"semantic_element_id":FILE,"semantic_source_id":"source:generation","path":"src/pipeline.rs",
                "semantic_element_type":"file","semantic_element_name":"pipeline.rs","parent_element_id":TARGET,
                "start_line":1,"end_line":8,"content_fingerprint":fingerprint,
                "metadata":{"signature":"pub fn parse(input: &str) -> Value { serde_json::json!(input) }"}}],
        "semantic_relationships":[],"semantic_artifacts":[]})
}

fn ensure(host: &mut DocumentBoundaryHost, refresh: bool) -> Value {
    host.knowledge(
        lumvise_plugin_knowledge::ENSURE_C4_EXPORT_ID,
        json!({"project_root":ROOT,"target_id":TARGET,"refresh":refresh}),
    )
    .unwrap()
}

fn poll(host: &mut DocumentBoundaryHost) -> Value {
    host.knowledge(
        lumvise_plugin_knowledge::ARTIFACT_GENERATION_POLL_EXPORT_ID,
        json!({}),
    )
    .unwrap()
}

fn notify(host: &mut DocumentBoundaryHost, id: &str) {
    let SemanticResult::RevisionFamilies(remaps) = host.execute(
        SemanticOperation::RevisionFamilies(lumvise_db_core::SemanticRevisionFamilyQuery {
            project_root: ROOT.into(),
            selection_id: "working".into(),
            expected_commit_version: None,
            records: vec![lumvise_db_core::SemanticRecordRef::element(id)],
        }),
    ) else {
        panic!("expected the accepted source event identity");
    };
    let selected = remaps
        .first()
        .map_or(id, |remap| remap.accepted.record_id.as_str());
    host.knowledge(
        lumvise_plugin_knowledge::ELEMENT_TRIGGER_EXPORT_ID,
        json!({"project_root":ROOT,"base_revision":1,"target_revision":2,"changed":[
            {"entity_id":selected,"entity_kind":"file","disposition":"upserted"}]}),
    )
    .unwrap();
}

fn generated_report(host: &mut DocumentBoundaryHost) -> Value {
    assert_eq!(ensure(host, false)["status"], "pending");
    assert_eq!(poll(host)["completed"], 1);
    let ready = ensure(host, false);
    assert_eq!(ready["status"], "ready", "{ready}");
    ready["document"].clone()
}

fn read(host: &mut DocumentBoundaryHost, id: &Value) -> Value {
    host.knowledge(
        "get_document",
        json!({"project_root":ROOT,"document_id":id}),
    )
    .unwrap()["document"]
        .clone()
}

fn context(host: &mut DocumentBoundaryHost) -> Value {
    host.knowledge(
        "list_documents",
        json!({"project_root":ROOT,"document_kind":"context"}),
    )
    .unwrap()["documents"][0]
        .clone()
}

fn edit(host: &mut DocumentBoundaryHost, document: &Value, content: &str) -> Value {
    host.knowledge(
        "update_document",
        json!({"project_root":ROOT,"document_id":document["document_id"],
        "expected_revision":document["revision"], "expected_semantic_element_id":document["semantic_element_id"],"content":content}),
    )
    .unwrap()["document"]
        .clone()
}

#[test]
fn generated_context_and_c4_reopen_as_documents_and_project_without_artifact_aliases() {
    let mut host = seeded();
    let report = generated_report(&mut host);
    let context = context(&mut host);
    assert_eq!(report["document_kind"], "report");
    assert_eq!(context["document_kind"], "context");
    assert_ne!(context["semantic_element_id"], FILE);
    assert_eq!(
        context["metadata"]["provenance"]["semantic_element_id"],
        FILE
    );
    assert_eq!(
        host.knowledge("list_knowledge", json!({"project_root":ROOT}))
            .unwrap()["artifacts"],
        json!([])
    );
    assert!(
        report["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["target_kind"] == "document"
                && reference["document_id"] == context["document_id"])
    );
    let projection = host
        .knowledge(
            lumvise_plugin_knowledge::PROJECTION_EXPORT_ID,
            json!({"projectRoot":ROOT,"changesSince":0}),
        )
        .unwrap();
    let pages = projection["elements"].as_array().unwrap();
    assert!(
        pages
            .iter()
            .any(|page| page["kind"] == "report" && page["elementId"] == report["document_id"])
    );
    let source = pages.iter().find(|page| page["elementId"] == FILE).unwrap();
    assert!(
        source["markdown"]
            .as_str()
            .unwrap()
            .contains(context["content"].as_str().unwrap())
    );
    assert_eq!(source["artifacts"], json!([]));
    let mut reopened = DocumentBoundaryHost::with_persistence(host.persistence.clone());
    assert_eq!(read(&mut reopened, &report["document_id"]), report);
    assert_eq!(read(&mut reopened, &context["document_id"]), context);
}

#[test]
fn source_refresh_updates_generated_bodies_and_retains_user_grouping_and_metadata() {
    let mut host = seeded();
    let report = generated_report(&mut host);
    let mut metadata = report["metadata"].clone();
    metadata["reviewer"] = json!("human");
    let grouped = host
        .knowledge(
            "update_document",
            json!({"project_root":ROOT,"document_id":report["document_id"], "expected_revision":report["revision"], "expected_semantic_element_id":report["semantic_element_id"],
        "path":"Architecture/Pipeline","metadata":metadata}),
        )
        .unwrap()["document"]
        .clone();
    sources(&host, "fp1:0000000000000002:updated");
    notify(&mut host, FILE);
    poll(&mut host);
    let updated = read(&mut host, &report["document_id"]);
    assert!(updated["revision"].as_u64().unwrap() > grouped["revision"].as_u64().unwrap());
    assert!(
        updated["content"]
            .as_str()
            .unwrap()
            .contains("fp1:0000000000000002:updated")
    );
    assert_eq!(updated["path"], "Architecture/Pipeline");
    assert_eq!(updated["metadata"]["reviewer"], "human");
    assert!(
        context(&mut host)["content"]
            .as_str()
            .unwrap()
            .contains("fp1:0000000000000002:updated")
    );
}

#[test]
fn source_revisions_keep_one_refresh_intent_for_the_established_report() {
    let mut host = seeded();
    let report = generated_report(&mut host);
    for revision in 2..=3 {
        sources(&host, &format!("fp1:000000000000000{revision}:changed"));
        notify(&mut host, FILE);
        poll(&mut host);
        let ready = ensure(&mut host, false);
        assert_eq!(ready["status"], "ready", "{ready}");
        assert_eq!(ready["document"]["document_id"], report["document_id"]);
    }
    let intents = host
        .host_call(
            "storage.plugin",
            json!({"operation":"list_rows",
        "table_name":"knowledge_c4_report_intents", "limit":100}),
        )
        .unwrap();
    assert_eq!(intents["rows"].as_array().unwrap().len(), 1, "{intents}");
    assert_eq!(
        intents["rows"][0]["value"]["document_id"],
        report["document_id"]
    );
}

#[test]
fn held_report_refresh_is_visible_after_its_source_revision_advances() {
    let mut host = seeded();
    let report = generated_report(&mut host);
    let authored = edit(
        &mut host,
        &report,
        "Reviewed report; preserve this explanation.",
    );
    sources(&host, "fp1:0000000000000002:changed");
    notify(&mut host, FILE);
    let debug = host
        .knowledge(
            lumvise_plugin_knowledge::DEBUG_C4_EXPORT_ID,
            json!({"project_root":ROOT, "target_id":TARGET}),
        )
        .unwrap();
    assert_eq!(
        debug["report"]["failure"]["reason_code"],
        "generated_document_authored"
    );
    assert_eq!(debug["report"]["document"], authored);
}

#[test]
fn authored_report_is_held_during_refresh_and_survives_source_removal() {
    let mut host = seeded();
    let report = generated_report(&mut host);
    let authored = edit(
        &mut host,
        &report,
        "# Reviewed architecture\n\nKeep this explanation.",
    );
    sources(&host, "fp1:0000000000000002:updated");
    notify(&mut host, FILE);
    poll(&mut host);
    let held = ensure(&mut host, false);
    assert_eq!(held["status"], "held");
    assert_eq!(held["reason_code"], "generated_document_authored");
    assert_eq!(held["document"], authored);
    let debug = host
        .knowledge(
            lumvise_plugin_knowledge::DEBUG_C4_EXPORT_ID,
            json!({"project_root":ROOT,"target_id":TARGET}),
        )
        .unwrap();
    assert_eq!(
        debug["report"]["failure"]["reason_code"],
        "generated_document_authored"
    );
    source_publication::publish(
        &host,
        json!({"provider_instance_id":"filesystem","project_root":ROOT,
        "replace_paths":[],"removed_paths":["src","src/pipeline.rs"],"semantic_sources":[],"semantic_elements":[],
        "semantic_relationships":[],"semantic_artifacts":[]}),
    );
    notify(&mut host, TARGET);
    assert_eq!(read(&mut host, &report["document_id"]), authored);
    assert_eq!(
        host.knowledge(
            "list_documents",
            json!({"project_root":ROOT,"document_kind":"report"})
        )
        .unwrap()["total"],
        1
    );
}

#[test]
fn deleted_generated_report_is_not_recreated_by_source_events_or_explicit_refresh() {
    let mut host = seeded();
    let report = generated_report(&mut host);
    host.knowledge(
        "delete_document",
        json!({"project_root":ROOT,"document_id":report["document_id"], "expected_revision":report["revision"], "expected_semantic_element_id":report["semantic_element_id"]}),
    )
    .unwrap();
    sources(&host, "fp1:0000000000000002:updated");
    notify(&mut host, FILE);
    let held = ensure(&mut host, true);
    assert_eq!(held["status"], "held");
    assert_eq!(held["reason_code"], "generated_document_deleted");
    assert_eq!(held["document"], Value::Null);
    poll(&mut host);
    assert_eq!(
        host.knowledge(
            "list_documents",
            json!({"project_root":ROOT,"document_kind":"report"})
        )
        .unwrap()["total"],
        0
    );
    let mut later = source_batch("fp1:0000000000000003:later-update");
    later["semantic_elements"][0]["content_fingerprint"] = json!("fp1:0000000000000002:updated");
    source_publication::publish(&host, later);
    notify(&mut host, FILE);
    poll(&mut host);
    assert_eq!(
        host.knowledge(
            "list_documents",
            json!({"project_root":ROOT,"document_kind":"report"})
        )
        .unwrap()["total"],
        0,
        "a held refresh intent must preserve the deleted logical identity through later source revisions"
    );
}

#[test]
fn replaying_a_deleted_report_receipt_preserves_the_recreated_reports_refresh_intent() {
    let mut host = seeded();
    let report = generated_report(&mut host);
    let deletion = super::revisions::deletion(&report);
    host.knowledge("delete_document", deletion.clone()).unwrap();
    let recreated = host
        .knowledge(
            "create_document",
            json!({"operation_id":"recreate-report",
        "project_root":ROOT, "document_id":report["document_id"], "document_kind":"report",
        "title":report["title"], "content":report["content"], "tags":report["tags"],
        "references":report["references"], "metadata":report["metadata"]}),
        )
        .unwrap()["document"]
        .clone();
    assert_eq!(ensure(&mut host, false)["document"], recreated);
    let query =
        json!({"operation":"list_rows", "table_name":"knowledge_c4_report_intents", "limit":100});
    let before = host.host_call("storage.plugin", query.clone()).unwrap();
    assert_eq!(before["rows"].as_array().unwrap().len(), 1);
    host.knowledge("delete_document", deletion).unwrap();
    assert_eq!(
        host.host_call("storage.plugin", query).unwrap(),
        before,
        "replaying an old deletion must not cancel a later document's refresh scheduling"
    );
    assert_eq!(read(&mut host, &report["document_id"]), recreated);
}

#[test]
fn authored_source_context_moves_finished_generation_to_durable_review_failure() {
    let mut host = seeded();
    generated_report(&mut host);
    let context = context(&mut host);
    let authored = edit(&mut host, &context, "Human-owned functional explanation.");
    sources(&host, "fp1:0000000000000002:updated");
    notify(&mut host, FILE);
    let finished = poll(&mut host);
    assert_eq!(finished["pending"], 0);
    let result = ensure(&mut host, false);
    assert_eq!(result["status"], "not_ready", "{result}");
    assert_eq!(result["reason_code"], "generated_document_authored");
    assert_eq!(read(&mut host, &context["document_id"]), authored);
    let mut reopened = DocumentBoundaryHost::with_persistence(host.persistence.clone());
    let result = ensure(&mut reopened, false);
    assert_eq!(result["reason_code"], "generated_document_authored");
}

fn authored(host: &mut DocumentBoundaryHost, id: &str, source: &str, kind: &str, tag: &str) {
    host.knowledge(
        "create_document",
        json!({"project_root":ROOT,"document_id":id,
        "document_kind":kind,"title":id,"content":format!("Preserved original {id}"),
        "tags":[tag],"path":"Original grouping","metadata":{"nucleus":{"target_element_id":source}},
        "references":[{"target_kind":"semantic_element","semantic_element_id":source}]}),
    )
    .unwrap();
}

#[test]
fn authored_source_documents_keep_their_noncanonical_identities_without_generator_adoption() {
    let mut host = DocumentBoundaryHost::new();
    host.generation = Some(FakeFunctionalJobs::default());
    host.seed_sources(source_batch("fp1:0000000000000001:original"))
        .unwrap();
    authored(&mut host, "original-report", TARGET, "report", "scoped-c4");
    authored(
        &mut host,
        "original-context",
        FILE,
        "context",
        "cultivation-functional",
    );
    let report = read(&mut host, &json!("original-report"));
    let context = read(&mut host, &json!("original-context"));
    let held = ensure(&mut host, false);
    assert_eq!(held["reason_code"], "generated_document_authored");
    assert_eq!(held["document_id"], "original-report");
    poll(&mut host);
    assert_eq!(read(&mut host, &json!("original-report")), report);
    assert_eq!(read(&mut host, &json!("original-context")), context);
    assert_eq!(
        host.knowledge("list_documents", json!({"project_root":ROOT}))
            .unwrap()["total"],
        2
    );
    assert_eq!(
        host.knowledge("list_knowledge", json!({"project_root":ROOT}))
            .unwrap()["artifacts"],
        json!([])
    );
}

#[test]
fn direct_cultivation_builds_document_dependencies_and_reuses_without_rewriting() {
    let mut host = seeded();
    let request = json!({"project_root":ROOT,"mode":"cultivate_from_scratch","target_id":TARGET});
    let first = host
        .knowledge(
            lumvise_plugin_knowledge::RUN_CULTIVATION_EXPORT_ID,
            request.clone(),
        )
        .unwrap();
    assert_eq!(first["run"]["document_ids"].as_array().unwrap().len(), 2);
    assert!(first["run"].get("artifact_ids").is_none());
    let documents = host
        .knowledge("list_documents", json!({"project_root":ROOT}))
        .unwrap()["documents"]
        .clone();
    assert!(
        documents
            .as_array()
            .unwrap()
            .iter()
            .all(|document| document["document_kind"] == "context")
    );
    assert!(documents.as_array().unwrap().iter().any(|document| {
        document["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["target_kind"] == "document")
    }));
    host.knowledge(lumvise_plugin_knowledge::RUN_CULTIVATION_EXPORT_ID, request)
        .unwrap();
    assert_eq!(
        host.knowledge("list_documents", json!({"project_root":ROOT}))
            .unwrap()["documents"],
        documents
    );
    let leaf = documents
        .as_array()
        .unwrap()
        .iter()
        .find(|document| document["metadata"]["provenance"]["semantic_element_id"] == FILE)
        .unwrap();
    let authored = edit(&mut host, leaf, "Retain this authored source context.");
    let failed = host
        .knowledge(
            lumvise_plugin_knowledge::RUN_CULTIVATION_EXPORT_ID,
            json!({"project_root":ROOT,"mode":"refresh_cultivation","target_id":TARGET}),
        )
        .unwrap_err();
    assert_eq!(failed.code, "generated_document_authored");
    assert_eq!(read(&mut host, &leaf["document_id"]), authored);
}
