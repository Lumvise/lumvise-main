#![cfg(unix)]
#![expect(
    dead_code,
    unused_imports,
    reason = "shared integration support intentionally serves lifecycle and repository binaries"
)]

include!("support/mod.rs");

use lumvise_plugin_protocol::{
    StorageTriggerChanged, StorageTriggerDisposition, StorageTriggerRequest,
};
use lumvise_plugin_runtime::{
    PluginInvocationCancellationRequest, PluginInvocationClass, PluginInvocationContext,
    PluginInvocationFailureKind, PluginInvocationRequest,
};

#[test]
fn default_plugin_invocation_deadline_is_sixty_seconds() {
    assert_eq!(
        lumvise_plugin_runtime::PLUGIN_INVOCATION_DEADLINE,
        Duration::from_secs(60)
    );
}

#[test]
fn controlled_invocation_executes_ready_export_through_public_contract() {
    let fixture = InstalledFixture::new("echo");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("echo").expect("start plugin");
    let context = PluginInvocationContext::new(
        "controlled-success",
        "runtime-test-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(1),
    );

    let outcome = system
        .invoke_controlled(PluginInvocationRequest::new(
            "echo",
            "echo.value",
            json!({"message": "controlled"}),
            context,
        ))
        .expect("controlled invocation");

    assert!(matches!(
        outcome,
        WireOutcome::Succeeded { value } if value["input"]["message"] == "controlled"
    ));
    system.stop("echo").expect("stop plugin");
}

#[tokio::test]
async fn controlled_handle_executes_ready_export_without_blocking_adapter() {
    let fixture = InstalledFixture::new("echo");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("echo").expect("start plugin");

    let handle = system
        .start_controlled_invocation(PluginInvocationRequest::new(
            "echo",
            "echo.value",
            json!({"message": "handle"}),
            PluginInvocationContext::new(
                "handle-success",
                "runtime-test-owner",
                PluginInvocationClass::Foreground,
                Instant::now() + Duration::from_secs(1),
            ),
        ))
        .expect("register handle");
    let outcome = handle.await.expect("handle invocation");
    assert!(matches!(
        outcome,
        WireOutcome::Succeeded { value } if value["input"]["message"] == "handle"
    ));
    system.stop("echo").expect("stop plugin");
}

#[tokio::test]
async fn exact_controlled_cancellation_only_completes_the_registered_handle() {
    let system = test_plugin_system();
    let first_context = PluginInvocationContext::new(
        "disconnect-request",
        "disconnect-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(1),
    )
    .with_route_identity("disconnect-session", Some("scope-a".to_owned()));
    let sibling_context = PluginInvocationContext::new(
        "sibling-request",
        "disconnect-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(1),
    )
    .with_route_identity("disconnect-session", Some("scope-a".to_owned()));
    let first = system
        .start_controlled_invocation(PluginInvocationRequest::new(
            "missing-plugin",
            "missing-export",
            json!({}),
            first_context,
        ))
        .expect("synchronously register first handle");
    let sibling = system
        .start_controlled_invocation(PluginInvocationRequest::new(
            "missing-plugin",
            "missing-export",
            json!({}),
            sibling_context,
        ))
        .expect("synchronously register sibling handle");

    assert!(
        system
            .cancel_controlled(&PluginInvocationCancellationRequest {
                plugin_id: Some("missing-plugin".to_owned()),
                request_id: "disconnect-request".to_owned(),
                owner_id: "disconnect-owner".to_owned(),
                session_id: Some("disconnect-session".to_owned()),
                scope_id: Some("scope-a".to_owned()),
            })
            .expect("cancel exact registered invocation")
    );
    assert_eq!(
        first.await.expect_err("cancelled handle").kind(),
        PluginInvocationFailureKind::Cancelled
    );
    assert_ne!(
        sibling.await.expect_err("missing sibling target").kind(),
        PluginInvocationFailureKind::Cancelled,
        "matching cancellation must not affect a sibling request"
    );
}

#[test]
fn controlled_invocation_rejects_empty_owner_as_invalid_input() {
    let system = test_plugin_system();
    let context = PluginInvocationContext::new(
        "invalid-owner",
        "",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(1),
    );

    let error = system
        .invoke_controlled(PluginInvocationRequest::new(
            "missing",
            "missing.export",
            json!({}),
            context,
        ))
        .expect_err("empty owner rejected before target lookup");

    assert_eq!(error.kind(), PluginInvocationFailureKind::InvalidInput);
    assert!(matches!(
        error.runtime_error(),
        PluginRuntimeError::InvocationContextInvalid {
            field: "owner_id",
            value,
            ..
        } if value.is_empty()
    ));
}

