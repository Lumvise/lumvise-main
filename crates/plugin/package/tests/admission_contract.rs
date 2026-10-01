use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{
    BackgroundDeliveryPolicy, BuildPackageRequest, ExclusiveLaneOperation, ExclusiveLanePolicy,
    ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility, InvocationAdmissionPolicy,
    PackageError, PluginManifest, ProtocolRange, PublisherIdentity, build_package, verify_package,
};
use sha2::{Digest, Sha256};

const TARGET: &str = "aarch64-apple-darwin";
const EXECUTABLE_PATH: &str = "bin/admission-test";
const EXECUTABLE: &[u8] = b"#!/bin/sh\nprintf admission-test";

#[test]
fn exclusive_lane_admission_requires_bounded_queue_and_timeouts() {
    let mut manifest = valid_manifest();
    manifest.exports[0].admission = Some(InvocationAdmissionPolicy::ExclusiveLane(
        ExclusiveLanePolicy {
            lane_id: "assistant".into(),
            operation: ExclusiveLaneOperation::Acquire,
            owner_argument: "owner_id".into(),
            session_argument: "session_id".into(),
            queue_argument: "queue".into(),
            replace_argument: "replace".into(),
            timeout_ms_argument: "timeout_ms".into(),
            default_timeout_ms: 1_000,
            max_timeout_ms: 5_000,
            max_queue_depth: 0,
            response_session_pointer: "/session_id".into(),
            terminal_pointer: "/phase".into(),
            terminal_values: vec!["completed".into()],
        },
    ));

    let (workspace, key) = write_package(manifest);
    let error = verify_package(
        &workspace.path().join("admission.lvp"),
        &key.verifying_key(),
        &compatible_host(),
    )
    .err()
    .expect("unbounded admission policy rejected");

    assert!(matches!(
        error,
        PackageError::InvalidInvocationAdmission { .. }
    ));
}

fn valid_manifest() -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "admission.release.1".into(),
        },
        plugin_id: "admission-test".into(),
        plugin_version: "1.0.0".into(),
        protocol: ProtocolRange { min: 2, max: 4 },
        targets: BTreeMap::from([(TARGET.into(), EXECUTABLE_PATH.into())]),
        files: BTreeMap::from([(
            EXECUTABLE_PATH.into(),
            hex::encode(Sha256::digest(EXECUTABLE)),
        )]),
        exports: vec![
            export(
                "assistant.start",
                ExportSurface::McpTool,
                ExecutionMode::Foreground,
            ),
            export(
                "knowledge.refresh",
                ExportSurface::RecurringTask {
                    interval_seconds: 600,
                    delivery: BackgroundDeliveryPolicy {
                        max_attempts: 3,
                        initial_backoff_ms: 100,
                        max_backoff_ms: 10_000,
                        dead_letter_max_entries: 100,
                        dead_letter_retention_seconds: 86_400,
                    },
                },
                ExecutionMode::Background,
            ),
        ],
        host_capabilities: Vec::new(),
    }
}

#[test]
fn release_observation_accepts_nullable_session_but_rejects_other_types() {
    for (session_schema, accepted) in [
        (
            serde_json::json!({"anyOf":[{"type":"string"},{"type":"null"}]}),
            true,
        ),
        (
            serde_json::json!({"anyOf":[{"type":"string"},{"type":"integer"}]}),
            false,
        ),
        (serde_json::json!({"type":"null"}), false),
    ] {
        let mut manifest = valid_manifest();
        manifest.exports[0].admission = Some(release_policy());
        manifest.exports[0].output_schema = serde_json::json!({"type":"object", "properties":{
            "session_id":session_schema,"phase":{"type":"string"}}});
        let (workspace, key) = write_package(manifest);
        let result = verify_package(
            &workspace.path().join("admission.lvp"),
            &key.verifying_key(),
            &compatible_host(),
        );
        assert_eq!(result.is_ok(), accepted, "nullable release ID validation");
    }
}

fn release_policy() -> InvocationAdmissionPolicy {
    InvocationAdmissionPolicy::ExclusiveLane(ExclusiveLanePolicy {
        lane_id: "assistant".into(),
        operation: ExclusiveLaneOperation::ReleaseOnOutput,
        owner_argument: "owner_id".into(),
        session_argument: "session_id".into(),
        queue_argument: "queue".into(),
        replace_argument: "replace".into(),
        timeout_ms_argument: "timeout_ms".into(),
        default_timeout_ms: 1000,
        max_timeout_ms: 5000,
        max_queue_depth: 8,
        response_session_pointer: "/session_id".into(),
        terminal_pointer: "/phase".into(),
        terminal_values: vec!["completed".into()],
    })
}

#[test]
fn acquisition_still_rejects_a_nullable_session_identity() {
    let mut manifest = valid_manifest();
    let InvocationAdmissionPolicy::ExclusiveLane(mut policy) = release_policy();
    policy.operation = ExclusiveLaneOperation::Acquire;
    manifest.exports[0].admission = Some(InvocationAdmissionPolicy::ExclusiveLane(policy));
    manifest.exports[0].input_schema = serde_json::json!({"type":"object", "properties":{
        "owner_id":{"type":"string"}, "session_id":{"type":"string"},
        "queue":{"type":"boolean"}, "replace":{"type":"boolean"}, "timeout_ms":{"type":"integer"}}});
    manifest.exports[0].output_schema = serde_json::json!({"type":"object", "properties":{
        "session_id":{"anyOf":[{"type":"string"},{"type":"null"}]}, "phase":{"type":"string"}}});
    let (workspace, key) = write_package(manifest);
    let result = verify_package(
        &workspace.path().join("admission.lvp"),
        &key.verifying_key(),
        &compatible_host(),
    );
    assert!(matches!(
        result,
        Err(PackageError::InvalidInvocationAdmission { .. })
    ));
}

fn export(id: &str, surface: ExportSurface, execution: ExecutionMode) -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: id.into(),
        name: id.replace('.', " "),
        surface,
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: serde_json::json!({"type": "object"}),
        admission: None,
        execution,
    }
}

fn write_package(manifest: PluginManifest) -> (tempfile::TempDir, SigningKey) {
    let workspace = tempfile::tempdir().expect("fixture workspace");
    let signing_key = SigningKey::from_bytes(&[9; 32]);
    build_package(
        &workspace.path().join("admission.lvp"),
        BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(EXECUTABLE_PATH.into(), EXECUTABLE.to_vec())]),
            signing_key: &signing_key,
        },
    )
    .expect("signed admission fixture");
    (workspace, signing_key)
}

fn compatible_host() -> HostCompatibility {
    HostCompatibility::new(3, TARGET)
}
