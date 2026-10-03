use std::{
    collections::BTreeSet,
    io,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use lumvise_db_core::{
    CentralizedPersistence, DbError, RelationalOperation, RelationalPersistence,
    RelationalReadiness, RelationalResult, SemanticOperation, SemanticPersistence,
    SemanticReadiness, SemanticResult,
};
use lumvise_resource_routing::{
    InvocationControl,
    auth::AuthenticatedPrincipal,
    protocol::{
        InvocationEnvelopeV1, InvocationStartV1, InvocationTerminalStatusV1, PROTOCOL_MAJOR,
        PROTOCOL_MINOR, invocation_envelope_v1::Payload, invocation_start_v1::Operation,
    },
};
use lumvise_resource_server::{
    AccessTokenValidator, PrincipalPersistenceFactory, ResourceServer, ServerResources,
    TenantAdapters, TenantOpenError,
};

struct NamedFakeAccessTokenValidator;

impl AccessTokenValidator for NamedFakeAccessTokenValidator {
    fn validate(&self, _: &str, _: &InvocationControl) -> Result<AuthenticatedPrincipal, String> {
        unreachable!("the test invokes the public dispatcher with a verified principal")
    }
}

#[derive(Clone, Copy)]
enum AdapterOutcome {
    UnresolvedCommit,
    TimedOut,
    InvalidValue,
    Success,
}

struct NamedFakePrincipalPersistenceFactory {
    outcome: AdapterOutcome,
}

impl PrincipalPersistenceFactory for NamedFakePrincipalPersistenceFactory {
    fn open(
        &self,
        _: &AuthenticatedPrincipal,
        _: &str,
        _: &InvocationControl,
    ) -> Result<Arc<TenantAdapters>, TenantOpenError> {
        Ok(Arc::new(TenantAdapters {
            semantic: Arc::new(NamedFakeSemanticPersistence {
                outcome: self.outcome,
            }),
            relational: Arc::new(NamedFakeRelationalPersistence {
                outcome: self.outcome,
            }),
        }))
    }
}

struct NamedFakeSemanticPersistence {
    outcome: AdapterOutcome,
}

impl SemanticPersistence for NamedFakeSemanticPersistence {
    fn execute(
        &self,
        _: SemanticOperation,
        _: &InvocationControl,
    ) -> lumvise_db_core::PersistenceResult<SemanticResult> {
        match self.outcome {
            AdapterOutcome::Success => Ok(SemanticResult::SemanticRevision { commit_version: 19 }),
            outcome => Err(adapter_error(outcome)),
        }
    }

    fn readiness(&self) -> lumvise_db_core::PersistenceResult<SemanticReadiness> {
        Ok(SemanticReadiness { ready: true })
    }
}

struct NamedFakeRelationalPersistence {
    outcome: AdapterOutcome,
}

impl RelationalPersistence for NamedFakeRelationalPersistence {
    fn execute(
        &self,
        _: RelationalOperation,
        _: &InvocationControl,
    ) -> lumvise_db_core::PersistenceResult<RelationalResult> {
        match self.outcome {
            AdapterOutcome::Success => Ok(RelationalResult::PersistentSetting(None)),
            outcome => Err(adapter_error(outcome)),
        }
    }

    fn readiness(&self) -> lumvise_db_core::PersistenceResult<RelationalReadiness> {
        Ok(RelationalReadiness { ready: true })
    }
}

fn adapter_error(outcome: AdapterOutcome) -> DbError {
    match outcome {
        AdapterOutcome::UnresolvedCommit => DbError::unresolved_commit(23, "indeterminate"),
        AdapterOutcome::TimedOut => {
            DbError::Io(io::Error::new(io::ErrorKind::TimedOut, "adapter timed out"))
        }
        AdapterOutcome::InvalidValue => {
            DbError::invalid_value("adapter-private-detail", "a valid semantic operation")
        }
        AdapterOutcome::Success => unreachable!("success has no adapter error"),
    }
}

fn setup(outcome: AdapterOutcome) -> ResourceServer {
    let temporary = tempfile::tempdir().unwrap();
    let mut resources = ServerResources::internal(
        temporary.path().join("server-data"),
        Arc::new(NamedFakeAccessTokenValidator),
        None,
        None,
        None,
    );
    resources.principal_factory = Some(Arc::new(NamedFakePrincipalPersistenceFactory { outcome }));
    ResourceServer::new(resources)
}

fn principal() -> AuthenticatedPrincipal {
    AuthenticatedPrincipal {
        issuer: "https://issuer.example".into(),
        subject: "subject-1".into(),
        tenant_id: "tenant-1".into(),
        scopes: BTreeSet::from(["lumvise.resources".into()]),
    }
}

fn future_deadline() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 30_000
}