#[test]
fn controlled_invocation_rejects_pre_cancelled_work_before_dispatch() {
    let system = test_plugin_system();
    let context = PluginInvocationContext::new(
        "cancelled-request",
        "runtime-test-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(1),
    );
    context.cancellation().cancel();

    let error = system
        .invoke_controlled(PluginInvocationRequest::new(
            "missing",
            "missing.export",
            json!({}),
            context,
        ))
        .expect_err("cancelled request rejected before target lookup");

    assert_eq!(error.kind(), PluginInvocationFailureKind::Cancelled);
    assert!(!error.retryable());
}

#[test]
fn controlled_invocation_rejects_expired_work_before_dispatch() {
    let system = test_plugin_system();
    let context = PluginInvocationContext::new(
        "expired-request",
        "runtime-test-owner",
        PluginInvocationClass::Foreground,
        Instant::now(),
    );

    let error = system
        .invoke_controlled(PluginInvocationRequest::new(
            "missing",
            "missing.export",
            json!({}),
            context,
        ))
        .expect_err("expired request rejected before target lookup");

    assert_eq!(error.kind(), PluginInvocationFailureKind::DeadlineExceeded);
    assert!(error.retryable());
}

#[test]
fn lifecycle_invokes_real_packaged_subprocess_then_detaches() {
    let fixture = InstalledFixture::new("echo");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");
    system.start("echo").expect("ready fixture process");

    let outcome = system
        .invoke("echo", "echo.value", json!({"message": "hello"}))
        .expect("invoke fixture process");
    let expected = WireOutcome::Succeeded {
        value: json!({
            "capability_id": "echo.value",
            "input": {"message": "hello"}
        }),
    };
    assert_eq!(outcome, expected);

    system.stop("echo").expect("graceful fixture stop");
    system.uninstall("echo").expect("detach fixture package");
    assert!(!system.is_installed("echo").expect("catalog readable"));
}

#[test]
fn compiled_process_round_trips_payload_above_legacy_eight_megabyte_limit() {
    let fixture = InstalledFixture::new("echo");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("echo").expect("start plugin");
    let content = "x".repeat(8 * 1024 * 1024 + 1);

    let outcome = system
        .invoke("echo", "echo.value", json!({"content": content}))
        .expect("invoke dynamic Protobuf frame");

    assert!(matches!(
        outcome,
        WireOutcome::Succeeded { value }
            if value["input"]["content"].as_str().is_some_and(|value| value.len() > 8 * 1024 * 1024)
    ));
    system.stop("echo").expect("stop plugin");
}
#[test]
fn compiled_process_round_trips_storage_trigger_batch_contract() {
    let fixture = InstalledFixture::new("echo");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("echo").expect("start plugin");
    let request = StorageTriggerRequest {
        project_root: "/repo".into(),
        base_revision: 10,
        target_revision: 12,
        changed: vec![
            StorageTriggerChanged {
                entity_id: "element-1".into(),
                entity_kind: "semantic_element".into(),
                disposition: StorageTriggerDisposition::Upserted,
            },
            StorageTriggerChanged {
                entity_id: "element-2".into(),
                entity_kind: "semantic_element".into(),
                disposition: StorageTriggerDisposition::Removal,
            },
        ],
    };
    let outcome = system
        .invoke(
            "echo",
            "echo.value",
            request.to_value().expect("encode batch"),
        )
        .expect("invoke StorageTrigger batch");
    assert!(matches!(
        outcome,
        WireOutcome::Succeeded { value } if value["input"] == request.to_value().unwrap()
    ));
    system.stop("echo").expect("stop plugin");
}

#[test]
fn active_process_snapshot_reports_real_ready_child_only() {
    let fixture = InstalledFixture::new("echo");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    assert!(
        system
            .active_processes()
            .expect("stopped snapshot")
            .is_empty()
    );

    system.start("echo").expect("start plugin");
    let snapshot = system.active_processes().expect("active snapshot");
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].plugin_id, "echo");
    assert_ne!(snapshot[0].process_id, 0);
    assert_ne!(snapshot[0].process_id, std::process::id());

    system.stop("echo").expect("stop plugin");
    assert!(
        system
            .active_processes()
            .expect("stopped snapshot")
            .is_empty()
    );
}

#[test]
fn cataloged_identities_survive_stop_and_crash_until_uninstall() {
    let echo = InstalledFixture::new("echo");
    let crash = InstalledFixture::new("crash");
    let system = test_plugin_system();
    system
        .install(&crash.package)
        .expect("install crash package");
    system.install(&echo.package).expect("install echo package");
    assert_eq!(
        system.cataloged_plugin_ids().expect("catalog"),
        ["crash", "echo"]
    );

    system.start("echo").expect("start echo");
    system.stop("echo").expect("stop echo");
    system.start("crash").expect("start crash");
    let _ = system.invoke("crash", "fixture.crash", json!({}));
    assert_eq!(
        system.cataloged_plugin_ids().expect("catalog"),
        ["crash", "echo"]
    );

    system
        .uninstall("crash")
        .expect("uninstall crashed package");
    system.uninstall("echo").expect("uninstall stopped package");
    assert!(system.cataloged_plugin_ids().expect("catalog").is_empty());
}

