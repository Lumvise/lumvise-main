use lumvise_db_core::{
    CentralizedPersistence, RelationalOperation, RelationalPersistence, RelationalReadiness,
    RelationalResult, SemanticOperation, SemanticPersistence, SemanticReadiness, SemanticResult,
};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use lumvise_resource_routing::InvocationControl;
use lumvise_resource_routing::{
    auth::AuthenticatedPrincipal,
    protocol::{
        CapabilityReadinessV1, InvocationEnvelopeV1, InvocationStartV1, InvocationTerminalStatusV1,
        PROTOCOL_MAJOR, PROTOCOL_MINOR, ReadinessRequestV1, ResourceCapabilityV1, SpeechToTextV1,
        invocation_envelope_v1::Payload, invocation_start_v1::Operation,
        invocation_terminal_v1::Result as TerminalResult,
    },
};
#[cfg(feature = "assistant-e2e")]
use lumvise_resource_server::ServerNeuralConfig;
use lumvise_resource_server::dispatch::DispatchError;
use lumvise_resource_server::{
    AccessTokenValidator, PrincipalPersistenceFactory, ResourceServer, ServerResources,
    TenantAdapters, TenantOpenError,
};
use std::sync::Mutex;

struct FakeAuth;

impl AccessTokenValidator for FakeAuth {
    fn validate(
        &self,
        _: &str,
        _: &lumvise_resource_routing::InvocationControl,
    ) -> Result<AuthenticatedPrincipal, String> {
        unreachable!("this test crosses the authenticated dispatch seam directly")
    }
}

fn principal(subject: &str, tenant_id: &str) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal {
        issuer: "https://issuer.example".into(),
        subject: subject.into(),
        tenant_id: tenant_id.into(),
        scopes: BTreeSet::from(["lumvise.resources".into()]),
    }
}

fn future_deadline() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 59_000
}

