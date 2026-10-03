//! Route-level project lifecycle proof using only portable persistence operations.

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

use lumvise_db_core::{
    DbError, LocalPersistence, PersistenceResult, SemanticArtifact, SemanticElement,
    SemanticOperation, SemanticPersistence, SemanticReadiness, SemanticResult,
};
use lumvise_frontend_core::FrontendCore;
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

use super::PROJECT_REMOVAL_ENDPOINT;
use crate::app::http_router::route_http_request;
use crate::app::mcp_http::{HttpRequest, HttpResponse};
use crate::{AppCore, ProjectExecutionRequest, ProjectExecutionStatus};

const TARGET: &str = "/projects/sample";
const NEIGHBOR: &str = "/projects/sample-copy";
const COPY: &str = "/projects/identical";
const ROOTS: [&str; 3] = [TARGET, NEIGHBOR, COPY];

fn execute(persistence: &dyn SemanticPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn credentialed_app(
    persistence: Arc<LocalPersistence>,
    semantic: Arc<dyn SemanticPersistence>,
) -> AppCore {
    let app = AppCore::new(
        semantic,
        persistence,
        FrontendCore::default(),
        LlmProviderRegistry::empty(),
    );
    app.install_bridge_credential_store(Arc::default()).unwrap();
    app.set_bridge_credential(Some("removal-test".into()), u64::MAX)
        .unwrap();
    app
}

fn request(body: Value) -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        path: PROJECT_REMOVAL_ENDPOINT.into(),
        query: BTreeMap::new(),
        authorization: Some("Bearer removal-test".into()),
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn remove(app: &AppCore, root: &str) -> HttpResponse {
    route_http_request(app, request(json!({"projectRoot": root})))
}

fn response_body(response: &HttpResponse) -> Value {
    serde_json::from_slice(response.buffered_bytes().unwrap()).unwrap()
}

fn owner(root: &str, name: &str) -> SemanticElement {
    serde_json::from_value(json!({"project_root": root,
        "semantic_element_id": format!("{root}#{name}"), "semantic_source_id": "fixture",
        "path": format!("{name}.rs"), "element_kind": "file", "name": name,
        "content_fingerprint": "fp1:0000000000000001:identical-content",
        "lifecycle": "active", "metadata": {}}))
    .unwrap()
}

fn sync_project(persistence: &dyn SemanticPersistence, root: &str, names: &[&str]) {
    execute(
        persistence,
        SemanticOperation::SyncStructure {
            project_root: root.into(),
            elements: names.iter().map(|name| owner(root, name)).collect(),
            relationships: Vec::new(),
        },
    );
}

fn artifact(root: &str, kind: &str, owner_name: &str) -> SemanticArtifact {
    let id = format!("{root}/{kind}");
    let mut metadata = json!({"source_artifact_id": format!("{TARGET}/{kind}")});
    if matches!(kind, "knowledge" | "inactive") {
        metadata["knowledge"] = json!({"project_root": root});
    }
    if kind == "canvas" {
        metadata["canvas"] = json!({"project_root": root});
    }
    serde_json::from_value(json!({"artifact_id": id,
        "semantic_element_id": format!("{root}#{owner_name}"), "artifact_kind": kind,
        "title": kind, "content": format!("retained {id}"), "content_ref": format!("content:{id}"),
        "dependencies": [], "metadata": metadata}))
    .unwrap()
}

fn upsert(persistence: &dyn SemanticPersistence, record: SemanticArtifact) {
    execute(
        persistence,
        SemanticOperation::UpsertArtifact {
            artifact: record,
            media_type: "text/plain".into(),
        },
    );
}

fn attach(persistence: &dyn SemanticPersistence, id: &str) {
    execute(
        persistence,
        SemanticOperation::ArtifactBlobPut {
            artifact_id: id.into(),
            content_ref: format!("attachment:{id}"),
            media_type: "image/png".into(),
            content: format!("image owned by {id}").into_bytes(),
        },
    );
}

fn seed_projects(persistence: &dyn SemanticPersistence) {
    for root in ROOTS {
        sync_project(persistence, root, &["active", "retired"]);
        for (kind, owner_name) in [
            ("knowledge", "active"),
            ("canvas", "active"),
            ("generic", "active"),
            ("inactive", "retired"),
        ] {
            let record = artifact(root, kind, owner_name);
            upsert(persistence, record.clone());
            attach(persistence, &record.artifact_id);
        }
        sync_project(persistence, root, &["active"]);
    }
    let mut linked = artifact(NEIGHBOR, "canvas", "active");
    linked.dependencies = serde_json::from_value(json!([
        {"target": {"target_kind": "artifact", "artifact_id": format!("{COPY}/knowledge")}}
    ]))
    .unwrap();
    upsert(persistence, linked);
}

fn project_artifacts(persistence: &dyn SemanticPersistence, root: &str) -> Vec<SemanticArtifact> {
    let SemanticResult::Artifacts(mut artifacts) = execute(
        persistence,
        SemanticOperation::ProjectArtifacts {
            project_root: root.into(),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected project artifacts");
    };
    artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    artifacts
}

fn live_roots(persistence: &dyn SemanticPersistence) -> Vec<String> {
    let SemanticResult::ProjectRoots(mut roots) =
        execute(persistence, SemanticOperation::ProjectRoots)
    else {
        panic!("expected project roots");
    };
    roots.sort();
    roots
}

fn blob_content(persistence: &dyn SemanticPersistence, content_ref: &str) -> Option<Vec<u8>> {
    let SemanticResult::ArtifactBlob(blob) = execute(
        persistence,
        SemanticOperation::ArtifactBlobGet {
            content_ref: content_ref.into(),
        },
    ) else {
        panic!("expected artifact blob");
    };
    blob.map(|blob| blob.content)
}

fn assert_removed_artifacts(persistence: &dyn SemanticPersistence, root: &str) {
    assert!(project_artifacts(persistence, root).is_empty());
    for kind in ["knowledge", "canvas", "generic", "inactive"] {
        let id = format!("{root}/{kind}");
        assert!(matches!(
            execute(
                persistence,
                SemanticOperation::Artifact {
                    artifact_id: id.clone()
                }
            ),
            SemanticResult::Artifact(None)
        ));
        assert_eq!(blob_content(persistence, &format!("attachment:{id}")), None);
        assert_eq!(blob_content(persistence, &format!("content:{id}")), None);
    }
}

fn assert_surviving_artifacts(
    persistence: &dyn SemanticPersistence,
    root: &str,
    expected: &[SemanticArtifact],
) {
    assert_eq!(project_artifacts(persistence, root), expected);
    for record in expected {
        assert_eq!(
            blob_content(persistence, &format!("attachment:{}", record.artifact_id)),
            Some(format!("image owned by {}", record.artifact_id).into_bytes())
        );
        assert_eq!(
            blob_content(persistence, record.content_ref.as_ref().unwrap()),
            record.content.as_ref().map(|text| text.as_bytes().to_vec())
        );
    }
}

#[test]
fn removal_is_exact_including_inactive_artifacts_and_preserves_other_copies() {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    seed_projects(persistence.as_ref());
    let neighbors = [NEIGHBOR, COPY].map(|root| project_artifacts(persistence.as_ref(), root));
    assert_eq!(project_artifacts(persistence.as_ref(), TARGET).len(), 4);
    let SemanticResult::Elements(inactive) = execute(
        persistence.as_ref(),
        SemanticOperation::ElementsByIdsIncludingInactive {
            project_root: TARGET.into(),
            semantic_element_ids: [format!("{TARGET}#retired")].into_iter().collect(),
        },
    ) else {
        panic!("expected inactive owner records");
    };
    assert_eq!(inactive.len(), 1);
    assert_eq!(inactive[0].lifecycle, "inactive");
    let app = credentialed_app(persistence.clone(), persistence.clone());
    let response = remove(&app, TARGET);
    assert_eq!(response.status, "200 OK", "{}", response_body(&response));
    assert_eq!(
        response_body(&response),
        json!({"projectRoot": TARGET, "removedElements": 1, "removedArtifacts": 4})
    );
    assert_eq!(
        live_roots(persistence.as_ref()),
        vec![COPY.to_owned(), NEIGHBOR.to_owned()]
    );
    assert_removed_artifacts(persistence.as_ref(), TARGET);
    for (root, expected) in [NEIGHBOR, COPY].into_iter().zip(neighbors) {
        assert_surviving_artifacts(persistence.as_ref(), root, &expected);
    }
}

#[test]
fn removal_leaves_the_named_project_folder_and_its_source_files_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let sentinel = directory.path().join("source.rs");
    std::fs::write(&sentinel, "fn kept_source() {}\n").unwrap();
    let root = directory.path().to_str().unwrap();
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    sync_project(persistence.as_ref(), root, &["active"]);
    upsert(persistence.as_ref(), artifact(root, "knowledge", "active"));
    let app = credentialed_app(persistence.clone(), persistence.clone());
    assert_eq!(remove(&app, root).status, "200 OK");
    assert!(directory.path().is_dir());
    assert_eq!(
        std::fs::read_to_string(sentinel).unwrap(),
        "fn kept_source() {}\n"
    );
    assert!(!live_roots(persistence.as_ref()).contains(&root.to_owned()));
    assert!(project_artifacts(persistence.as_ref(), root).is_empty());
}

#[test]
fn invalid_credentials_and_invalid_bodies_preserve_every_project() {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    seed_projects(persistence.as_ref());
    let before = ROOTS.map(|root| project_artifacts(persistence.as_ref(), root));
    let app = credentialed_app(persistence.clone(), persistence.clone());
    for authorization in [None, Some("Bearer wrong-token".into())] {
        let mut attempted = request(json!({"projectRoot": TARGET}));
        attempted.authorization = authorization;
        assert_eq!(
            route_http_request(&app, attempted).status,
            "401 Unauthorized"
        );
    }
    for body in [
        json!({}),
        json!({"projectRoot": "  "}),
        json!({"projectRoot": 7}),
        json!({"projectRoot": TARGET, "extra": true}),
    ] {
        assert_eq!(
            route_http_request(&app, request(body)).status,
            "400 Bad Request"
        );
    }
    let mut malformed = request(json!({}));
    malformed.body = b"{invalid-json".to_vec();
    assert_eq!(
        route_http_request(&app, malformed).status,
        "400 Bad Request"
    );
    assert_eq!(live_roots(persistence.as_ref()).len(), 3);
    for (root, expected) in ROOTS.into_iter().zip(before) {
        assert_surviving_artifacts(persistence.as_ref(), root, &expected);
    }
}

#[test]
fn repeated_removal_is_idempotent_and_explicit_reimport_starts_without_old_artifacts() {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    seed_projects(persistence.as_ref());
    let app = credentialed_app(persistence.clone(), persistence.clone());
    assert_eq!(remove(&app, TARGET).status, "200 OK");
    let repeated = remove(&app, TARGET);
    assert_eq!(repeated.status, "200 OK");
    assert_eq!(
        response_body(&repeated),
        json!({"projectRoot": TARGET, "removedElements": 0, "removedArtifacts": 0})
    );
    sync_project(persistence.as_ref(), TARGET, &["active"]);
    assert!(live_roots(persistence.as_ref()).contains(&TARGET.to_owned()));
    assert_removed_artifacts(persistence.as_ref(), TARGET);
    upsert(persistence.as_ref(), artifact(TARGET, "fresh", "active"));
    assert_eq!(project_artifacts(persistence.as_ref(), TARGET).len(), 1);
}

#[test]
fn a_late_artifact_write_cannot_revive_an_inactive_owner() {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    seed_projects(persistence.as_ref());
    let app = credentialed_app(persistence.clone(), persistence.clone());
    assert_eq!(remove(&app, TARGET).status, "200 OK");
    let rejected = persistence.execute(
        SemanticOperation::UpsertArtifact {
            artifact: artifact(TARGET, "late", "active"),
            media_type: "text/plain".into(),
        },
        &InvocationControl::sixty_seconds(),
    );
    assert!(
        rejected.is_err(),
        "late artifact upsert must reject inactive project owner: {rejected:?}"
    );
    assert_removed_artifacts(persistence.as_ref(), TARGET);
    assert!(!live_roots(persistence.as_ref()).contains(&TARGET.to_owned()));
}

struct FakeFailingArtifactRemoval {
    persistence: Arc<LocalPersistence>,
    removal_calls: AtomicUsize,
}

impl SemanticPersistence for FakeFailingArtifactRemoval {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        if matches!(operation, SemanticOperation::RemoveArtifact { .. })
            && self.removal_calls.fetch_add(1, Ordering::SeqCst) == 1
        {
            return Err(DbError::invalid_value(
                "injected artifact removal failure",
                "successful RemoveArtifact",
            ));
        }
        self.persistence.execute(operation, control)
    }
    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        SemanticPersistence::readiness(self.persistence.as_ref())
    }
}