#[test]
fn background_export_discovery_is_signed_and_ready_only() {
    let fixture = InstalledFixture::new("background-catalog");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    assert!(system.background_exports().expect("catalog").is_empty());

    system.start("background-catalog").expect("start package");
    let exports = system.background_exports().expect("ready catalog");
    assert_eq!(exports.len(), 2);
    assert_eq!(exports[0].export_id, "fixture.recurring");
    assert_eq!(exports[1].export_id, "fixture.storage");

    system.stop("background-catalog").expect("stop package");
    assert!(system.background_exports().expect("catalog").is_empty());
}

#[test]
fn command_discovery_and_invocation_are_signed_and_ready_only() {
    let fixture = InstalledFixture::new("command-catalog");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    assert!(
        system
            .command_exports()
            .expect("stopped commands")
            .is_empty()
    );

    system.start("command-catalog").expect("start package");
    let commands = system.command_exports().expect("ready commands");
    assert!(
        commands
            .iter()
            .any(|command| command.plugin_id == "command-catalog"
                && command.export.id == "echo.value")
    );
    let outcome = system
        .invoke(
            "command-catalog",
            "echo.value",
            json!({"message": "command"}),
        )
        .expect("invoke Command");
    assert!(matches!(outcome, WireOutcome::Succeeded { value }
        if value["capability_id"] == "echo.value"));
    assert!(
        !commands
            .iter()
            .any(|command| command.export.id == "target.echo")
    );

    system.stop("command-catalog").expect("stop package");
    assert!(
        system
            .command_exports()
            .expect("stopped catalog")
            .is_empty()
    );
}

#[test]
fn default_sandbox_denies_execution_before_spawn() {
    let fixture = InstalledFixture::new("sandbox-denied");
    let system = PluginSystem::new(fast_config());
    system.install(&fixture.package).expect("install package");

    let error = system
        .start("sandbox-denied")
        .expect_err("default sandbox must fail closed");

    assert!(matches!(
        error,
        PluginRuntimeError::Sandbox { plugin_id, source }
            if plugin_id == "sandbox-denied"
                && source.code == "sandbox_adapter_required"
    ));
    assert!(
        !system
            .is_active("sandbox-denied")
            .expect("catalog readable")
    );
}

#[test]
fn invoke_rejects_installed_process_before_ready() {
    let fixture = InstalledFixture::new("not-started");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");

    let error = system
        .invoke("not-started", "echo.value", json!({}))
        .expect_err("invoke must require ready");

    assert!(matches!(error, PluginRuntimeError::NotReady(plugin_id) if plugin_id == "not-started"));
}

#[test]
fn start_rejects_ready_plugin_id_different_from_signed_manifest() {
    let fixture = InstalledFixture::new("wrong-id");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");

    let error = system.start("wrong-id").expect_err("identity mismatch");

    assert!(
        matches!(error, PluginRuntimeError::ReadyIdentityMismatch { actual_id, .. } if actual_id == "other-plugin")
    );
}

#[test]
fn start_rejects_ready_digest_different_from_signed_manifest() {
    let fixture = InstalledFixture::new("wrong-digest");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");

    let error = system.start("wrong-digest").expect_err("digest mismatch");

    assert!(
        matches!(error, PluginRuntimeError::ReadyIdentityMismatch { actual_digest, .. } if actual_digest == "0".repeat(64))
    );
}

#[test]
fn crash_unpublishes_process_after_real_subprocess_exit() {
    let fixture = InstalledFixture::new("crash");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");
    system.start("crash").expect("ready fixture process");

    let error = system
        .invoke("crash", "fixture.crash", json!({}))
        .expect_err("crashed process must fail invocation");

    assert!(matches!(
        error,
        PluginRuntimeError::ProcessExited { status, stderr, .. }
            if status.contains("23") && stderr.contains("fixture crash requested")
    ));
    assert!(!system.is_active("crash").expect("catalog readable"));
}

