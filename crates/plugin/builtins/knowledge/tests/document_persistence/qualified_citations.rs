//! Qualified references and retention run through Knowledge's dispatch and the real host/persistence.
use super::*;
use lumvise_db_core::{PersistenceResult, SemanticReadiness};
use lumvise_semantic_revisions::{
    SemanticRecordRef, SemanticRevisionBody, SemanticRevisionLink, SemanticRevisionUnavailable,
};
use std::sync::Mutex;

fn foreign_document(host: &mut DocumentBoundaryHost) -> Value {
    let mut request = report();
    request["project_root"] = json!("/foreign");
    request["content"] = json!("Original foreign evidence");
    host.knowledge("create_document", request).unwrap()["document"].clone()
}

fn read_document_citation(host: &mut DocumentBoundaryHost, document: &Value) -> Value {
    host.knowledge("resolve_document_reference", json!({"project_root":document["project_root"], "target":{
        "target_kind":"document", "document_id":document["document_id"], "selection":{"mode":"current"}}})).unwrap()
}

fn destination(reference: &Value) -> Value {
    let mut reference = reference.clone();
    reference["target_kind"] = json!("qualified");
    let mut request = report();
    request["document_id"] = json!("cross-project-decision");
    request["operation_id"] = json!("retain-foreign-evidence");
    request["references"] = json!([reference]);
    request
}

// Keep source-loss corruption in DB Core; exercise Knowledge's approved-result handling here.
struct UnavailableCitationSemantic {
    persistence: Arc<LocalPersistence>,
    unavailable_record: SemanticRecordRef,
    successor_record: SemanticRecordRef,
    source_context: String,
    resolved_records: Arc<Mutex<Vec<SemanticRecordRef>>>,
}

impl SemanticPersistence for UnavailableCitationSemantic {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        let (project_root, record) = match &operation {
            SemanticOperation::ResolveRevision {
                project_root,
                record,
            } => (project_root.clone(), record.clone()),
            _ => {
                return SemanticPersistence::execute(self.persistence.as_ref(), operation, control);
            }
        };
        self.resolved_records.lock().unwrap().push(record.clone());
        if record != self.unavailable_record {
            return SemanticPersistence::execute(self.persistence.as_ref(), operation, control);
        }
        let output = SemanticPersistence::execute(self.persistence.as_ref(), operation, control)?;
        let mut resolution = match output {
            SemanticResult::RevisionResolution(Some(resolution)) => resolution,
            other => return Ok(other),
        };
        resolution.body = SemanticRevisionBody::Unavailable(SemanticRevisionUnavailable {
            recovery_operation_id: "approved-source-loss".into(),
            unavailable_at_ms: 2,
            canonical_lifecycle: Some("active".into()),
        });
        resolution.successors = vec![SemanticRevisionLink {
            project_root,
            source_context: self.source_context.clone(),
            older: record,
            newer: self.successor_record.clone(),
            created_at_ms: 3,
            commit_version: 3,
        }];
        Ok(SemanticResult::RevisionResolution(Some(resolution)))
    }

    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        SemanticPersistence::readiness(self.persistence.as_ref())
    }
}

fn install_unavailable_citation_read(
    host: &mut DocumentBoundaryHost,
    record_id: &str,
    source_context: &str,
) -> Arc<Mutex<Vec<SemanticRecordRef>>> {
    let resolved_records = Arc::new(Mutex::new(Vec::new()));
    let semantic = Arc::new(UnavailableCitationSemantic {
        persistence: host.persistence.clone(),
        unavailable_record: SemanticRecordRef::element(record_id),
        successor_record: SemanticRecordRef::element("fresh-successor"),
        source_context: source_context.into(),
        resolved_records: resolved_records.clone(),
    });
    host.broker = Arc::new(
        AppCoreHostCapabilityBroker::new(semantic, host.persistence.clone())
            .with_index_client(host.client_instance_id.clone())
            .unwrap(),
    );
    resolved_records
}