fn invocation(request_id: &str, major: u32, deadline: u64) -> InvocationEnvelopeV1 {
    InvocationEnvelopeV1 {
        protocol_major: major,
        protocol_minor: 0,
        request_id: request_id.into(),
        sequence: 0,
        payload: Some(Payload::Start(InvocationStartV1 {
            deadline_unix_ms: deadline,
            client_instance_id: "desktop-a".into(),
            operation: Some(Operation::SpeechToText(SpeechToTextV1 {
                audio: vec![1, 2, 3],
                media_type: "audio/wav".into(),
                model: None,
            })),
        })),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PrincipalOpenCall {
    principal: AuthenticatedPrincipal,
    client_instance_id: String,
    deadline_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PersistenceCall {
    kind: &'static str,
    subject: String,
    deadline_unix_ms: u64,
}

struct FakePrincipalPersistenceFactory {
    open_calls: Arc<Mutex<Vec<PrincipalOpenCall>>>,
    persistence_calls: Arc<Mutex<Vec<PersistenceCall>>>,
    fail: bool,
}

impl PrincipalPersistenceFactory for FakePrincipalPersistenceFactory {
    fn open(
        &self,
        principal: &AuthenticatedPrincipal,
        client_instance_id: &str,
        control: &InvocationControl,
    ) -> Result<Arc<TenantAdapters>, TenantOpenError> {
        self.open_calls.lock().unwrap().push(PrincipalOpenCall {
            principal: principal.clone(),
            client_instance_id: client_instance_id.into(),
            deadline_unix_ms: control.deadline_unix_ms(),
        });
        if self.fail {
            return Err(TenantOpenError::Open(
                "injected principal store failure".into(),
            ));
        }
        Ok(Arc::new(fake_adapters(
            principal.subject.clone(),
            Arc::clone(&self.persistence_calls),
        )))
    }
}

struct FakeSemanticPersistence {
    subject: String,
    calls: Arc<Mutex<Vec<PersistenceCall>>>,
}

impl SemanticPersistence for FakeSemanticPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::PersistenceResult<SemanticResult> {
        assert!(matches!(operation, SemanticOperation::SemanticRevision));
        self.calls.lock().unwrap().push(PersistenceCall {
            kind: "semantic",
            subject: self.subject.clone(),
            deadline_unix_ms: control.deadline_unix_ms(),
        });
        Ok(SemanticResult::SemanticRevision {
            commit_version: if self.subject == "alice" { 11 } else { 22 },
        })
    }

    fn readiness(&self) -> lumvise_db_core::PersistenceResult<SemanticReadiness> {
        Ok(SemanticReadiness { ready: true })
    }
}

struct FakeRelationalPersistence {
    subject: String,
    calls: Arc<Mutex<Vec<PersistenceCall>>>,
}

impl RelationalPersistence for FakeRelationalPersistence {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::PersistenceResult<RelationalResult> {
        assert!(matches!(
            operation,
            RelationalOperation::GetPersistentSetting { .. }
        ));
        self.calls.lock().unwrap().push(PersistenceCall {
            kind: "relational",
            subject: self.subject.clone(),
            deadline_unix_ms: control.deadline_unix_ms(),
        });
        Ok(RelationalResult::PersistentSetting(None))
    }

    fn readiness(&self) -> lumvise_db_core::PersistenceResult<RelationalReadiness> {
        Ok(RelationalReadiness { ready: true })
    }
}

fn fake_adapters(subject: String, calls: Arc<Mutex<Vec<PersistenceCall>>>) -> TenantAdapters {
    TenantAdapters {
        semantic: Arc::new(FakeSemanticPersistence {
            subject: subject.clone(),
            calls: Arc::clone(&calls),
        }),
        relational: Arc::new(FakeRelationalPersistence { subject, calls }),
    }
}

fn deadline_in(milliseconds: u64) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + milliseconds
}

fn persistence_invocation(
    request_id: &str,
    client_instance_id: &str,
    deadline_unix_ms: u64,
    operation: Operation,
) -> InvocationEnvelopeV1 {
    InvocationEnvelopeV1 {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        request_id: request_id.into(),
        sequence: 0,
        payload: Some(Payload::Start(InvocationStartV1 {
            deadline_unix_ms,
            client_instance_id: client_instance_id.into(),
            operation: Some(operation),
        })),
    }
}

#[cfg(feature = "assistant-e2e")]
#[test]
fn assistant_e2e_internal_neural_adapters_are_ready_and_described() {
    let temporary = tempfile::tempdir().unwrap();
    let server = ResourceServer::new(
        ServerResources::internal_from_neural_configuration(
            temporary.path().to_path_buf(),
            Arc::new(FakeAuth),
            &ServerNeuralConfig::default(),
        )
        .expect("deterministic assistant-e2e adapters construct"),
    );
    let readiness = server
        .dispatcher()
        .readiness(
            &principal("alice", "tenant-a"),
            ReadinessRequestV1 {
                supported_majors: vec![PROTOCOL_MAJOR],
                supported_minors: vec![PROTOCOL_MINOR],
                deadline_unix_ms: future_deadline(),
                client_instance_id: "desktop-a".into(),
                requested_capabilities: vec![
                    ResourceCapabilityV1::LlmExecution as i32,
                    ResourceCapabilityV1::SpeechInference as i32,
                ],
            },
        )
        .expect("readiness succeeds");

    assert_eq!(
        readiness
            .capabilities
            .iter()
            .map(|entry| entry.status)
            .collect::<Vec<_>>(),
        vec![
            CapabilityReadinessV1::Ready as i32,
            CapabilityReadinessV1::Ready as i32,
        ]
    );
    assert_eq!(
        readiness
            .llm_providers
            .iter()
            .map(|descriptor| descriptor.provider_id.as_str())
            .collect::<Vec<_>>(),
        vec![lumvise_neural_core::assistant_e2e::PROVIDER_ID]
    );
    assert_eq!(readiness.speech.len(), 1);
    assert_eq!(readiness.speech[0].adapter_id, "internal-speech");
}

#[test]
fn authenticated_tenants_share_only_their_tenant_store_and_reject_bad_protocols() {
    let temporary = tempfile::tempdir().unwrap();
    let resources = ServerResources::internal(
        temporary.path().to_path_buf(),
        Arc::new(FakeAuth),
        None,
        None,
        None,
    );
    let server = ResourceServer::new(resources);
    let alice = principal("alice", "tenant-a");
    let bob = principal("bob", "tenant-a");
    let other = principal("carol", "tenant-b");

    let alice_path = server.dispatcher().tenant_database_path(&alice);
    assert_eq!(alice_path, server.dispatcher().tenant_database_path(&bob));
    assert_ne!(alice_path, server.dispatcher().tenant_database_path(&other));
    assert!(alice_path.ends_with("lumvise.db"));
    assert!(!alice_path.to_string_lossy().contains("tenant-a"));

    let readiness = server
        .dispatcher()
        .readiness(
            &alice,
            ReadinessRequestV1 {
                supported_majors: vec![PROTOCOL_MAJOR],
                supported_minors: vec![PROTOCOL_MINOR],
                deadline_unix_ms: future_deadline(),
                client_instance_id: "desktop-a".into(),
                requested_capabilities: vec![
                    ResourceCapabilityV1::GraphPersistence as i32,
                    ResourceCapabilityV1::SqlPersistence as i32,
                ],
            },
        )
        .unwrap();
    assert_eq!(readiness.tenant_id, "tenant-a");
    assert!(alice_path.exists());
    assert!(!server.dispatcher().tenant_database_path(&other).exists());

    let response = server.dispatcher().invoke(
        &alice,
        vec![invocation(
            "wrong-major",
            PROTOCOL_MAJOR + 1,
            future_deadline(),
        )],
    );
    let Payload::Terminal(terminal) = response.payload.unwrap() else {
        panic!("dispatch must return a terminal envelope")
    };
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::InvalidArgument as i32
    );
    assert_eq!(terminal.error_code.as_deref(), Some("protocol_error"));
}

