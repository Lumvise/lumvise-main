//! Real shared scanner -> Semantic -> public native persistence; only publication faults are faked.
use super::jobs::RETAINED_FINISHED_JOBS;
use super::*;
use lumvise_db_core::{
    LocalPersistence, PersistenceResult, RelationalOperation, RelationalPersistence,
    RelationalReadiness, RelationalResult, SemanticPersistence, SemanticRevisionSelectionSnapshot,
};
use lumvise_frontend_core::{AppSettingsPatch, FrontendCore};
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, HostPluginInvocation,
    HostPluginInvoker, PluginInvocationClass, PluginInvocationContext,
};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_plugin_semantic::SemanticPlugin;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

const PROJECT: &str = "/shared-folder-import";

struct SemanticImportBoundary {
    broker: crate::AppCoreHostCapabilityBroker,
    client: String,
    batches: Vec<Value>,
    lightweight_pages: Option<Vec<IngestionPageEvidence>>,
    /// Every `record_index_log` entry the importer asked Semantic to write.
    logs: Vec<Value>,
    lose_next_commit_reply: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IngestionPageEvidence {
    operation_id: String,
    page_index: usize,
    page_count: usize,
    element_count: usize,
    relationship_count: usize,
    element_paths: BTreeSet<String>,
    replace_paths: BTreeSet<String>,
    removed_paths: BTreeSet<String>,
}

fn ingestion_page_evidence(input: &Value) -> IngestionPageEvidence {
    let paths = |field: &str| {
        input[field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["path"].as_str().map(str::to_owned))
            .collect()
    };
    let strings = |field: &str| {
        input[field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let elements = input["semantic_elements"]
        .as_array()
        .expect("ingestion page contains semantic_elements");
    let relationships = input["semantic_relationships"]
        .as_array()
        .expect("ingestion page contains semantic_relationships");
    IngestionPageEvidence {
        operation_id: input["ingestion_job_id"]
            .as_str()
            .expect("ingestion page contains operation ID")
            .to_owned(),
        page_index: usize::try_from(
            input["ingestion_page_index"]
                .as_u64()
                .expect("ingestion page index is non-negative"),
        )
        .expect("ingestion page index fits usize"),
        page_count: usize::try_from(
            input["ingestion_page_count"]
                .as_u64()
                .expect("ingestion page count is non-negative"),
        )
        .expect("ingestion page count fits usize"),
        element_count: elements.len(),
        relationship_count: relationships.len(),
        element_paths: paths("semantic_elements"),
        replace_paths: strings("replace_paths"),
        removed_paths: strings("removed_paths"),
    }
}

impl HostCallTransport for SemanticImportBoundary {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        if capability == "runtime.project_execution" && input["operation"] == "scope" {
            return Ok(json!({"client_instance_id":self.client}));
        }
        if capability == "frontend.action" {
            return Ok(json!({"accepted":true}));
        }
        self.broker
            .invoke(HostCapabilityRequest {
                plugin_id: "builtin.semantic".into(),
                invocation_id: "shared-import".into(),
                call_id: "semantic-storage".into(),
                capability_id: capability.into(),
                required_version: if capability == "storage.semantic" {
                    "^1.4.0"
                } else {
                    "^1.0"
                }
                .into(),
                input,
            })
            .map_err(|error| PluginError::new("import_boundary", error.to_string(), false))
    }
}

struct SemanticImportPublisher {
    boundary: Mutex<SemanticImportBoundary>,
}

impl HostPluginInvoker for SemanticImportPublisher {
    fn invoke_plugin(&self, request: HostPluginInvocation) -> Result<Value, HostCapabilityError> {
        assert_eq!(request.plugin_id, "builtin.semantic");
        let mut boundary = self.boundary.lock().unwrap();
        let ingestion = request.export_id == INGEST_EXPORT_ID;
        if ingestion {
            if let Some(pages) = &mut boundary.lightweight_pages {
                pages.push(ingestion_page_evidence(&request.input));
            } else {
                boundary.batches.push(request.input.clone());
            }
        }
        if request.export_id == RECORD_INDEX_LOG_EXPORT_ID {
            boundary.logs.push(request.input.clone());
        }
        let output = SemanticPlugin
            .dispatch(
                &request.export_id,
                request.input,
                &mut PluginContext::for_test(&mut *boundary),
            )
            .map_err(|error| {
                HostCapabilityError::new(
                    "project.import",
                    error.code,
                    error.message,
                    error.retryable,
                )
            })?;
        if ingestion && boundary.lose_next_commit_reply && output["ingestion_status"] == "completed"
        {
            boundary.lose_next_commit_reply = false;
            return Err(HostCapabilityError::new(
                "project.import",
                "lost_commit_reply",
                "accepted Semantic reply was lost; expected frozen replay",
                true,
            ));
        }
        Ok(output)
    }
}

struct SharedImportHost {
    app: AppCore,
    local: Arc<LocalPersistence>,
    publisher: Arc<SemanticImportPublisher>,
}

impl SharedImportHost {
    fn new(local: Arc<LocalPersistence>, client: &str) -> Self {
        Self::with_relational(local.clone(), local, client)
    }

    fn with_relational(
        local: Arc<LocalPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        client: &str,
    ) -> Self {
        let semantic: Arc<dyn SemanticPersistence> = local.clone();
        let app = AppCore::new(
            semantic.clone(),
            relational.clone(),
            FrontendCore::default(),
            LlmProviderRegistry::empty(),
        );
        app.install_index_publication_client(client.into()).unwrap();
        let boundary = SemanticImportBoundary {
            broker: crate::AppCoreHostCapabilityBroker::new(semantic, relational)
                .with_index_client(client.into())
                .unwrap(),
            client: client.into(),
            batches: vec![],
            lightweight_pages: None,
            logs: vec![],
            lose_next_commit_reply: false,
        };
        Self {
            app,
            local,
            publisher: Arc::new(SemanticImportPublisher {
                boundary: Mutex::new(boundary),
            }),
        }
    }

    fn import(&self, root: &Path) -> Result<Value, HostCapabilityError> {
        drive_import(self, root, self.publisher.clone())
    }

    fn capture_lightweight_ingestion(&self) {
        self.publisher.boundary.lock().unwrap().lightweight_pages = Some(Vec::new());
    }

    fn selected(&self) -> SemanticRevisionSelectionSnapshot {
        let SemanticResult::RevisionSelection(Some(snapshot)) = SemanticPersistence::execute(
            self.local.as_ref(),
            SemanticOperation::RevisionSelection {
                project_root: PROJECT.into(),
                selection_id: "working".into(),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap() else {
            panic!("expected selected source")
        };
        snapshot
    }
}

struct GatedCheckpointReads {
    local: Arc<LocalPersistence>,
    enabled: AtomicBool,
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl RelationalPersistence for GatedCheckpointReads {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        if matches!(&operation, RelationalOperation::GetPluginData{plugin_id,..} if plugin_id=="app.index-publication")
            && self.enabled.swap(false, Ordering::AcqRel)
        {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        RelationalPersistence::execute(self.local.as_ref(), operation, control)
    }

    fn readiness(&self) -> PersistenceResult<RelationalReadiness> {
        RelationalPersistence::readiness(self.local.as_ref())
    }
}

struct CheckpointReadRelease(mpsc::Sender<()>);
impl Drop for CheckpointReadRelease {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn import_admission_returns_while_checkpoint_io_is_blocked() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let (entered, blocked) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let gate = Arc::new(GatedCheckpointReads {
        local: local.clone(),
        enabled: AtomicBool::new(true),
        entered,
        release: Mutex::new(released),
    });
    let host = SharedImportHost::with_relational(local, gate, "client");
    let root = checkout();
    let service = host.app.plugin_host_services.project_import_service();
    let publisher = host.publisher.clone();
    let input = json!({"folderPath":root.path(),"projectRoot":PROJECT,"statusOnly":true});
    let (sent, admission) = mpsc::channel();
    let request = input.clone();
    let _release = CheckpointReadRelease(release);
    let caller = std::thread::spawn(move || {
        sent.send(service.import_folder(request, &import_context(), publisher))
            .unwrap()
    });
    blocked.recv_timeout(Duration::from_secs(3)).unwrap();
    let result = admission
        .recv_timeout(Duration::from_secs(1))
        .expect("checkpoint I/O held import admission")
        .unwrap();
    assert_eq!(result["phase"], "preparing");
    caller.join().unwrap();
    drop(_release);
    let mut continuation = input;
    continuation["importId"] = result["importId"].clone();
    continuation["statusOnly"] = json!(false);
    drive_import_input(&host, continuation, host.publisher.clone()).unwrap();
}

fn import_context() -> PluginInvocationContext {
    PluginInvocationContext::new(
        "shared-folder-import",
        "test-mcp-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.semantic", "index_project")
}

fn checkout() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "fn b() {}\n").unwrap();
    root
}

fn file_id(snapshot: &SemanticRevisionSelectionSnapshot, path: &str) -> String {
    snapshot
        .elements
        .iter()
        .find(|element| element.path == path && element.element_kind == "file")
        .unwrap()
        .semantic_element_id
        .clone()
}

#[test]
fn shared_folder_import_merges_two_checkouts_and_keeps_their_own_pins() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let first = checkout();
    let second = checkout();
    let a = SharedImportHost::new(local.clone(), "client-a");
    let b = SharedImportHost::new(local, "client-b");
    a.import(first.path()).unwrap();
    let original = a.selected();
    b.import(second.path()).unwrap();
    assert_eq!(file_id(&a.selected(), "a.rs"), file_id(&original, "a.rs"));
    std::fs::write(first.path().join("a.rs"), "fn a() { let value = 2; }\n").unwrap();
    a.import(first.path()).unwrap();
    let a2 = file_id(&a.selected(), "a.rs");
    std::fs::write(second.path().join("b.rs"), "fn b() { let value = 2; }\n").unwrap();
    b.publisher.boundary.lock().unwrap().batches.clear();
    b.import(second.path()).unwrap();
    assert_eq!(file_id(&b.selected(), "a.rs"), a2);
    assert_ne!(file_id(&b.selected(), "b.rs"), file_id(&original, "b.rs"));
    let boundary = b.publisher.boundary.lock().unwrap();
    assert!(
        boundary
            .batches
            .iter()
            .flat_map(|page| page["semantic_elements"].as_array().unwrap())
            .all(|element| element["path"] == "b.rs")
    );
    let pin = boundary.batches[0]["revision_observation"]["producer_base"]["selection_id"]
        .as_str()
        .unwrap();
    let SemanticResult::RevisionSelection(Some(base)) = SemanticPersistence::execute(
        b.local.as_ref(),
        SemanticOperation::RevisionSelection {
            project_root: PROJECT.into(),
            selection_id: pin.into(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap() else {
        panic!("expected producer pin")
    };
    assert_eq!(file_id(&base, "a.rs"), file_id(&original, "a.rs"));
}

#[test]
fn shared_folder_import_replays_lost_acknowledgement_before_new_edits() {
    let root = checkout();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.sqlite");
    let host = SharedImportHost::new(Arc::new(LocalPersistence::open(&path).unwrap()), "client");
    host.import(root.path()).unwrap();
    std::fs::write(root.path().join("a.rs"), "fn a() { let value = 2; }\n").unwrap();
    {
        let mut boundary = host.publisher.boundary.lock().unwrap();
        boundary.lose_next_commit_reply = true;
        boundary.batches.clear();
    }
    assert!(host.import(root.path()).is_err());
    let original = host.publisher.boundary.lock().unwrap().batches[0].clone();
    let accepted = file_id(&host.selected(), "a.rs");
    drop(host);
    std::fs::write(root.path().join("b.rs"), "fn b() { let value = 3; }\n").unwrap();
    let host = SharedImportHost::new(Arc::new(LocalPersistence::open(&path).unwrap()), "client");
    host.import(root.path()).unwrap();
    let boundary = host.publisher.boundary.lock().unwrap();
    assert_eq!(boundary.batches[0], original);
    assert_eq!(file_id(&host.selected(), "a.rs"), accepted);
    assert!(boundary.batches.iter().skip(1).any(|page| {
        page["semantic_elements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["path"] == "b.rs")
    }));
}

#[test]
fn shared_folder_import_reads_current_pdf_preference_and_shared_converter() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("report.pdf"),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../project-indexer/tests/fixtures/documents/report.pdf"
        )),
    )
    .unwrap();
    host.import(root.path()).unwrap();
    host.app
        .frontend()
        .apply_app_settings_patch(&AppSettingsPatch::DocumentEnhancementEnabled(false))
        .unwrap();
    host.publisher.boundary.lock().unwrap().batches.clear();
    host.import(root.path()).unwrap();
    let boundary = host.publisher.boundary.lock().unwrap();
    let file = boundary
        .batches
        .iter()
        .flat_map(|page| page["semantic_elements"].as_array().unwrap())
        .find(|element| element["semantic_element_type"] == "file")
        .unwrap();
    assert_eq!(
        file["metadata"]["markdown_alias"]["enhancement"],
        "disabled"
    );
    assert!(
        file["metadata"]["markdown_alias"]["content"]
            .as_str()
            .unwrap()
            .contains("Ocean measurements")
    );
    assert!(Arc::ptr_eq(
        &host.app.document_converter,
        &host.app.plugin_host_services.project_document_converter()
    ));
    assert!(Arc::ptr_eq(
        &host.app.project_import_sessions,
        &host.app.plugin_host_services.project_import_sessions()
    ));
}

#[test]
fn shared_folder_import_rejects_expired_or_cancelled_lifecycle_before_scanning() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let cancelled = import_context();
    cancelled.cancellation().cancel();
    let expired = PluginInvocationContext::new(
        "expired",
        "owner",
        PluginInvocationClass::Foreground,
        Instant::now() - Duration::from_secs(1),
    );
    let root = checkout();
    for context in [cancelled, expired] {
        assert!(
            host.app
                .plugin_host_services
                .project_import_service()
                .import_folder(
                    json!({"folderPath":root.path()}),
                    &context,
                    host.publisher.clone()
                )
                .unwrap_err()
                .to_string()
                .contains("folder import cancelled or deadline expired; expected active inherited invocation")
        );
    }
    assert!(host.app.project_import_sessions.lock().unwrap().is_empty());
    assert!(host.publisher.boundary.lock().unwrap().batches.is_empty());
}

struct MalformedReceiptPublisher(Arc<SemanticImportPublisher>);

impl HostPluginInvoker for MalformedReceiptPublisher {
    fn invoke_plugin(&self, request: HostPluginInvocation) -> Result<Value, HostCapabilityError> {
        let mut response = self.0.invoke_plugin(request)?;
        if !response["publication"].is_null() {
            response["publication"] =
                json!({"outcome":"accepted","value":"malformed-original-receipt"});
        }
        Ok(response)
    }
}

#[test]
fn shared_folder_import_preserves_malformed_receipt_value_and_replays_pending() {
    let root = checkout();
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let message = malformed_import_error(&host, root.path()).to_string();
    assert!(
        message.contains("invalid Semantic publication"),
        "{message}"
    );
    assert!(message.contains("malformed-original-receipt"), "{message}");
    assert!(message.contains("expected typed outcome"), "{message}");
    host.import(root.path()).unwrap();
    assert!(!host.selected().elements.is_empty());
}

fn malformed_import_error(host: &SharedImportHost, root: &Path) -> HostCapabilityError {
    drive_import(
        host,
        root,
        Arc::new(MalformedReceiptPublisher(host.publisher.clone())),
    )
    .unwrap_err()
}

fn drive_import(
    host: &SharedImportHost,
    root: &Path,
    publisher: Arc<dyn ImportPublicationPort>,
) -> Result<Value, HostCapabilityError> {
    drive_import_input(
        host,
        json!({"folderPath":root,"projectRoot":PROJECT}),
        publisher,
    )
}

fn drive_import_input(
    host: &SharedImportHost,
    mut input: Value,
    publisher: Arc<dyn ImportPublicationPort>,
) -> Result<Value, HostCapabilityError> {
    let service = host.app.plugin_host_services.project_import_service();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < deadline, "import fixture failed to settle");
        let progress =
            service.import_folder(input.clone(), &import_context(), publisher.clone())?;
        match progress["phase"].as_str() {
            Some("completed" | "completed_with_errors") => return Ok(progress["result"].clone()),
            Some("failed") => {
                return Err(HostCapabilityError::new(
                    "project.import",
                    "import_failed",
                    progress["error"].to_string(),
                    false,
                ));
            }
            _ => input["importId"] = progress["importId"].clone(),
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

impl ImportPublicationPort for SemanticImportPublisher {}
impl ImportPublicationPort for MalformedReceiptPublisher {}

struct SpentPublicationStep {
    semantic: Arc<SemanticImportPublisher>,
    control: Mutex<Option<InvocationControl>>,
}

impl ImportPublicationPort for SpentPublicationStep {
    fn authorize(&self, control: &InvocationControl) -> Result<(), HostCapabilityError> {
        *self.control.lock().unwrap() = Some(control.clone());
        Ok(())
    }
}

impl HostPluginInvoker for SpentPublicationStep {
    fn invoke_plugin(&self, request: HostPluginInvocation) -> Result<Value, HostCapabilityError> {
        let output = self.semantic.invoke_plugin(request)?;
        if output["ingestion_status"] == "completed" {
            self.control.lock().unwrap().as_ref().unwrap().cancel();
        }
        Ok(output)
    }
}

#[test]
fn accepted_publication_acknowledges_after_its_step_budget_is_spent() {
    let root = checkout();
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let publisher = Arc::new(SpentPublicationStep {
        semantic: host.publisher.clone(),
        control: Mutex::new(None),
    });
    drive_import(&host, root.path(), publisher).unwrap();
    assert!(!host.selected().elements.is_empty());
    let log = host
        .publisher
        .invoke_plugin(HostPluginInvocation {
            plugin_id: "builtin.semantic".into(),
            export_id: "latest_index_log".into(),
            input: json!({"project_root":PROJECT}),
        })
        .unwrap();
    assert_eq!(log["index_log"]["status"], "completed", "{log}");
    host.publisher.boundary.lock().unwrap().batches.clear();
    let unchanged = host.import(root.path()).unwrap();
    assert_eq!(unchanged["filesScanned"], 0);
    assert!(host.publisher.boundary.lock().unwrap().batches.is_empty());
}

#[test]
fn continuing_import_cancellation_and_scope_are_checked_before_publication() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    let service = host.app.plugin_host_services.project_import_service();
    let mut input = json!({"folderPath":root.path(),"projectRoot":PROJECT,"statusOnly":true});
    let pending = service
        .import_folder(input.clone(), &import_context(), host.publisher.clone())
        .unwrap();
    assert_eq!(pending["phase"], "preparing");
    assert!(host.publisher.boundary.lock().unwrap().batches.is_empty());
    input["importId"] = pending["importId"].clone();
    let other_owner = PluginInvocationContext::new(
        "other",
        "other",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_route_identity("other-connection", None);
    assert!(
        service
            .import_folder(input.clone(), &other_owner, host.publisher.clone())
            .is_err()
    );
    let mut wrong_scope = input.clone();
    wrong_scope["replacePaths"] = json!(["a.rs"]);
    assert!(
        service
            .import_folder(wrong_scope, &import_context(), host.publisher.clone())
            .is_err()
    );
    input["cancel"] = json!(true);
    let canceled = service
        .import_folder(input, &import_context(), host.publisher.clone())
        .unwrap();
    assert_eq!(canceled["phase"], "canceled");
    assert!(host.publisher.boundary.lock().unwrap().batches.is_empty());
    host.import(root.path()).unwrap();
    assert!(!host.selected().elements.is_empty());
}

#[test]
fn a_later_refresh_does_not_erase_an_unread_completed_import_handle() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    let service = host.app.plugin_host_services.project_import_service();
    let mut input = json!({"folderPath":root.path(),"projectRoot":PROJECT,"statusOnly":true});
    let first = service
        .import_folder(input.clone(), &import_context(), host.publisher.clone())
        .unwrap();
    input["importId"] = first["importId"].clone();
    input["statusOnly"] = json!(false);
    drive_import_input(&host, input.clone(), host.publisher.clone()).unwrap();
    host.import(root.path()).unwrap();
    input["statusOnly"] = json!(true);
    let completed = service
        .import_folder(input, &import_context(), host.publisher.clone())
        .unwrap();
    assert_eq!(completed["phase"], "completed");
    assert_eq!(completed["importId"], first["importId"]);
}

#[test]
fn cold_selective_import_reconciles_deleted_paths_against_the_complete_inventory() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    host.import(root.path()).unwrap();
    host.app.project_import_sessions.lock().unwrap().clear();
    std::fs::remove_file(root.path().join("a.rs")).unwrap();
    std::fs::write(root.path().join("b.rs"), "fn updated_b() {}\n").unwrap();
    drive_import_input(
        &host,
        json!({"folderPath":root.path(),"projectRoot":PROJECT,
        "replacePaths":["a.rs","b.rs"]}),
        host.publisher.clone(),
    )
    .unwrap();
    let current = host.selected();
    assert!(
        !current
            .elements
            .iter()
            .any(|element| element.path == "a.rs")
    );
    assert!(
        current
            .elements
            .iter()
            .any(|element| element.name == "updated_b")
    );
}