#[test]
fn qualified_document_read_and_save_keep_the_original_body_after_foreign_update() {
    let mut host = DocumentBoundaryHost::new();
    let original = foreign_document(&mut host);
    let read = read_document_citation(&mut host, &original);
    assert_eq!(read["state"], "available");
    let cited = host
        .knowledge("create_document", destination(&read["reference"]))
        .unwrap();
    host.knowledge("update_document", json!({"project_root":"/foreign", "document_id":original["document_id"],
        "expected_revision":original["revision"], "expected_semantic_element_id":original["semantic_element_id"], "content":"Revised foreign evidence"})).unwrap();
    let exact = host
        .knowledge(
            "resolve_document_reference",
            json!({"project_root":"/foreign", "target":read["reference"]["target"]}),
        )
        .unwrap();
    assert_eq!(
        exact["body"]["document"]["content"],
        "Original foreign evidence"
    );
    assert_eq!(
        cited["document"]["references"][0]["project"],
        read["reference"]["project"]
    );
    assert_eq!(source_claims(&host).len(), 1);
}

#[test]
fn lost_source_acknowledgement_reopens_and_replays_before_destination_acceptance() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("citation.db");
    let request = {
        let mut host = DocumentBoundaryHost::with_persistence(Arc::new(
            LocalPersistence::open(&path).unwrap(),
        ));
        let original = foreign_document(&mut host);
        let request = destination(&read_document_citation(&mut host, &original)["reference"]);
        host.drop_citation_reply = true;
        let error = host
            .knowledge("create_document", request.clone())
            .unwrap_err();
        assert_eq!(error.code, "lost_citation_reply");
        assert_eq!(source_claims(&host).len(), 1);
        assert!(
            host.knowledge(
                "get_document",
                json!({"project_root":"/project", "document_id":"cross-project-decision"})
            )
            .is_err()
        );
        request
    };
    let mut host =
        DocumentBoundaryHost::with_persistence(Arc::new(LocalPersistence::open(&path).unwrap()));
    let accepted = host.knowledge("create_document", request.clone()).unwrap();
    assert_eq!(
        host.knowledge("create_document", request).unwrap(),
        accepted
    );
    assert_eq!(source_claims(&host).len(), 1);
}

#[test]
fn forged_project_identity_and_missing_exact_evidence_never_publish() {
    let mut host = DocumentBoundaryHost::new();
    let original = foreign_document(&mut host);
    let mut request = destination(&read_document_citation(&mut host, &original)["reference"]);
    request["references"][0]["project"]["storage_authority"] = json!("forged-authority");
    assert!(host.knowledge("create_document", request).is_err());
    assert!(source_claims(&host).is_empty());
    let missing = host.knowledge("resolve_document_reference", json!({"project_root":"/foreign", "target":{
        "target_kind":"document", "document_id":original["document_id"],
        "selection":{"mode":"exact", "semantic_element_id":"unavailable", "source_context":"local"}}})).unwrap();
    assert_eq!(missing["state"], "missing");
    assert!(missing["body"].is_null());
    assert!(
        host.knowledge("create_document", destination(&missing["reference"]))
            .is_err()
    );
}

#[test]
fn exact_unavailable_citation_hides_cached_and_successor_bodies_and_cannot_publish() {
    let mut host = DocumentBoundaryHost::new();
    let original = foreign_document(&mut host);
    let semantic_element_id = original["semantic_element_id"].as_str().unwrap();
    let locator = read_document_citation(&mut host, &original);
    let source_context = locator["reference"]["target"]["selection"]["source_context"]
        .as_str()
        .unwrap();
    let mut successor = source_batch();
    successor["project_root"] = json!("/foreign");
    successor["semantic_sources"][0]["root_path"] = json!("/foreign");
    successor["semantic_sources"][0]["root_uri"] = json!("file:///foreign");
    successor["semantic_elements"][0]["semantic_element_id"] = json!("fresh-successor");
    successor["semantic_elements"][0]["metadata"] = json!({"body":"fresh successor bytes"});
    host.seed_sources(successor).unwrap();

    let unavailable_record = SemanticRecordRef::element(semantic_element_id);
    let cached = host.execute(SemanticOperation::ResolveRevision {
        project_root: "/foreign".into(),
        record: unavailable_record.clone(),
    });
    assert!(matches!(
        cached,
        SemanticResult::RevisionResolution(Some(resolution))
            if matches!(resolution.body, SemanticRevisionBody::Element(_))
    ));
    let successor_body = host.execute(SemanticOperation::ResolveRevision {
        project_root: "/foreign".into(),
        record: SemanticRecordRef::element("fresh-successor"),
    });
    assert!(matches!(
        successor_body,
        SemanticResult::RevisionResolution(Some(resolution))
            if matches!(resolution.body, SemanticRevisionBody::Element(_))
    ));

    let resolved_records =
        install_unavailable_citation_read(&mut host, semantic_element_id, source_context);
    let read = host
        .knowledge(
            "resolve_document_reference",
            json!({"project_root":"/foreign", "target":{
                "target_kind":"semantic_element",
                "semantic_element_id":semantic_element_id,
                "selection":{"mode":"exact", "source_context":source_context}
            }}),
        )
        .unwrap();
    assert_eq!(read["state"], "unavailable");
    assert!(read["body"].is_null());
    assert!(
        resolved_records
            .lock()
            .unwrap()
            .iter()
            .all(|record| record.record_id != "fresh-successor")
    );

    let error = host
        .knowledge("create_document", destination(&read["reference"]))
        .unwrap_err();
    assert_eq!(error.code, "knowledge_citation_unavailable");
    assert!(
        resolved_records
            .lock()
            .unwrap()
            .iter()
            .all(|record| record.record_id != "fresh-successor")
    );
    assert!(source_claims(&host).is_empty());
}