#[test]
fn cancellation_and_deadline_are_terminal_before_adapter_dispatch() {
    let temporary = tempfile::tempdir().unwrap();
    let server = ResourceServer::new(ServerResources::internal(
        temporary.path().to_path_buf(),
        Arc::new(FakeAuth),
        None,
        None,
        None,
    ));
    let tenant = principal("alice", "tenant-a");

    let cancelled = invocation("cancelled", PROTOCOL_MAJOR, future_deadline());
    let mut cancel_frame = cancelled.clone();
    cancel_frame.sequence = 1;
    cancel_frame.payload = Some(Payload::Cancel(
        lumvise_resource_routing::protocol::InvocationCancelV1 {
            reason: Some("user cancelled".into()),
            client_instance_id: "desktop-a".into(),
        },
    ));
    let response = server
        .dispatcher()
        .invoke(&tenant, vec![cancelled, cancel_frame]);
    let Payload::Terminal(terminal) = response.payload.unwrap() else {
        panic!("dispatch must return a terminal envelope")
    };
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::Cancelled as i32
    );

    let response = server
        .dispatcher()
        .invoke(&tenant, vec![invocation("expired", PROTOCOL_MAJOR, 1)]);
    let Payload::Terminal(terminal) = response.payload.unwrap() else {
        panic!("dispatch must return a terminal envelope")
    };
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::DeadlineExceeded as i32
    );
}

#[test]
fn internal_server_uses_the_binary_persistence_codec_by_default() {
    let temporary = tempfile::tempdir().unwrap();
    let server = ResourceServer::new(ServerResources::internal(
        temporary.path().to_path_buf(),
        Arc::new(FakeAuth),
        None,
        None,
        None,
    ));
    let principal = principal("alice", "tenant-a");
    let operation =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::SemanticRevision)
            .unwrap();
    let response = server.dispatcher().invoke(
        &principal,
        vec![InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            request_id: "semantic-revision".into(),
            sequence: 0,
            payload: Some(Payload::Start(InvocationStartV1 {
                deadline_unix_ms: future_deadline(),
                client_instance_id: "desktop-a".into(),
                operation: Some(Operation::SemanticOperation(operation)),
            })),
        }],
    );
    let Payload::Terminal(terminal) = response.payload.unwrap() else {
        panic!("dispatch must return a terminal envelope");
    };
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::Completed as i32
    );
    let Some(TerminalResult::Semantic(result)) = terminal.result.as_ref() else {
        panic!("persistence must return its typed binary result");
    };
    assert!(matches!(
        CentralizedPersistence::decode_semantic_result(result).unwrap(),
        SemanticResult::SemanticRevision { .. }
    ));
}