#[test]
fn deleting_a_cold_failed_file_removes_its_retained_accepted_elements() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    let path = root.path().join("report.pdf");
    std::fs::write(
        &path,
        include_bytes!("../../../../project-indexer/tests/fixtures/documents/report.pdf"),
    )
    .unwrap();
    host.import(root.path()).unwrap();
    host.app.project_import_sessions.lock().unwrap().clear();
    std::fs::write(&path, b"%PDF-invalid").unwrap();
    let partial = host.import(root.path()).unwrap();
    assert_eq!(partial["failures"].as_array().unwrap().len(), 1);
    assert!(
        host.selected()
            .elements
            .iter()
            .any(|element| element.path == "report.pdf")
    );
    std::fs::remove_file(path).unwrap();
    host.import(root.path()).unwrap();
    let current = host.selected();
    assert!(
        !current
            .elements
            .iter()
            .any(|element| element.path == "report.pdf")
    );
    assert!(
        current
            .elements
            .iter()
            .any(|element| element.path == "a.rs")
    );
}

#[test]
fn healthy_changes_publish_while_broken_pdf_retains_accepted_content_with_warm_and_cold_caches() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    let pdf = include_bytes!("../../../../project-indexer/tests/fixtures/documents/report.pdf");
    std::fs::write(root.path().join("report.pdf"), pdf).unwrap();
    host.import(root.path()).unwrap();
    let original = host.selected();
    let accepted_pdf = file_id(&original, "report.pdf");
    std::fs::write(root.path().join("report.pdf"), b"%PDF-invalid").unwrap();
    for value in [2, 3] {
        if value == 3 {
            host.app.project_import_sessions.lock().unwrap().clear();
        }
        let previous = host.selected();
        std::fs::write(
            root.path().join("a.rs"),
            format!("fn a() {{ let value = {value}; }}\n"),
        )
        .unwrap();
        let result = host.import(root.path()).unwrap();
        assert_eq!(result["failures"].as_array().unwrap().len(), 1);
        assert_eq!(result["failures"][0]["path"], "report.pdf");
        let current = host.selected();
        assert_eq!(file_id(&current, "report.pdf"), accepted_pdf);
        let pdf_elements = |snapshot: &SemanticRevisionSelectionSnapshot| {
            snapshot
                .elements
                .iter()
                .filter(|element| element.path == "report.pdf")
                .map(|element| element.semantic_element_id.clone())
                .collect::<BTreeSet<_>>()
        };
        assert!(pdf_elements(&original).len() > 1);
        assert_eq!(pdf_elements(&current), pdf_elements(&original));
        assert_ne!(file_id(&current, "a.rs"), file_id(&previous, "a.rs"));
    }
    std::fs::write(root.path().join("report.pdf"), pdf).unwrap();
    assert!(
        host.import(root.path()).unwrap()["failures"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(file_id(&host.selected(), "report.pdf"), accepted_pdf);
}

#[test]
fn prepared_job_survives_its_original_call_and_requires_fresh_control_to_continue() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    let service = host.app.plugin_host_services.project_import_service();
    let context = import_context();
    let mut input = json!({"folderPath":root.path(),"projectRoot":PROJECT,"statusOnly":true});
    let pending = service
        .import_folder(input.clone(), &context, host.publisher.clone())
        .unwrap();
    context.cancellation().cancel();
    input["importId"] = pending["importId"].clone();
    input["statusOnly"] = json!(false);
    assert!(
        service
            .import_folder(input.clone(), &context, host.publisher.clone())
            .is_err()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        let state = service
            .import_folder(input.clone(), &import_context(), host.publisher.clone())
            .unwrap();
        if state["phase"] == "completed" {
            break;
        }
        assert_ne!(state["phase"], "failed");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!host.selected().elements.is_empty());
}