#[test]
fn invocation_timeout_kills_and_unpublishes_process() {
    let fixture = InstalledFixture::new("timeout");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");
    system.start("timeout").expect("ready fixture process");

    let error = system
        .invoke("timeout", "fixture.timeout", json!({}))
        .expect_err("slow invocation must time out");

    assert!(matches!(
        error,
        PluginRuntimeError::InvocationTimeout { .. }
    ));
    assert!(!system.is_active("timeout").expect("catalog readable"));

    system.start("timeout").expect("restart timed-out plugin");
    let outcome = system
        .invoke("timeout", "echo.value", json!({"after": "timeout"}))
        .expect("invoke fresh child after timeout");
    assert!(matches!(outcome, WireOutcome::Succeeded { value }
        if value["input"]["after"] == "timeout"));
}

#[test]
fn invocation_rejects_result_with_different_correlation_id() {
    let fixture = InstalledFixture::new("wrong-correlation");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");
    system
        .start("wrong-correlation")
        .expect("ready fixture process");

    let error = system
        .invoke("wrong-correlation", "fixture.echo", json!({}))
        .expect_err("result correlation must match request");

    assert!(matches!(
        error,
        PluginRuntimeError::CorrelationMismatch { actual, .. }
            if actual == "different-invocation"
    ));
}

#[test]
fn shutdown_grace_forces_unresponsive_process_to_exit() {
    let fixture = InstalledFixture::new("ignore-shutdown");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");
    system
        .start("ignore-shutdown")
        .expect("ready fixture process");
    let started = Instant::now();

    system.stop("ignore-shutdown").expect("forced fixture stop");

    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn handshake_timeout_does_not_publish_process() {
    let fixture = InstalledFixture::new("handshake-timeout");
    let mut config = fast_config();
    config.handshake_timeout = Duration::from_millis(100);
    let system = test_plugin_system_with_broker(config, Arc::new(DenyAllHostCapabilityBroker));
    system
        .install(&fixture.package)
        .expect("install runtime package");

    let error = system
        .start("handshake-timeout")
        .expect_err("ready timeout");

    assert!(matches!(error, PluginRuntimeError::HandshakeTimeout { .. }));
}

#[test]
fn ready_catalog_publishes_signed_exports_and_stop_removes_them() {
    let fixture = InstalledFixture::new("export-catalog");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");
    assert!(matches!(
        system.exports("export-catalog"),
        Err(PluginRuntimeError::NotReady(_))
    ));
    system
        .start("export-catalog")
        .expect("ready fixture process");

    let published = system.exports("export-catalog").expect("ready exports");
    assert!(
        published
            .iter()
            .any(|export| export.id == "fixture.host-call")
    );

    system.stop("export-catalog").expect("stop fixture process");
    assert!(matches!(
        system.exports("export-catalog"),
        Err(PluginRuntimeError::NotReady(_))
    ));
}

#[test]
fn scoped_export_discovery_is_signed_filtered_and_ready_only() {
    let fixture = InstalledFixture::new("scoped-catalog");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install scoped package");
    assert!(
        system
            .scoped_exports("assistant_session")
            .expect("stopped scope")
            .is_empty()
    );
    system
        .start("scoped-catalog")
        .expect("start scoped package");

    let assistant = system
        .scoped_exports("assistant_session")
        .expect("assistant scope");
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0].plugin_id, "scoped-catalog");
    assert_eq!(assistant[0].exports.len(), 1);
    assert_eq!(assistant[0].exports[0].id, "fixture.assistant");
    assert!(
        system
            .scoped_exports("missing_scope")
            .expect("missing scope")
            .is_empty()
    );

    system.stop("scoped-catalog").expect("stop scoped package");
    assert!(
        system
            .scoped_exports("assistant_session")
            .expect("removed scope")
            .is_empty()
    );
}

