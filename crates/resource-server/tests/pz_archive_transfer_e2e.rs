use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use lumvise_db_core::{
    CentralizedPersistence, LocalPersistence, RelationalPersistence, SemanticArtifact,
    SemanticElement, SemanticOperation, SemanticPersistence, SemanticReadiness,
    SemanticRelationship, SemanticResult,
};
use lumvise_resource_routing::{
    InvocationControl, ResourceInvocationClient, TransportError,
    auth::AuthenticatedPrincipal,
    protocol::{
        InvocationEnvelopeV1, InvocationStartV1, InvocationTerminalStatusV1, InvocationTerminalV1,
        PROTOCOL_MAJOR, PROTOCOL_MINOR, PZ_ARCHIVE_CHUNK_TYPE, ReadinessRequestV1,
        ReadinessResponseV1, SemanticOperationV1, TypedBinaryChunkV1,
        invocation_envelope_v1::Payload, invocation_start_v1::Operation,
    },
};
use lumvise_resource_server::{
    AccessTokenValidator, PrincipalPersistenceFactory, ResourceServer, ServerResources,
    TenantAdapters, TenantOpenError,
};
use serde_json::json;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponseEdit {
    Keep,
    DropArchive,
    WrongArchiveType,
    TruncateArchive,
    Cancel,
}

struct TransferTrace {
    requests: Vec<Vec<InvocationEnvelopeV1>>,
    server_paths: Arc<Mutex<Vec<String>>>,
}