fn refresh_input(root: &Path, project_root: &str) -> ImportInput {
    ImportInput {
        folder_path: root.to_string_lossy().into_owned(),
        project_root: Some(project_root.into()),
        replace_paths: None,
        source: None,
    }
}

/// `(import id, status)` of every index-log entry written so far, in order.
fn index_log_entries(host: &SharedImportHost) -> Vec<(String, String)> {
    let boundary = host.publisher.boundary.lock().unwrap();
    boundary
        .logs
        .iter()
        .map(|log| {
            let text = |key: &str| log[key].as_str().unwrap().to_owned();
            (text("index_log_id"), text("status"))
        })
        .collect()
}

fn latest_index_log(host: &SharedImportHost) -> Value {
    host.publisher
        .invoke_plugin(HostPluginInvocation {
            plugin_id: "builtin.semantic".into(),
            export_id: "latest_index_log".into(),
            input: json!({"project_root":PROJECT}),
        })
        .unwrap()["index_log"]
        .clone()
}

#[test]
fn idle_refresh_rounds_write_no_index_log_rows_and_keep_a_bounded_job_registry() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    let service = host.app.plugin_host_services.project_import_service();
    let mut input = json!({"folderPath":root.path(),"projectRoot":PROJECT,"statusOnly":true});
    let first = service
        .import_folder(input.clone(), &import_context(), host.publisher.clone())
        .unwrap();
    input["importId"] = first["importId"].clone();
    input["statusOnly"] = json!(false);
    drive_import_input(&host, input.clone(), host.publisher.clone()).unwrap();
    let logged = index_log_entries(&host);
    let statuses: Vec<_> = logged.iter().map(|(_, status)| status.as_str()).collect();
    assert_eq!(statuses, ["running", "completed"]);
    let latest = latest_index_log(&host);
    host.publisher.boundary.lock().unwrap().batches.clear();

    for round in 0..RETAINED_FINISHED_JOBS + 3 {
        let refreshed = service
            .import_native_via(
                host.publisher.clone(),
                refresh_input(root.path(), PROJECT),
                true,
            )
            .ok()
            .expect("an idle refresh succeeds");
        assert_eq!(refreshed["elementsUpserted"], 0, "round {round}");
        assert!(
            service.jobs.len() <= RETAINED_FINISHED_JOBS,
            "round {round}"
        );
        if round == 0 {
            // The previous caller can still read its completion after a refresh started.
            input["statusOnly"] = json!(true);
            let handle = service
                .import_folder(input.clone(), &import_context(), host.publisher.clone())
                .unwrap();
            assert_eq!(handle["phase"], "completed");
        }
    }

    assert_eq!(
        index_log_entries(&host),
        logged,
        "idle refresh rounds must not write index-log rows"
    );
    assert_eq!(latest_index_log(&host), latest);
    assert!(host.publisher.boundary.lock().unwrap().batches.is_empty());
    assert_eq!(service.jobs.len(), RETAINED_FINISHED_JOBS);
    // Long-finished handles are dropped instead of accumulating until restart.
    assert!(
        service
            .import_folder(input, &import_context(), host.publisher.clone())
            .is_err()
    );
}