#[test]
fn declared_and_granted_host_call_uses_injected_broker() {
    let fixture = InstalledFixture::new("host-call-allowed");
    let broker = Arc::new(RecordingClockBroker::default());
    let system = test_plugin_system_with_broker(fast_config(), broker.clone());
    system
        .install(&fixture.package)
        .expect("install runtime package");
    system
        .start("host-call-allowed")
        .expect("ready fixture process");

    let outcome = system
        .invoke("host-call-allowed", "fixture.host-call", json!({}))
        .expect("brokered host call");

    assert_eq!(
        outcome,
        WireOutcome::Succeeded {
            value: json!({
                "host_outcome": {
                    "status": "succeeded",
                    "value": {"unix_seconds": 42}
                }
            }),
        }
    );
    let requests = broker.requests.lock().expect("recording broker lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].capability_id, "clock.read");
    assert_eq!(requests[0].required_version, "^1.0");
}

#[test]
fn declared_but_ungranted_host_call_receives_default_deny_result() {
    let fixture = InstalledFixture::new("host-call-denied");
    let system = test_plugin_system();
    system
        .install(&fixture.package)
        .expect("install runtime package");
    system
        .start("host-call-denied")
        .expect("ready fixture process");

    let outcome = system
        .invoke("host-call-denied", "fixture.host-call", json!({}))
        .expect("denied host call returns terminal plugin result");

    assert_eq!(
        outcome,
        WireOutcome::Succeeded {
            value: json!({
                "host_outcome": {
                    "status": "failed",
                    "error": {
                        "code": "host_capability_denied",
                        "message": "host policy grants no capabilities",
                        "details": null,
                        "retryable": false
                    }
                }
            }),
        }
    );
}

#[test]
fn undeclared_host_call_receives_structured_rejection() {
    let fixture = InstalledFixture::new("host-call-undeclared");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("host-call-undeclared").expect("ready plugin");

    let outcome = system
        .invoke("host-call-undeclared", "fixture.host-call", json!({}))
        .expect("plugin receives undeclared result");

    let value = match outcome {
        WireOutcome::Succeeded { value } => value,
        other => panic!("expected terminal success, got {other:?}"),
    };
    assert_eq!(
        value["host_outcome"]["error"]["code"],
        "host_capability_undeclared"
    );
}

#[test]
fn host_call_with_mismatched_session_unpublishes_process() {
    let fixture = InstalledFixture::new("host-call-session-mismatch");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system
        .start("host-call-session-mismatch")
        .expect("ready plugin");

    let error = system
        .invoke("host-call-session-mismatch", "fixture.host-call", json!({}))
        .expect_err("session mismatch must fail");

    assert!(matches!(
        error,
        PluginRuntimeError::MessageSessionMismatch { .. }
    ));
}

#[test]
fn host_call_with_mismatched_parent_unpublishes_process() {
    let fixture = InstalledFixture::new("host-call-parent-mismatch");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system
        .start("host-call-parent-mismatch")
        .expect("ready plugin");

    let error = system
        .invoke("host-call-parent-mismatch", "fixture.host-call", json!({}))
        .expect_err("parent mismatch must fail");

    assert!(matches!(
        error,
        PluginRuntimeError::HostCallParentMismatch { .. }
    ));
}

#[test]
fn repeated_live_host_call_id_is_rejected_as_reentrant() {
    let fixture = InstalledFixture::new("host-call-reentrant");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("host-call-reentrant").expect("ready plugin");

    let error = system
        .invoke("host-call-reentrant", "fixture.host-call", json!({}))
        .expect_err("duplicate host call must fail");

    assert!(matches!(
        error,
        PluginRuntimeError::ReentrantHostCall { call_id, .. } if call_id == "host-call-1"
    ));
}

#[test]
fn blocked_plugin_does_not_block_catalog_or_second_plugin() {
    let plugin_a = InstalledFixture::new("host-call-allowed");
    let plugin_b = InstalledFixture::new("plugin-b");
    let (broker, entered, release) = BlockingClockBroker::new();
    let mut config = fast_config();
    config = config.with_controlled_test_deadline(Duration::from_secs(5));
    let system = Arc::new(test_plugin_system_with_broker(config, broker));
    system.install(&plugin_a.package).expect("install plugin A");
    system.install(&plugin_b.package).expect("install plugin B");
    system.start("host-call-allowed").expect("start plugin A");
    let invoking_system = Arc::clone(&system);
    let invocation = thread::spawn(move || {
        invoking_system.invoke("host-call-allowed", "fixture.host-call", json!({}))
    });
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("plugin A reached blocking broker");

    let probing_system = Arc::clone(&system);
    let (probe_tx, probe_rx) = mpsc::channel();
    let probe = thread::spawn(move || {
        let result = probe_second_plugin(&probing_system).map_err(|error| error.to_string());
        probe_tx.send(result).expect("send isolation result");
    });
    let probe_result = probe_rx.recv_timeout(Duration::from_secs(2));
    release.send(()).expect("release plugin A broker");

    probe_result
        .expect("catalog and plugin B must not wait for plugin A")
        .expect("plugin B isolation operations");
    probe.join().expect("join isolation probe");
    invocation
        .join()
        .expect("join plugin A invocation")
        .expect("complete plugin A invocation");
}

#[test]
fn saturated_plugin_mailbox_does_not_block_second_plugin_or_catalog_reads() {
    let plugin_a = InstalledFixture::new("host-call-allowed");
    let plugin_b = InstalledFixture::new("plugin-b");
    let (broker, entered, release) = BlockingClockBroker::new();
    let mut config = fast_config();
    config = config.with_controlled_test_deadline(Duration::from_secs(5));
    config.maximum_queued_invocations_per_plugin = 4;
    let system = Arc::new(test_plugin_system_with_broker(config, broker));
    system.install(&plugin_a.package).expect("install plugin A");
    system.install(&plugin_b.package).expect("install plugin B");
    system.start("host-call-allowed").expect("start plugin A");
    let active = spawn_blocking_invocation(Arc::clone(&system));
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("plugin A reached blocking broker");
    let waiters = (0..4)
        .map(|_| spawn_blocking_invocation(Arc::clone(&system)))
        .collect::<Vec<_>>();
    wait_for_plugin_queue(&system, "host-call-allowed", 4);

    let overload_context = PluginInvocationContext::new(
        "overloaded-request",
        "runtime-test-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(1),
    );
    let overload = system
        .invoke_controlled(PluginInvocationRequest::new(
            "host-call-allowed",
            "fixture.host-call",
            json!({}),
            overload_context,
        ))
        .expect_err("fifth waiter exceeds plugin-local bound");
    assert_eq!(overload.kind(), PluginInvocationFailureKind::Busy);

    let started = Instant::now();
    probe_second_plugin(&system).expect("plugin B isolation operations");
    assert!(started.elapsed() < Duration::from_secs(2));
    for _ in 0..5 {
        release.send(()).expect("release plugin A invocation");
    }
    active.join().expect("active join").expect("active result");
    for waiter in waiters {
        waiter.join().expect("waiter join").expect("waiter result");
    }

    let snapshot = system
        .invocation_admission_snapshot("host-call-allowed")
        .expect("admission snapshot");
    assert_eq!((snapshot.executing, snapshot.queued), (false, 0));
    assert_eq!(snapshot.overloaded, 1);
}

#[test]
fn full_owner_tuple_cancels_queued_invocation_without_cross_owner_access() {
    let fixture = InstalledFixture::new("host-call-allowed");
    let (broker, entered, release) = BlockingClockBroker::new();
    let mut config = fast_config();
    config = config.with_controlled_test_deadline(Duration::from_secs(5));
    let system = Arc::new(test_plugin_system_with_broker(config, broker));
    system.install(&fixture.package).expect("install plugin");
    system.start("host-call-allowed").expect("start plugin");
    let active = spawn_blocking_invocation(Arc::clone(&system));
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("active entered");
    let queued_system = Arc::clone(&system);
    let queued = thread::spawn(move || {
        let context = PluginInvocationContext::new(
            "queued-request",
            "owner-a",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(2),
        )
        .with_route_identity("session-a", Some("scope-a".into()));
        queued_system.invoke_controlled(PluginInvocationRequest::new(
            "host-call-allowed",
            "fixture.host-call",
            json!({}),
            context,
        ))
    });
    wait_for_plugin_queue(&system, "host-call-allowed", 1);

    let mut cancellation = PluginInvocationCancellationRequest {
        plugin_id: Some("host-call-allowed".into()),
        request_id: "queued-request".into(),
        owner_id: "owner-b".into(),
        session_id: Some("session-a".into()),
        scope_id: Some("scope-a".into()),
    };
    assert!(!system.cancel_controlled(&cancellation).unwrap());
    cancellation.owner_id = "owner-a".into();
    assert!(system.cancel_controlled(&cancellation).unwrap());
    let error = queued
        .join()
        .expect("queued join")
        .expect_err("queued cancelled");
    assert_eq!(error.kind(), PluginInvocationFailureKind::Cancelled);

    release.send(()).expect("release active");
    active.join().expect("active join").expect("active result");
    let snapshot = system
        .invocation_admission_snapshot("host-call-allowed")
        .unwrap();
    assert_eq!((snapshot.queued, snapshot.cancelled), (0, 1));
}

#[test]
fn running_cancellation_uses_protocol_control_and_plugin_restarts_cleanly() {
    let fixture = InstalledFixture::new("timeout");
    let mut config = fast_config();
    config = config.with_controlled_test_deadline(Duration::from_secs(5));
    config.shutdown_grace = Duration::from_millis(50);
    let system = Arc::new(test_plugin_system_with_broker(
        config,
        Arc::new(DenyAllHostCapabilityBroker),
    ));
    system.install(&fixture.package).expect("install plugin");
    system.start("timeout").expect("start plugin");
    let invoking = Arc::clone(&system);
    let invocation = thread::spawn(move || {
        let context = PluginInvocationContext::new(
            "running-request",
            "owner-a",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(2),
        )
        .with_route_identity("session-a", None);
        invoking.invoke_controlled(PluginInvocationRequest::new(
            "timeout",
            "fixture.timeout",
            json!({}),
            context,
        ))
    });
    wait_for_plugin_execution(&system, "timeout");

    let started = Instant::now();
    assert!(
        system
            .cancel_controlled(&PluginInvocationCancellationRequest {
                plugin_id: Some("timeout".into()),
                request_id: "running-request".into(),
                owner_id: "owner-a".into(),
                session_id: Some("session-a".into()),
                scope_id: None,
            })
            .unwrap()
    );
    let error = invocation
        .join()
        .expect("invocation join")
        .expect_err("cancelled");
    assert_eq!(error.kind(), PluginInvocationFailureKind::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(system.published_plugins().unwrap().is_empty());

    system.start("timeout").expect("restart plugin");
    system
        .invoke("timeout", "echo.value", json!({}))
        .expect("invoke after restart");
}

#[test]
fn blocked_host_capability_cancels_and_plugin_restarts() {
    let fixture = InstalledFixture::new("host-call-allowed");
    let (broker, entered, release) = BlockingClockBroker::new();
    let mut config = fast_config();
    config = config.with_controlled_test_deadline(Duration::from_secs(5));
    config.shutdown_grace = Duration::from_millis(50);
    let system = Arc::new(test_plugin_system_with_broker(config, broker));
    system.install(&fixture.package).expect("install plugin");
    system.start("host-call-allowed").expect("start plugin");
    let invoking = Arc::clone(&system);
    let invocation = thread::spawn(move || {
        let context = PluginInvocationContext::new(
            "host-request",
            "owner-a",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(2),
        )
        .with_route_identity("session-a", None);
        invoking.invoke_controlled(PluginInvocationRequest::new(
            "host-call-allowed",
            "fixture.host-call",
            json!({}),
            context,
        ))
    });
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("host call entered");

    assert!(
        system
            .cancel_controlled(&PluginInvocationCancellationRequest {
                plugin_id: Some("host-call-allowed".into()),
                request_id: "host-request".into(),
                owner_id: "owner-a".into(),
                session_id: Some("session-a".into()),
                scope_id: None,
            })
            .unwrap()
    );
    let error = invocation
        .join()
        .expect("invocation join")
        .expect_err("cancelled");
    assert_eq!(error.kind(), PluginInvocationFailureKind::Cancelled);
    release.send(()).expect("release detached host call");

    system.start("host-call-allowed").expect("restart plugin");
    release.send(()).expect("release restarted host call");
    system
        .invoke("host-call-allowed", "echo.value", json!({}))
        .expect("invoke after restart");
}

fn wait_for_plugin_execution(system: &PluginSystem, plugin_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !system
        .invocation_admission_snapshot(plugin_id)
        .unwrap()
        .executing
    {
        assert!(Instant::now() < deadline, "plugin did not begin execution");
        thread::yield_now();
    }
}

fn spawn_blocking_invocation(
    system: Arc<PluginSystem>,
) -> thread::JoinHandle<Result<WireOutcome, PluginRuntimeError>> {
    thread::spawn(move || system.invoke("host-call-allowed", "fixture.host-call", json!({})))
}

fn wait_for_plugin_queue(system: &PluginSystem, plugin_id: &str, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while system
        .invocation_admission_snapshot(plugin_id)
        .expect("admission snapshot")
        .queued
        != expected
    {
        assert!(Instant::now() < deadline, "queue did not reach {expected}");
        thread::yield_now();
    }
}

fn probe_second_plugin(system: &PluginSystem) -> Result<(), PluginRuntimeError> {
    assert!(system.is_installed("host-call-allowed")?);
    assert!(!system.exports("host-call-allowed")?.is_empty());
    assert!(
        system
            .published_plugins()?
            .iter()
            .any(|plugin| plugin.plugin_id == "host-call-allowed")
    );
    system.start("plugin-b")?;
    system.invoke("plugin-b", "echo.value", json!({}))?;
    Ok(())
}

#[test]
fn broker_can_reenter_read_only_catalog_without_deadlock() {
    let fixture = InstalledFixture::new("host-call-allowed");
    let broker = Arc::new(CatalogReadingBroker::default());
    let system = Arc::new(test_plugin_system_with_broker(
        fast_config(),
        broker.clone(),
    ));
    broker
        .system
        .set(Arc::downgrade(&system))
        .expect("configure catalog reader");
    system.install(&fixture.package).expect("install package");
    system.start("host-call-allowed").expect("start plugin");

    system
        .invoke("host-call-allowed", "fixture.host-call", json!({}))
        .expect("broker catalog reentry");

    assert_eq!(
        *broker
            .observed_ready_plugins
            .lock()
            .expect("observed plugins lock"),
        vec!["host-call-allowed".to_owned()]
    );
}

#[test]
fn concurrent_duplicate_start_publishes_one_process() {
    let fixture = InstalledFixture::new("duplicate-start");
    let system = Arc::new(test_plugin_system());
    system.install(&fixture.package).expect("install package");
    let barrier = Arc::new(Barrier::new(3));
    let first = spawn_start(Arc::clone(&system), Arc::clone(&barrier), "duplicate-start");
    let second = spawn_start(Arc::clone(&system), Arc::clone(&barrier), "duplicate-start");
    barrier.wait();

    let results = [
        first.join().expect("first start"),
        second.join().expect("second start"),
    ];

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(PluginRuntimeError::AlreadyActive(_))))
            .count(),
        1
    );
}