impl Default for TransferTrace {
    fn default() -> Self {
        Self {
            requests: Vec::new(),
            server_paths: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

struct NamedFakeResourceInvocationClient {
    server: Arc<ResourceServer>,
    principal: AuthenticatedPrincipal,
    response_edit: ResponseEdit,
    trace: Arc<Mutex<TransferTrace>>,
}

impl ResourceInvocationClient for NamedFakeResourceInvocationClient {
    fn readiness(
        &self,
        request: &ReadinessRequestV1,
        _: &InvocationControl,
    ) -> Result<ReadinessResponseV1, TransportError> {
        self.server
            .dispatcher()
            .readiness(&self.principal, request.clone())
            .map_err(|_| TransportError::UnexpectedResponse)
    }

    fn invoke(
        &self,
        envelopes: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> Result<Vec<InvocationEnvelopeV1>, TransportError> {
        self.trace.lock().unwrap().requests.push(envelopes.to_vec());
        if self.response_edit == ResponseEdit::Cancel {
            control.cancel();
        }
        let mut response = self
            .server
            .dispatcher()
            .invoke_envelopes(&self.principal, envelopes.to_vec());
        match self.response_edit {
            ResponseEdit::DropArchive => {
                response.retain(|frame| !matches!(frame.payload, Some(Payload::BinaryChunk(_))))
            }
            ResponseEdit::WrongArchiveType => {
                if let Some(Payload::BinaryChunk(chunk)) =
                    response.iter_mut().find_map(|frame| frame.payload.as_mut())
                {
                    chunk.type_name = "wrong.type".into();
                }
            }
            ResponseEdit::TruncateArchive => {
                if let Some(Payload::BinaryChunk(chunk)) =
                    response.iter_mut().find_map(|frame| frame.payload.as_mut())
                {
                    chunk.bytes.truncate(chunk.bytes.len() / 2);
                }
            }
            ResponseEdit::Keep | ResponseEdit::Cancel => {}
        }
        Ok(response)
    }

    fn cancel(
        &self,
        _: &InvocationEnvelopeV1,
        _: &InvocationControl,
    ) -> Result<InvocationTerminalV1, TransportError> {
        Err(TransportError::UnexpectedResponse)
    }
}

struct NamedFakePrincipalPersistenceFactory {
    persistence: Arc<LocalPersistence>,
    server_paths: Arc<Mutex<Vec<String>>>,
}

impl PrincipalPersistenceFactory for NamedFakePrincipalPersistenceFactory {
    fn open(
        &self,
        _: &AuthenticatedPrincipal,
        _: &str,
        _: &InvocationControl,
    ) -> Result<Arc<TenantAdapters>, TenantOpenError> {
        let semantic: Arc<dyn SemanticPersistence> = Arc::new(RecordingLocalSemanticPersistence {
            persistence: Arc::clone(&self.persistence),
            server_paths: Arc::clone(&self.server_paths),
        });
        let relational: Arc<dyn RelationalPersistence> = Arc::clone(&self.persistence) as _;
        Ok(Arc::new(TenantAdapters {
            semantic,
            relational,
        }))
    }
}

struct RecordingLocalSemanticPersistence {
    persistence: Arc<LocalPersistence>,
    server_paths: Arc<Mutex<Vec<String>>>,
}

impl SemanticPersistence for RecordingLocalSemanticPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::PersistenceResult<SemanticResult> {
        match &operation {
            SemanticOperation::CreatePzSnapshot { output_path, .. }
            | SemanticOperation::ImportPzSnapshot {
                input_path: output_path,
                ..
            } => self.server_paths.lock().unwrap().push(output_path.clone()),
            _ => {}
        }
        SemanticPersistence::execute(self.persistence.as_ref(), operation, control)
    }

    fn readiness(&self) -> lumvise_db_core::PersistenceResult<SemanticReadiness> {
        SemanticPersistence::readiness(self.persistence.as_ref())
    }
}

struct NamedFakeAccessTokenValidator;

impl AccessTokenValidator for NamedFakeAccessTokenValidator {
    fn validate(&self, _: &str, _: &InvocationControl) -> Result<AuthenticatedPrincipal, String> {
        Ok(principal())
    }
}

fn principal() -> AuthenticatedPrincipal {
    AuthenticatedPrincipal {
        issuer: "https://issuer.example".into(),
        subject: "alice".into(),
        tenant_id: "tenant-a".into(),
        scopes: Default::default(),
    }
}

fn setup(
    response_edit: ResponseEdit,
) -> (
    CentralizedPersistence,
    Arc<Mutex<TransferTrace>>,
    Arc<LocalPersistence>,
) {
    let directory = tempfile::tempdir().unwrap();
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    seed_project(&persistence);
    let local_persistence = Arc::clone(&persistence);
    let trace = Arc::new(Mutex::new(TransferTrace::default()));
    let mut resources = ServerResources::internal(
        directory.path().join("server-data"),
        Arc::new(NamedFakeAccessTokenValidator),
        None,
        None,
        None,
    );
    resources.principal_factory = Some(Arc::new(NamedFakePrincipalPersistenceFactory {
        persistence,
        server_paths: Arc::clone(&trace.lock().unwrap().server_paths),
    }));
    let server = Arc::new(ResourceServer::new(resources));
    let client: Arc<dyn ResourceInvocationClient> = Arc::new(NamedFakeResourceInvocationClient {
        server,
        principal: principal(),
        response_edit,
        trace: Arc::clone(&trace),
    });
    (
        CentralizedPersistence::new(client, "desktop-test"),
        trace,
        local_persistence,
    )
}

#[test]
fn centralized_pz_export_import_transfers_bytes_and_never_sends_desktop_paths() {
    let (central, trace, local_persistence) = setup(ResponseEdit::Keep);
    let temporary = tempfile::tempdir().unwrap();
    let export_path = temporary.path().join("desktop-source.pz");
    let import_path = temporary.path().join("desktop-input.pz");
    let export = SemanticPersistence::execute(
        &central,
        SemanticOperation::CreatePzSnapshot {
            project_root: "/source".into(),
            output_path: export_path.to_string_lossy().into_owned(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    let SemanticResult::PzSnapshot(metadata) = export else {
        panic!("expected PZ snapshot metadata");
    };
    assert_eq!(metadata.output_path, export_path);
    assert_eq!(
        std::fs::metadata(&export_path).unwrap().len(),
        metadata.output_bytes
    );
    std::fs::copy(&export_path, &import_path).unwrap();

    let imported = SemanticPersistence::execute(
        &central,
        SemanticOperation::ImportPzSnapshot {
            project_root: "/target".into(),
            input_path: import_path.to_string_lossy().into_owned(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    assert!(matches!(imported, SemanticResult::PzImport(_)));
    assert_eq!(read_elements(local_persistence.as_ref(), "/target"), 2);
    assert!(matches!(
        SemanticPersistence::execute(
            local_persistence.as_ref(),
            SemanticOperation::Artifact {
                artifact_id: "guide".into()
            },
            &InvocationControl::sixty_seconds(),
        ),
        Ok(SemanticResult::Artifact(Some(_)))
    ));
    assert!(matches!(
        SemanticPersistence::execute(
            local_persistence.as_ref(),
            SemanticOperation::ArtifactBlobGet { content_ref: "guide-blob".into() },
            &InvocationControl::sixty_seconds(),
        ),
        Ok(SemanticResult::ArtifactBlob(Some(blob))) if blob.content == b"attached image"
    ));

    let requests = trace.lock().unwrap().requests.clone();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let Some(Payload::Start(start)) = request.first().and_then(|frame| frame.payload.as_ref())
        else {
            panic!("expected start frame");
        };
        let Some(Operation::SemanticOperation(operation)) = &start.operation else {
            panic!("expected semantic operation");
        };
        let decoded = CentralizedPersistence::decode_semantic_operation(operation).unwrap();
        match decoded {
            SemanticOperation::CreatePzSnapshot { output_path, .. } => {
                assert!(output_path.is_empty())
            }
            SemanticOperation::ImportPzSnapshot { input_path, .. } => {
                assert!(input_path.is_empty())
            }
            other => panic!("unexpected remote archive op: {other:?}"),
        }
        assert!(!format!("{request:?}").contains(temporary.path().to_str().unwrap()));
    }
    let server_paths = trace.lock().unwrap().server_paths.lock().unwrap().clone();
    assert_eq!(server_paths.len(), 2);
    let server_temp_root = std::env::temp_dir().to_string_lossy().into_owned();
    let desktop_paths = [
        export_path.to_string_lossy().into_owned(),
        import_path.to_string_lossy().into_owned(),
    ];
    assert!(server_paths.iter().all(|path| {
        path.starts_with(&server_temp_root)
            && !path.starts_with(temporary.path().to_str().unwrap())
            && !desktop_paths.contains(path)
    }));
}

#[test]
fn malformed_or_legacy_archive_requests_are_rejected_without_accepting_paths() {
    let literal_path = "/desktop/private/source.pz";
    let op =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::ImportPzSnapshot {
            project_root: "/target".into(),
            input_path: literal_path.into(),
        })
        .unwrap();
    let server = server_from_trace_setup();
    for (request_id, major, operation, chunk) in [
        ("literal-path", PROTOCOL_MAJOR, op.clone(), None),
        ("missing-chunk", PROTOCOL_MAJOR, blank_import_op(), None),
        (
            "wrong-chunk",
            PROTOCOL_MAJOR,
            blank_import_op(),
            Some(binary_chunk("wrong.type", b"x", true)),
        ),
        (
            "truncated-chunk",
            PROTOCOL_MAJOR,
            blank_import_op(),
            Some(binary_chunk(PZ_ARCHIVE_CHUNK_TYPE, b"partial", false)),
        ),
        (
            "old-major",
            3,
            blank_import_op(),
            Some(binary_chunk(PZ_ARCHIVE_CHUNK_TYPE, b"x", true)),
        ),
    ] {
        let result = dispatch_raw(&server, request_id, major, operation, chunk);
        let Some(Payload::Terminal(terminal)) =
            result.last().and_then(|frame| frame.payload.as_ref())
        else {
            panic!("expected terminal response");
        };
        assert_ne!(
            terminal.status,
            InvocationTerminalStatusV1::Completed as i32,
            "{request_id}"
        );
    }
}

#[test]
fn failed_export_validation_never_replaces_existing_desktop_output() {
    for response_edit in [
        ResponseEdit::DropArchive,
        ResponseEdit::WrongArchiveType,
        ResponseEdit::TruncateArchive,
    ] {
        let (central, _, _) = setup(response_edit);
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("existing.pz");
        std::fs::write(&destination, b"keep existing contents").unwrap();
        let result = SemanticPersistence::execute(
            &central,
            SemanticOperation::CreatePzSnapshot {
                project_root: "/source".into(),
                output_path: destination.to_string_lossy().into_owned(),
            },
            &InvocationControl::sixty_seconds(),
        );
        assert!(result.is_err(), "{response_edit:?} must be rejected");
        assert_eq!(
            std::fs::read(destination).unwrap(),
            b"keep existing contents"
        );
    }
}

#[test]
fn cancelled_and_expired_archive_exports_leave_existing_output_untouched() {
    let (central, _, _) = setup(ResponseEdit::Cancel);
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("existing.pz");
    std::fs::write(&destination, b"keep existing contents").unwrap();
    let control = InvocationControl::sixty_seconds();
    assert!(
        SemanticPersistence::execute(
            &central,
            SemanticOperation::CreatePzSnapshot {
                project_root: "/source".into(),
                output_path: destination.to_string_lossy().into_owned(),
            },
            &control,
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"keep existing contents"
    );

    let expired = InvocationControl::from_deadline_unix_ms(1);
    assert!(
        SemanticPersistence::execute(
            &central,
            SemanticOperation::CreatePzSnapshot {
                project_root: "/source".into(),
                output_path: destination.to_string_lossy().into_owned(),
            },
            &expired,
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(destination).unwrap(),
        b"keep existing contents"
    );
}

fn seed_project(persistence: &LocalPersistence) {
    let control = InvocationControl::sixty_seconds();
    SemanticPersistence::execute(
        &*persistence,
        SemanticOperation::SyncStructure {
            project_root: "/source".into(),
            elements: vec![element("a"), element("b")],
            relationships: vec![SemanticRelationship {
                project_root: "/source".into(),
                source_element_id: "a".into(),
                target_element_id: "b".into(),
                relationship_kind: "calls".into(),
                label: "calls".into(),
                metadata: json!({}),
            }],
        },
        &control,
    )
    .unwrap();
    SemanticPersistence::execute(
        &*persistence,
        SemanticOperation::UpsertArtifact {
            artifact: SemanticArtifact {
                artifact_id: "guide".into(),
                semantic_element_id: "a".into(),
                artifact_kind: "definition".into(),
                title: "Guide".into(),
                content_ref: None,
                content: Some("guide text".into()),
                searchable_text: None,
                content_size_bytes: None,
                dependencies: vec![],
                metadata: json!({}),
            },
            media_type: "text/markdown".into(),
        },
        &control,
    )
    .unwrap();
    SemanticPersistence::execute(
        &*persistence,
        SemanticOperation::ArtifactBlobPut {
            content_ref: "guide-blob".into(),
            artifact_id: "guide".into(),
            media_type: "image/png".into(),
            content: b"attached image".to_vec(),
        },
        &control,
    )
    .unwrap();
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/source".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: format!("src/{id}.rs"),
        element_kind: "file".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"origin": id}),
    }
}

fn read_elements(persistence: &LocalPersistence, project_root: &str) -> usize {
    let result = SemanticPersistence::execute(
        persistence,
        SemanticOperation::ElementsByIdsIncludingInactive {
            project_root: project_root.into(),
            semantic_element_ids: ["a".into(), "b".into()].into(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    match result {
        SemanticResult::Elements(elements) => elements.len(),
        other => panic!("unexpected: {other:?}"),
    }
}

fn server_from_trace_setup() -> ResourceServer {
    let directory = tempfile::tempdir().unwrap();
    let mut resources = ServerResources::internal(
        directory.path().join("data"),
        Arc::new(NamedFakeAccessTokenValidator),
        None,
        None,
        None,
    );
    resources.principal_factory = None;
    ResourceServer::new(resources)
}

fn blank_import_op() -> SemanticOperationV1 {
    CentralizedPersistence::encode_semantic_operation(SemanticOperation::ImportPzSnapshot {
        project_root: "/target".into(),
        input_path: String::new(),
    })
    .unwrap()
}

fn binary_chunk(type_name: &str, bytes: &[u8], final_chunk: bool) -> TypedBinaryChunkV1 {
    TypedBinaryChunkV1 {
        type_name: type_name.into(),
        bytes: bytes.into(),
        metadata_json: None,
        final_chunk,
    }
}

fn dispatch_raw(
    server: &ResourceServer,
    request_id: &str,
    major: u32,
    operation: SemanticOperationV1,
    chunk: Option<TypedBinaryChunkV1>,
) -> Vec<InvocationEnvelopeV1> {
    let mut frames = vec![InvocationEnvelopeV1 {
        protocol_major: major,
        protocol_minor: PROTOCOL_MINOR,
        request_id: request_id.into(),
        sequence: 0,
        payload: Some(Payload::Start(InvocationStartV1 {
            deadline_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                + 30_000,
            client_instance_id: "desktop-test".into(),
            operation: Some(Operation::SemanticOperation(operation)),
        })),
    }];
    if let Some(chunk) = chunk {
        frames.push(InvocationEnvelopeV1 {
            protocol_major: major,
            protocol_minor: PROTOCOL_MINOR,
            request_id: request_id.into(),
            sequence: 1,
            payload: Some(Payload::BinaryChunk(chunk)),
        });
    }
    server.dispatcher().invoke_envelopes(&principal(), frames)
}
