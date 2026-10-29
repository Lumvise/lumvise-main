//! Real shared scanner -> Semantic -> public native persistence; only publication faults are faked.
use super::*;
use lumvise_db_core::{
    LocalPersistence, RelationalPersistence, SemanticPersistence, SemanticRevisionSelectionSnapshot,
};
use lumvise_frontend_core::{AppSettingsPatch, FrontendCore};
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, HostPluginInvocation,
    HostPluginInvoker, PluginInvocationClass, PluginInvocationContext,
};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_plugin_semantic::SemanticPlugin;
use std::sync::Mutex;

const PROJECT: &str = "/shared-folder-import";

struct SemanticImportBoundary {
    broker: crate::AppCoreHostCapabilityBroker,
    client: String,
    batches: Vec<Value>,
    lose_next_commit_reply: bool,
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
            boundary.batches.push(request.input.clone());
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
    publisher: SemanticImportPublisher,
}

impl SharedImportHost {
    fn new(local: Arc<LocalPersistence>, client: &str) -> Self {
        let semantic: Arc<dyn SemanticPersistence> = local.clone();
        let relational: Arc<dyn RelationalPersistence> = local.clone();
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
            lose_next_commit_reply: false,
        };
        Self {
            app,
            local,
            publisher: SemanticImportPublisher {
                boundary: Mutex::new(boundary),
            },
        }
    }

    fn import(&self, root: &Path) -> Result<Value, HostCapabilityError> {
        self.app
            .plugin_host_services
            .project_import_service()
            .import_folder(
                json!({"folderPath":root,"projectRoot":PROJECT}),
                &import_context(),
                &self.publisher,
            )
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

fn import_context() -> PluginInvocationContext {
    PluginInvocationContext::new(
        "shared-folder-import",
        "test-mcp-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.canvas", "import_folder")
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
    for context in [cancelled, expired] {
        assert!(
            host.app
                .plugin_host_services
                .project_import_service()
                .import_folder(json!({"folderPath":"/missing"}), &context, &host.publisher)
                .is_err()
        );
    }
    assert!(host.app.project_import_sessions.lock().unwrap().is_empty());
    assert!(host.publisher.boundary.lock().unwrap().batches.is_empty());
}

struct MalformedReceiptPublisher<'a>(&'a SemanticImportPublisher);

impl HostPluginInvoker for MalformedReceiptPublisher<'_> {
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
    host.app
        .plugin_host_services
        .project_import_service()
        .import_folder(
            json!({"folderPath":root,"projectRoot":PROJECT}),
            &import_context(),
            &MalformedReceiptPublisher(&host.publisher),
        )
        .unwrap_err()
}