fn spawn_start(
    system: Arc<PluginSystem>,
    barrier: Arc<Barrier>,
    plugin_id: &'static str,
) -> thread::JoinHandle<Result<(), PluginRuntimeError>> {
    thread::spawn(move || {
        barrier.wait();
        system.start(plugin_id)
    })
}

#[test]
fn concurrent_start_and_uninstall_have_one_atomic_winner() {
    let fixture = InstalledFixture::new("start-uninstall-race");
    let system = Arc::new(test_plugin_system());
    system.install(&fixture.package).expect("install package");
    let barrier = Arc::new(Barrier::new(3));
    let start = spawn_start(
        Arc::clone(&system),
        Arc::clone(&barrier),
        "start-uninstall-race",
    );
    let uninstall_system = Arc::clone(&system);
    let uninstall_barrier = Arc::clone(&barrier);
    let uninstall = thread::spawn(move || {
        uninstall_barrier.wait();
        uninstall_system.uninstall("start-uninstall-race")
    });
    barrier.wait();

    let start = start.join().expect("join start");
    let uninstall = uninstall.join().expect("join uninstall");

    let start_won =
        start.is_ok() && matches!(&uninstall, Err(PluginRuntimeError::ActiveUninstall(_)));
    let uninstall_won =
        uninstall.is_ok() && matches!(&start, Err(PluginRuntimeError::NotInstalled(_)));
    assert!(start_won || uninstall_won);
}