#[test]
fn principal_factory_receives_identity_client_and_deadline_for_all_persistence_paths() {
    let temporary = tempfile::tempdir().unwrap();
    let open_calls = Arc::new(Mutex::new(Vec::new()));
    let persistence_calls = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(FakePrincipalPersistenceFactory {
        open_calls: Arc::clone(&open_calls),
        persistence_calls: Arc::clone(&persistence_calls),
        fail: false,
    });
    let mut resources = ServerResources::internal(
        temporary.path().to_path_buf(),
        Arc::new(FakeAuth),
        None,
        None,
        None,
    );
    resources.principal_factory = Some(factory);
    let server = ResourceServer::new(resources);
    let dispatcher = server.dispatcher();
    let alice = principal("alice", "tenant-a");
    let bob = principal("bob", "tenant-a");
    let readiness_deadline = deadline_in(50_000);
    let alice_semantic_deadline = deadline_in(51_000);
    let bob_semantic_deadline = deadline_in(52_000);
    let relational_deadline = deadline_in(53_000);

    let readiness = dispatcher
        .readiness(
            &alice,
            ReadinessRequestV1 {
                supported_majors: vec![PROTOCOL_MAJOR],
                supported_minors: vec![PROTOCOL_MINOR],
                deadline_unix_ms: readiness_deadline,
                client_instance_id: "readiness-client".into(),
                requested_capabilities: vec![
                    ResourceCapabilityV1::GraphPersistence as i32,
                    ResourceCapabilityV1::SqlPersistence as i32,
                ],
            },
        )
        .unwrap();
    assert_eq!(
        readiness
            .capabilities
            .iter()
            .map(|entry| entry.status)
            .collect::<Vec<_>>(),
        vec![
            CapabilityReadinessV1::Ready as i32,
            CapabilityReadinessV1::Ready as i32,
        ]
    );

    let semantic =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::SemanticRevision)
            .unwrap();
    let alice_response = dispatcher.invoke(
        &alice,
        vec![persistence_invocation(
            "semantic-alice",
            "semantic-client",
            alice_semantic_deadline,
            Operation::SemanticOperation(semantic.clone()),
        )],
    );
    let bob_response = dispatcher.invoke(
        &bob,
        vec![persistence_invocation(
            "semantic-bob",
            "semantic-client",
            bob_semantic_deadline,
            Operation::SemanticOperation(semantic),
        )],
    );
    assert_eq!(
        semantic_revision(alice_response),
        11,
        "Alice must receive her principal-specific persistence adapter"
    );
    assert_eq!(
        semantic_revision(bob_response),
        22,
        "Bob must not reuse Alice's adapter even with the same tenant"
    );

    let relational = CentralizedPersistence::encode_relational_operation(
        RelationalOperation::GetPersistentSetting {
            scope: "app".into(),
            key: "theme".into(),
        },
    )
    .unwrap();
    let relational_response = dispatcher.invoke(
        &alice,
        vec![persistence_invocation(
            "relational-alice",
            "relational-client",
            relational_deadline,
            Operation::RelationalOperation(relational),
        )],
    );
    assert_completed(relational_response);

    assert_eq!(
        *open_calls.lock().unwrap(),
        vec![
            PrincipalOpenCall {
                principal: alice.clone(),
                client_instance_id: "readiness-client".into(),
                deadline_unix_ms: readiness_deadline,
            },
            PrincipalOpenCall {
                principal: alice.clone(),
                client_instance_id: "semantic-client".into(),
                deadline_unix_ms: alice_semantic_deadline,
            },
            PrincipalOpenCall {
                principal: bob.clone(),
                client_instance_id: "semantic-client".into(),
                deadline_unix_ms: bob_semantic_deadline,
            },
            PrincipalOpenCall {
                principal: alice,
                client_instance_id: "relational-client".into(),
                deadline_unix_ms: relational_deadline,
            },
        ]
    );
    assert_eq!(
        *persistence_calls.lock().unwrap(),
        vec![
            PersistenceCall {
                kind: "semantic",
                subject: "alice".into(),
                deadline_unix_ms: alice_semantic_deadline,
            },
            PersistenceCall {
                kind: "semantic",
                subject: "bob".into(),
                deadline_unix_ms: bob_semantic_deadline,
            },
            PersistenceCall {
                kind: "relational",
                subject: "alice".into(),
                deadline_unix_ms: relational_deadline,
            },
        ]
    );
}