#[test]
fn a_partial_artifact_failure_can_be_retried_without_touching_neighbor_records() {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    seed_projects(persistence.as_ref());
    let neighbor = project_artifacts(persistence.as_ref(), NEIGHBOR);
    let semantic = Arc::new(FakeFailingArtifactRemoval {
        persistence: persistence.clone(),
        removal_calls: AtomicUsize::new(0),
    });
    let app = credentialed_app(persistence.clone(), semantic);
    let failure = remove(&app, TARGET);
    assert_eq!(failure.status, "500 Internal Server Error");
    assert!(
        response_body(&failure)
            .to_string()
            .contains("injected artifact removal failure")
    );
    assert_eq!(project_artifacts(persistence.as_ref(), TARGET).len(), 3);
    assert!(!live_roots(persistence.as_ref()).contains(&TARGET.to_owned()));
    assert_surviving_artifacts(persistence.as_ref(), NEIGHBOR, &neighbor);
    let retry = remove(&app, TARGET);
    assert_eq!(retry.status, "200 OK", "{}", response_body(&retry));
    assert_eq!(response_body(&retry)["removedArtifacts"], 3);
    assert_removed_artifacts(persistence.as_ref(), TARGET);
    assert_surviving_artifacts(persistence.as_ref(), NEIGHBOR, &neighbor);
}

