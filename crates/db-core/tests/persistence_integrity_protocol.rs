//! Public binary round trips and caller-deadline propagation for the coordinated major-five change.
use lumvise_db_core::{
    CentralizedPersistence, SemanticOperation, SemanticPersistence, SemanticRecordRef,
    SemanticResult, SemanticRevisionBody, SemanticRevisionElementScope,
    SemanticRevisionRecoveryRequest, SemanticRevisionResolution, SemanticRevisionUnavailable,
};
use lumvise_resource_routing::protocol::{
    InvocationEnvelopeV1, InvocationTerminalV1, ReadinessRequestV1, ReadinessResponseV1,
    invocation_envelope_v1::Payload, invocation_start_v1::Operation,
};
use lumvise_resource_routing::{InvocationControl, ResourceInvocationClient, TransportError};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn recovery_request() -> SemanticRevisionRecoveryRequest {
    SemanticRevisionRecoveryRequest {
        operation_id: "recover-雪".into(),
        project_root: "/source".into(),
        project_id: "lineage".into(),
        expected_commit_version: i64::MAX - 10,
        provider_instance_id: "source-parser".into(),
        missing_records: vec![SemanticRecordRef::element("exact-missing-revision")],
    }
}

#[test]
fn guarded_recovery_request_keeps_exact_identity_and_integer_commit() {
    let encoded = CentralizedPersistence::encode_semantic_operation(
        SemanticOperation::RecoverRevisionIntegrity(recovery_request()),
    )
    .unwrap();
    assert_eq!(encoded.operation_name, "RecoverRevisionIntegrity");
    let decoded = CentralizedPersistence::decode_semantic_operation(&encoded).unwrap();
    let SemanticOperation::RecoverRevisionIntegrity(request) = decoded else {
        panic!("expected recovery request")
    };
    assert_eq!(request.operation_id, "recover-雪");
    assert_eq!(request.expected_commit_version, 9_223_372_036_854_775_797);
    assert_eq!(
        request.missing_records,
        vec![SemanticRecordRef::element("exact-missing-revision")]
    );
    assert_eq!(request.provider_instance_id, "source-parser");
    assert_eq!(request.project_root, "/source");
    assert_eq!(request.project_id, "lineage");
}

#[test]
fn unavailable_resolution_remains_distinct_from_pruning_and_has_no_fabricated_body() {
    let resolution = SemanticRevisionResolution {
        record: SemanticRecordRef::element("old-revision"),
        element_scope: Some(SemanticRevisionElementScope {
            path: "src/main.rs".into(),
            provider_instance_id: Some("parser".into()),
        }),
        body: SemanticRevisionBody::Unavailable(SemanticRevisionUnavailable {
            recovery_operation_id: "loss-acknowledgement".into(),
            unavailable_at_ms: 1234,
            canonical_lifecycle: Some("active".into()),
        }),
        artifact_state: None,
        created_at_ms: 50,
        pruned_at_ms: None,
        successors: vec![],
    };
    let encoded = CentralizedPersistence::encode_semantic_result(
        SemanticResult::RevisionResolution(Some(resolution)),
    )
    .unwrap();
    let SemanticResult::RevisionResolution(Some(decoded)) =
        CentralizedPersistence::decode_semantic_result(&encoded).unwrap()
    else {
        panic!("expected exact resolution")
    };
    assert_eq!(decoded.record.record_id, "old-revision");
    assert_eq!(decoded.pruned_at_ms, None);
    let SemanticRevisionBody::Unavailable(marker) = decoded.body else {
        panic!("expected unavailable, never a replacement body or pruned marker")
    };
    assert_eq!(marker.recovery_operation_id, "loss-acknowledgement");
    assert_eq!(marker.unavailable_at_ms, 1234);
    assert_eq!(marker.canonical_lifecycle.as_deref(), Some("active"));
}

struct StalledRecoveryClient {
    expected_deadline: Instant,
    invocations: AtomicUsize,
}

impl ResourceInvocationClient for StalledRecoveryClient {
    fn readiness(
        &self,
        _: &ReadinessRequestV1,
        _: &InvocationControl,
    ) -> Result<ReadinessResponseV1, TransportError> {
        panic!("execution must not replace the caller deadline with readiness")
    }

    fn invoke(
        &self,
        frames: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> Result<Vec<InvocationEnvelopeV1>, TransportError> {
        assert_eq!(control.deadline(), self.expected_deadline);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].protocol_major, 5);
        let Some(Payload::Start(start)) = &frames[0].payload else {
            panic!("expected invocation start")
        };
        assert_eq!(start.deadline_unix_ms, control.deadline_unix_ms());
        let Some(Operation::SemanticOperation(operation)) = &start.operation else {
            panic!("expected semantic request")
        };
        assert_eq!(operation.operation_name, "RecoverRevisionIntegrity");
        self.invocations.fetch_add(1, Ordering::SeqCst);
        while !control.is_expired() {
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(TransportError::DeadlineExceeded)
    }

    fn cancel(
        &self,
        _: &InvocationEnvelopeV1,
        _: &InvocationControl,
    ) -> Result<InvocationTerminalV1, TransportError> {
        panic!("the transport owns cancellation for its stalled invocation")
    }
}

#[test]
fn recovery_uses_one_original_deadline_and_reports_expiry_without_retry() {
    let control = InvocationControl::with_deadline(Duration::from_millis(100));
    let client = Arc::new(StalledRecoveryClient {
        expected_deadline: control.deadline(),
        invocations: AtomicUsize::new(0),
    });
    let persistence = CentralizedPersistence::new(client.clone(), "repair-operator");
    let error = persistence
        .execute(
            SemanticOperation::RecoverRevisionIntegrity(recovery_request()),
            &control,
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("deadline"), "{error}");
    assert_eq!(client.invocations.load(Ordering::SeqCst), 1);
    assert!(control.is_expired());
}