#[test]
fn principal_factory_failure_never_falls_back_to_tenant_local_persistence() {
    let temporary = tempfile::tempdir().unwrap();
    let factory = Arc::new(FakePrincipalPersistenceFactory {
        open_calls: Arc::new(Mutex::new(Vec::new())),
        persistence_calls: Arc::new(Mutex::new(Vec::new())),
        fail: true,
    });
    let mut resources = ServerResources::internal(
        temporary.path().to_path_buf(),
        Arc::new(FakeAuth),
        None,
        None,
        None,
    );
    resources.principal_factory = Some(factory);
    let server = ResourceServer::new(resources);
    let dispatcher = server.dispatcher();
    let alice = principal("alice", "tenant-a");
    let local_path = dispatcher.tenant_database_path(&alice);

    let readiness_error = dispatcher.readiness(
        &alice,
        ReadinessRequestV1 {
            supported_majors: vec![PROTOCOL_MAJOR],
            supported_minors: vec![PROTOCOL_MINOR],
            deadline_unix_ms: future_deadline(),
            client_instance_id: "desktop-a".into(),
            requested_capabilities: vec![ResourceCapabilityV1::GraphPersistence as i32],
        },
    );
    assert!(matches!(
        readiness_error,
        Err(DispatchError::Tenant(TenantOpenError::Open(message)))
            if message == "injected principal store failure"
    ));
    assert!(!local_path.exists());

    let operation =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::SemanticRevision)
            .unwrap();
    let response = dispatcher.invoke(
        &alice,
        vec![persistence_invocation(
            "factory-error",
            "desktop-a",
            future_deadline(),
            Operation::SemanticOperation(operation),
        )],
    );
    let Payload::Terminal(terminal) = response.payload.unwrap() else {
        panic!("dispatch must return a terminal envelope");
    };
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::Unavailable as i32
    );
    assert_eq!(terminal.error_code.as_deref(), Some("tenant_unavailable"));
    assert!(!local_path.exists());
}

fn semantic_revision(response: InvocationEnvelopeV1) -> i64 {
    let Payload::Terminal(terminal) = response.payload.unwrap() else {
        panic!("dispatch must return a terminal envelope");
    };
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::Completed as i32
    );
    let Some(TerminalResult::Semantic(result)) = terminal.result.as_ref() else {
        panic!("semantic operation must return its typed result");
    };
    match CentralizedPersistence::decode_semantic_result(result).unwrap() {
        SemanticResult::SemanticRevision { commit_version } => commit_version,
        other => panic!("unexpected semantic result: {other:?}"),
    }
}

fn assert_completed(response: InvocationEnvelopeV1) {
    let Payload::Terminal(terminal) = response.payload.unwrap() else {
        panic!("dispatch must return a terminal envelope");
    };
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::Completed as i32
    );
    assert!(matches!(
        terminal.result,
        Some(TerminalResult::Relational(_))
    ));
}

/// Regression test: OidcAccessTokenValidator::discover uses reqwest::blocking,
/// which creates/drops its own Tokio runtime internally.  The binary fix wraps
/// discovery in spawn_blocking to avoid the "Cannot drop a runtime in a context
/// where blocking is not allowed" panic.  This test reproduces the same context
/// (a #[tokio::test]) and verifies that spawn_blocking + a reqwest::blocking
/// HTTPS request does not panic.
#[tokio::test]
async fn reqwest_blocking_via_spawn_blocking_does_not_panic_in_async_context()
-> Result<(), Box<dyn std::error::Error>> {
    // This is the exact mechanism that caused the binary to panic:
    // reqwest::blocking inside a #[tokio::main] context.  Wrapping it in
    // spawn_blocking moves the synchronous client to a blocking thread
    // where its inner runtime lifecycle is safe.
    let status = tokio::task::spawn_blocking(|| {
        let client = reqwest::blocking::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("blocking reqwest client builds inside spawn_blocking");
        client
            .head("https://google.com")
            .send()
            .ok()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    })
    .await
    .map_err(|join_error| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("reqwest::blocking panicked: {join_error}"),
        )) as Box<dyn std::error::Error>
    })?;

    // The request itself may succeed or fail depending on network, but
    // what matters is that it did NOT panic with "Cannot drop a runtime
    // in a context where blocking is not allowed".
    if !status {
        eprintln!(
            "note: reqwest blocking request to google.com was not successful (network?), but did not panic"
        );
    }
    Ok(())
}