#[test]
fn invalid_nested_input_is_rejected_before_process_send() {
    let fixture = InstalledFixture::new("schema-guard");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("schema-guard").expect("start plugin");

    let error = system
        .invoke(
            "schema-guard",
            "schema.nested",
            json!({"profile": {"name": 42, "tags": ["safe"]}}),
        )
        .expect_err("invalid input schema");

    assert!(error.to_string().contains("offending value 42"));
    assert!(
        error
            .to_string()
            .contains("expected signed schema fragment")
    );
    assert!(matches!(
        &error,
        PluginRuntimeError::SchemaValidation {
            plugin_id,
            export_id,
            direction: SchemaDirection::Input,
            instance_path,
            ..
        } if plugin_id == "schema-guard"
            && export_id == "schema.nested"
            && instance_path == "/profile/name"
    ));
    assert!(system.is_active("schema-guard").expect("active catalog"));
    system
        .invoke(
            "schema-guard",
            "schema.nested",
            json!({"profile": {"name": "Ada", "tags": ["safe"]}}),
        )
        .expect("valid request proves invalid request was not sent");
}

#[test]
fn valid_nested_additional_properties_schema_round_trips() {
    let fixture = InstalledFixture::new("schema-guard");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("schema-guard").expect("start plugin");

    let outcome = system
        .invoke(
            "schema-guard",
            "schema.nested",
            json!({"profile": {"name": "Ada", "tags": ["rust", "plugins"]}}),
        )
        .expect("valid nested schemas");

    assert!(matches!(outcome, WireOutcome::Succeeded { .. }));
}