#[test]
fn removal_survives_local_persistence_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("removal.db");
    {
        let persistence = Arc::new(LocalPersistence::open(&database).unwrap());
        seed_projects(persistence.as_ref());
        let app = credentialed_app(persistence.clone(), persistence);
        assert_eq!(remove(&app, TARGET).status, "200 OK");
    }
    let reopened = LocalPersistence::open(&database).unwrap();
    assert_eq!(
        live_roots(&reopened),
        vec![COPY.to_owned(), NEIGHBOR.to_owned()]
    );
    assert_removed_artifacts(&reopened, TARGET);
    assert_eq!(project_artifacts(&reopened, NEIGHBOR).len(), 4);
}

fn submit_provider_job(app: &AppCore, root: &str, key: &str) -> crate::ProjectExecutionJob {
    app.project_execution()
        .submit(ProjectExecutionRequest {
            requester_id: "removal-requester".into(),
            project_root: root.into(),
            capability_id: "run".into(),
            idempotency_key: key.into(),
            input: json!({}),
            priority: Default::default(),
        })
        .unwrap()
}

#[test]
fn removal_cancels_only_target_jobs_including_already_succeeded_jobs() {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    seed_projects(persistence.as_ref());
    let app = credentialed_app(persistence.clone(), persistence);
    let mut receivers = Vec::new();
    for root in [TARGET, NEIGHBOR] {
        let (commands, receiver) = mpsc::sync_channel(8);
        app.project_execution()
            .register_provider(root, root, ["run".into()], commands)
            .unwrap();
        receivers.push(receiver);
    }
    let queued = submit_provider_job(&app, TARGET, "target-pending");
    let succeeded = submit_provider_job(&app, TARGET, "target-completed");
    app.project_execution()
        .record_result(TARGET, &succeeded.job_id, Ok(json!({"done": true})))
        .unwrap();
    let neighbor = submit_provider_job(&app, NEIGHBOR, "neighbor-pending");
    assert_eq!(remove(&app, TARGET).status, "200 OK");
    for job in [&queued, &succeeded] {
        assert_eq!(
            app.project_execution()
                .status("removal-requester", &job.job_id)
                .unwrap()
                .status,
            ProjectExecutionStatus::Cancelled
        );
    }
    assert_eq!(
        app.project_execution()
            .status("removal-requester", &neighbor.job_id)
            .unwrap()
            .status,
        ProjectExecutionStatus::Queued
    );
    assert!(
        app.project_execution()
            .record_result(TARGET, &queued.job_id, Ok(json!({"late": true})))
            .is_err()
    );
}