#[test]
fn a_changed_file_refresh_still_logs_running_then_completed_under_one_import() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    host.import(root.path()).unwrap();
    host.publisher.boundary.lock().unwrap().logs.clear();
    std::fs::write(root.path().join("a.rs"), "fn a() { let value = 2; }\n").unwrap();

    let refreshed = host
        .app
        .plugin_host_services
        .project_import_service()
        .import_native_via(
            host.publisher.clone(),
            refresh_input(root.path(), PROJECT),
            true,
        )
        .ok()
        .expect("refresh publishes the edit");

    assert!(refreshed["elementsUpserted"].as_u64().unwrap() > 0);
    let logged = index_log_entries(&host);
    let statuses: Vec<_> = logged.iter().map(|(_, status)| status.as_str()).collect();
    assert_eq!(statuses, ["running", "completed"]);
    assert_eq!(logged[0].0, logged[1].0);
    assert_eq!(latest_index_log(&host)["status"], "completed");
}

#[test]
fn background_refresh_rounds_keep_a_bounded_job_registry() {
    let host = SharedImportHost::new(Arc::new(LocalPersistence::in_memory().unwrap()), "client");
    let root = checkout();
    let project = root.path().canonicalize().unwrap();
    let project_root = project.to_string_lossy().into_owned();
    drive_import_input(
        &host,
        json!({"folderPath":project,"projectRoot":project_root}),
        host.publisher.clone(),
    )
    .unwrap();
    let service = host.app.plugin_host_services.project_import_service();

    // The driver's own entry point: every round starts a job for the unchanged project.
    for round in 0..RETAINED_FINISHED_JOBS + 3 {
        refresh_local_projects(&host.app).unwrap();
        assert!(
            service.jobs.len() <= RETAINED_FINISHED_JOBS,
            "round {round}"
        );
    }
    assert_eq!(service.jobs.len(), RETAINED_FINISHED_JOBS);
}