fn invoke_on(
    server: &ResourceServer,
    operation: Operation,
) -> (
    String,
    lumvise_resource_routing::protocol::InvocationTerminalV1,
) {
    let request_id = "caller-request-91".to_string();
    let response = server.dispatcher().invoke_envelopes(
        &principal(),
        vec![InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            request_id: request_id.clone(),
            sequence: 0,
            payload: Some(Payload::Start(InvocationStartV1 {
                deadline_unix_ms: future_deadline(),
                client_instance_id: "desktop-test".into(),
                operation: Some(operation),
            })),
        }],
    );
    let envelope = response
        .into_iter()
        .next()
        .expect("terminal response exists");
    assert_eq!(envelope.request_id, request_id);
    let Some(Payload::Terminal(terminal)) = envelope.payload else {
        panic!("dispatcher response must be terminal")
    };
    (request_id, terminal)
}

#[test]
fn unresolved_commit_preserves_unknown_outcome_and_request_id_on_both_persistence_paths() {
    let server = setup(AdapterOutcome::UnresolvedCommit);
    let semantic =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::SemanticRevision)
            .unwrap();
    let (_, semantic_terminal) = invoke_on(&server, Operation::SemanticOperation(semantic));
    assert_error_terminal(
        &semantic_terminal,
        InvocationTerminalStatusV1::Failed,
        true,
        false,
        Some("commit_outcome_unknown"),
    );

    let relational = CentralizedPersistence::encode_relational_operation(
        RelationalOperation::GetPersistentSetting {
            scope: "app".into(),
            key: "theme".into(),
        },
    )
    .unwrap();
    let (_, relational_terminal) = invoke_on(&server, Operation::RelationalOperation(relational));
    assert_error_terminal(
        &relational_terminal,
        InvocationTerminalStatusV1::Failed,
        true,
        false,
        Some("commit_outcome_unknown"),
    );
}

#[test]
fn adapter_timeout_is_deadline_exceeded_without_retry_on_both_persistence_paths() {
    let server = setup(AdapterOutcome::TimedOut);
    let semantic =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::SemanticRevision)
            .unwrap();
    let (_, semantic_terminal) = invoke_on(&server, Operation::SemanticOperation(semantic));
    assert_error_terminal(
        &semantic_terminal,
        InvocationTerminalStatusV1::DeadlineExceeded,
        true,
        false,
        Some("persistence_failed"),
    );

    let relational = CentralizedPersistence::encode_relational_operation(
        RelationalOperation::GetPersistentSetting {
            scope: "app".into(),
            key: "theme".into(),
        },
    )
    .unwrap();
    let (_, relational_terminal) = invoke_on(&server, Operation::RelationalOperation(relational));
    assert_error_terminal(
        &relational_terminal,
        InvocationTerminalStatusV1::DeadlineExceeded,
        true,
        false,
        Some("persistence_failed"),
    );
}

#[test]
fn invalid_value_is_failed_without_unknown_outcome() {
    let server = setup(AdapterOutcome::InvalidValue);
    let semantic =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::SemanticRevision)
            .unwrap();
    let (_, terminal) = invoke_on(&server, Operation::SemanticOperation(semantic));
    assert_error_terminal(
        &terminal,
        InvocationTerminalStatusV1::Failed,
        false,
        false,
        Some("persistence_failed"),
    );
}

#[test]
fn successful_semantic_and_relational_results_keep_their_encoded_values() {
    let server = setup(AdapterOutcome::Success);
    let semantic =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::SemanticRevision)
            .unwrap();
    let (_, terminal) = invoke_on(&server, Operation::SemanticOperation(semantic));
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::Completed as i32
    );
    let Some(lumvise_resource_routing::protocol::invocation_terminal_v1::Result::Semantic(result)) =
        terminal.result
    else {
        panic!("semantic result must retain its typed payload")
    };
    assert!(matches!(
        CentralizedPersistence::decode_semantic_result(&result).unwrap(),
        SemanticResult::SemanticRevision { commit_version: 19 }
    ));

    let relational = CentralizedPersistence::encode_relational_operation(
        RelationalOperation::GetPersistentSetting {
            scope: "app".into(),
            key: "theme".into(),
        },
    )
    .unwrap();
    let (_, terminal) = invoke_on(&server, Operation::RelationalOperation(relational));
    assert_eq!(
        terminal.status,
        InvocationTerminalStatusV1::Completed as i32
    );
    let Some(lumvise_resource_routing::protocol::invocation_terminal_v1::Result::Relational(
        result,
    )) = terminal.result
    else {
        panic!("relational result must retain its typed payload")
    };
    assert!(matches!(
        CentralizedPersistence::decode_relational_result(&result).unwrap(),
        RelationalResult::PersistentSetting(None)
    ));
}

fn assert_error_terminal(
    terminal: &lumvise_resource_routing::protocol::InvocationTerminalV1,
    expected_status: InvocationTerminalStatusV1,
    outcome_unknown: bool,
    retryable: bool,
    expected_code: Option<&str>,
) {
    assert_eq!(terminal.status, expected_status as i32);
    assert_eq!(terminal.outcome_unknown, outcome_unknown);
    assert_eq!(terminal.retryable, retryable);
    assert_eq!(terminal.error_code.as_deref(), expected_code);
    assert!(terminal.result.is_none());
}
