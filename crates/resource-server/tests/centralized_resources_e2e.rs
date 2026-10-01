use lumvise_db_core::{CentralizedPersistence, SemanticOperation, SemanticResult};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use lumvise_resource_routing::{
    auth::AuthenticatedPrincipal,
    protocol::{
        CapabilityReadinessV1, InvocationEnvelopeV1, InvocationStartV1, InvocationTerminalStatusV1,
        PROTOCOL_MAJOR, PROTOCOL_MINOR, ReadinessRequestV1, ResourceCapabilityV1, SpeechToTextV1,
        invocation_envelope_v1::Payload, invocation_start_v1::Operation,
        invocation_terminal_v1::Result as TerminalResult,
    },
};
use lumvise_resource_server::{
    AccessTokenValidator, ResourceServer, ServerNeuralConfig, ServerResources,
};

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