fn retention_request() -> Value {
    json!({"project_root":"/project", "operation_id":"retain-foreign-evidence", "action":"create"})
}

#[test]
fn cleanup_waits_for_destination_history_and_releases_only_its_original_claim() {
    let mut host = DocumentBoundaryHost::new();
    let original = foreign_document(&mut host);
    let reference = read_document_citation(&mut host, &original)["reference"].clone();
    let mut current = host
        .knowledge("create_document", destination(&reference))
        .unwrap()["document"]
        .clone();
    let mut other = destination(&reference);
    other["document_id"] = json!("another-dependent");
    other["operation_id"] = json!("another-source-claim");
    host.knowledge("create_document", other).unwrap();
    for content in ["References removed", "Further edit", "Final edit"] {
        current = host.knowledge("update_document", json!({"project_root":"/project",
            "document_id":current["document_id"], "expected_revision":current["revision"],
            "expected_semantic_element_id":current["semantic_element_id"], "references":[], "content":content})).unwrap()["document"].clone();
    }
    let held = host
        .knowledge("reconcile_document_citations", retention_request())
        .unwrap();
    assert_eq!(held["claims"][0]["state"], "retained");
    collect_history(&host, "/project", "collect-destination");
    let released = host
        .knowledge("reconcile_document_citations", retention_request())
        .unwrap();
    assert_eq!(released["claims"][0]["state"], "released");
    assert_eq!(
        host.knowledge("reconcile_document_citations", retention_request())
            .unwrap(),
        released
    );
    let claims = source_claims(&host);
    assert_eq!(claims.len(), 2);
    assert_eq!(
        claims
            .iter()
            .filter(|claim| !claim["records"].as_array().unwrap().is_empty())
            .count(),
        1
    );
}

fn collect_history(host: &DocumentBoundaryHost, root: &str, operation: &str) {
    let policy = lumvise_db_core::SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) =
        host.execute(SemanticOperation::PlanRevisionRetention {
            project_root: root.into(),
            operation_id: operation.into(),
            policy: policy.clone(),
        })
    else {
        panic!("expected retention plan");
    };
    assert!(!plan.records.is_empty());
    host.execute(SemanticOperation::CollectRevisionRetention { plan, policy });
}

#[test]
fn live_qualified_document_reference_creates_no_retention_claim() {
    let mut host = DocumentBoundaryHost::new();
    let original = foreign_document(&mut host);
    let mut reference = read_document_citation(&mut host, &original)["reference"].clone();
    reference["target"]["selection"] = json!({"mode":"current"});
    host.knowledge("create_document", destination(&reference))
        .unwrap();
    assert!(source_claims(&host).is_empty());
}

fn source_claims(host: &DocumentBoundaryHost) -> Vec<Value> {
    let SemanticResult::RevisionHistory(history) =
        host.execute(SemanticOperation::RevisionHistory {
            project_root: "/foreign".into(),
        })
    else {
        panic!("expected source revision history");
    };
    history
        .selections
        .into_iter()
        .filter(|selection| {
            selection
                .selection
                .selection_id
                .starts_with("knowledge-citation:")
        })
        .map(|selection| json!(selection))
        .collect()
}