#[test]
fn html_pages_sharing_an_external_link_import_into_their_own_partitions() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let root = tempfile::tempdir().unwrap();
    let external = "https://other.example/x?q=1";
    std::fs::write(
        root.path().join("index.html"),
        format!(
            r#"<html><head><meta name="lumvise:source-url" content="https://site.example/index.html"></head>
<body><p><a href="about.html">About</a> <a href="{external}#top">Ext</a> <a href="missing.html">Gone</a></p></body></html>"#
        ),
    )
    .unwrap();
    std::fs::write(
        root.path().join("about.html"),
        format!(
            r#"<html><head><meta name="lumvise:source-url" content="https://site.example/about.html"></head>
<body><p><a href="index.html">Home</a> <a href="{external}">Same</a></p></body></html>"#
        ),
    )
    .unwrap();
    let host = SharedImportHost::new(local, "client-html");

    host.import(root.path())
        .expect("HTML pages sharing an external URL import");

    let snapshot = host.selected();
    let index = file_id(&snapshot, "index.html");
    let about = file_id(&snapshot, "about.html");
    let externals: Vec<_> = snapshot
        .elements
        .iter()
        .filter(|element| element.element_kind == "html_external_link")
        .collect();
    // One link element per distinct URL per linking page: index has the external and the
    // unsaved same-site `missing.html`; about has the external only.
    let missing = "https://site.example/missing.html";
    assert_eq!(
        externals.len(),
        3,
        "one link element per URL per linking page"
    );
    let external_of = |path: &str, url: &str| {
        externals
            .iter()
            .find(|element| element.path == path && element.name == url)
            .unwrap_or_else(|| panic!("no {url} link element in {path}"))
            .semantic_element_id
            .clone()
    };
    let index_external = external_of("index.html", external);
    let index_missing = external_of("index.html", missing);
    let about_external = external_of("about.html", external);
    let links: Vec<_> = snapshot
        .connections
        .iter()
        .map(|connection| &connection.relationship)
        .filter(|relationship| relationship.label == "links_to")
        .collect();
    let edges_from = |source: &str| {
        links
            .iter()
            .filter(|relationship| relationship.source_element_id == source)
            .map(|relationship| relationship.target_element_id.as_str())
            .collect::<BTreeSet<_>>()
    };
    // The missing same-site page keeps its link element: index links about, external, missing.
    assert_eq!(
        edges_from(&index),
        BTreeSet::from([
            about.as_str(),
            index_external.as_str(),
            index_missing.as_str()
        ]),
    );
    assert_eq!(
        edges_from(&about),
        BTreeSet::from([index.as_str(), about_external.as_str()]),
    );
}