#[test]
fn invalid_successful_output_unpublishes_plugin() {
    let fixture = InstalledFixture::new("schema-invalid-output");
    let system = test_plugin_system();
    system.install(&fixture.package).expect("install package");
    system.start("schema-invalid-output").expect("start plugin");

    let error = system
        .invoke("schema-invalid-output", "schema.invalid-output", json!({}))
        .expect_err("invalid successful output");

    assert!(matches!(
        error,
        PluginRuntimeError::SchemaValidation {
            plugin_id,
            export_id,
            direction: SchemaDirection::Output,
            instance_path,
            ..
        } if plugin_id == "schema-invalid-output"
            && export_id == "schema.invalid-output"
            && instance_path == "/capability_id"
    ));
    assert!(
        !system
            .is_active("schema-invalid-output")
            .expect("active catalog")
    );
}

#[test]
fn invalid_signed_schema_rejects_install_before_catalog_publication() {
    let fixture = InstalledFixture::new("schema-compile-failure");
    let system = PluginSystem::new(fast_config());

    let error = system
        .install(&fixture.package)
        .expect_err("invalid Draft 2020-12 schema");

    assert!(matches!(
        error,
        PluginRuntimeError::SchemaCompilation {
            plugin_id,
            export_id,
            direction: SchemaDirection::Input,
            ..
        } if plugin_id == "schema-compile-failure"
            && export_id == "schema.compile-failure"
    ));
    assert!(
        !system
            .is_installed("schema-compile-failure")
            .expect("catalog readable")
    );
}
