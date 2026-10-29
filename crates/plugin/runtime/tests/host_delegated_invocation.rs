#[expect(dead_code, unused_imports, reason = "shared signed runtime fixtures")]
mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, HostPluginInvocation,
    HostPluginInvoker, PluginInvocationCancellationRequest, PluginInvocationClass,
    PluginInvocationContext, PluginInvocationFailureKind, PluginInvocationRequest,
};
use serde_json::{Value, json};
use support::*;

struct DelegatingClockBroker {
    target: String,
    deny_publication: bool,
    target_contexts: Mutex<Vec<PluginInvocationContext>>,
}

impl HostCapabilityBroker for DelegatingClockBroker {
    fn invoke(&self, request: HostCapabilityRequest) -> Result<Value, HostCapabilityError> {
        if request.capability_id == "plugin.invoke" && !self.deny_publication {
            return Ok(json!({}));
        }
        Err(HostCapabilityError::new(
            request.capability_id,
            "fixture_denied",
            "expected controlled host delegation and an authorized publication",
            false,
        ))
    }

    fn invoke_with_nested(
        &self,
        request: HostCapabilityRequest,
        context: &PluginInvocationContext,
        nested: &dyn HostPluginInvoker,
    ) -> Result<Value, HostCapabilityError> {
        if request.plugin_id == "host-call-nested-target" {
            self.target_contexts.lock().unwrap().push(context.clone());
            return Ok(json!({"observed":"nested-target"}));
        }
        if request.capability_id != "clock.read" {
            return self.invoke(request);
        }
        nested.invoke_plugin(HostPluginInvocation {
            plugin_id: self.target.clone(),
            export_id: "target.echo".into(),
            input: json!({"message":"host-owned publication"}),
        })
    }
}

fn delegated_fixture(
    caller: &str,
    target: &str,
    deny: bool,
) -> (
    lumvise_plugin_runtime::PluginSystem,
    Arc<DelegatingClockBroker>,
    InstalledFixture,
    InstalledFixture,
) {
    let caller_package = InstalledFixture::new(caller);
    let target_package = InstalledFixture::new(target);
    let broker = Arc::new(DelegatingClockBroker {
        target: target.into(),
        deny_publication: deny,
        target_contexts: Mutex::default(),
    });
    let config = fast_config().with_controlled_test_deadline(Duration::from_secs(2));
    let system = test_plugin_system_with_broker(config, broker.clone());
    system.install(&caller_package.package).unwrap();
    system.install(&target_package.package).unwrap();
    system.start(caller).unwrap();
    system.start(target).unwrap();
    (system, broker, caller_package, target_package)
}

fn delegated_context(deadline: Instant) -> PluginInvocationContext {
    PluginInvocationContext::new(
        "delegated-request",
        "original-owner",
        PluginInvocationClass::Foreground,
        deadline,
    )
    .with_route_identity("original-session", Some("knowledge_mcp".into()))
    .with_scope_epoch(Some(7))
}

fn invoke_delegated(
    system: &lumvise_plugin_runtime::PluginSystem,
    caller: &str,
    context: PluginInvocationContext,
) -> WireOutcome {
    system
        .invoke_controlled(PluginInvocationRequest::new(
            caller,
            "fixture.host-call",
            json!({}),
            context,
        ))
        .expect("caller transport completes")
}

#[test]
fn host_publication_inherits_identity_without_registering_another_root() {
    let (system, broker, _caller, _target) =
        delegated_fixture("host-call-delegating", "host-call-nested-target", false);
    let deadline = Instant::now() + Duration::from_secs(2);
    let outcome = invoke_delegated(&system, "host-call-delegating", delegated_context(deadline));
    let WireOutcome::Succeeded { value } = outcome else {
        panic!("expected host success")
    };
    assert_eq!(
        value["host_outcome"]["value"]["host_outcome"]["value"]["observed"],
        "nested-target"
    );
    let contexts = broker.target_contexts.lock().unwrap();
    assert_eq!(contexts.len(), 1);
    let inherited = &contexts[0];
    assert_eq!(inherited.request_id(), "delegated-request");
    assert_eq!(inherited.owner_id(), "original-owner");
    assert_eq!(inherited.session_id(), Some("original-session"));
    assert_eq!(inherited.scope_id(), Some("knowledge_mcp"));
    assert_eq!(inherited.scope_epoch(), Some(7));
    assert!(inherited.deadline() <= deadline);
}

#[test]
fn host_publication_still_requires_the_callers_signed_declaration_and_grant() {
    for (caller, denied, expected) in [
        (
            "host-call-delegating-undeclared",
            false,
            "host_capability_undeclared",
        ),
        ("host-call-delegating", true, "fixture_denied"),
    ] {
        let (system, broker, _caller, _target) =
            delegated_fixture(caller, "host-call-nested-target", denied);
        let outcome = invoke_delegated(
            &system,
            caller,
            delegated_context(Instant::now() + Duration::from_secs(2)),
        );
        let WireOutcome::Succeeded { value } = outcome else {
            panic!("expected structured denial")
        };
        assert_eq!(value["host_outcome"]["error"]["code"], expected);
        assert!(broker.target_contexts.lock().unwrap().is_empty());
    }
}

#[test]
fn host_publication_cannot_extend_its_parent_deadline() {
    let (system, _broker, _caller, _target) =
        delegated_fixture("host-call-delegating", "timeout", false);
    let started = Instant::now();
    let result = system.invoke_controlled(PluginInvocationRequest::new(
        "host-call-delegating",
        "fixture.host-call",
        json!({}),
        delegated_context(started + Duration::from_millis(150)),
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    match result {
        Err(error) => assert_eq!(error.kind(), PluginInvocationFailureKind::DeadlineExceeded),
        Ok(WireOutcome::Succeeded { value }) => assert!(matches!(
            value["host_outcome"]["error"]["code"].as_str(),
            Some("plugin_invoke_target_timeout" | "host_capability_deadline_exceeded")
        )),
        other => panic!("expected the shared deadline, got {other:?}"),
    }
}

#[test]
fn cancelling_host_publication_releases_the_nested_target() {
    let (system, _broker, _caller, _target) =
        delegated_fixture("host-call-delegating", "timeout", false);
    let system = Arc::new(system);
    let invoking = Arc::clone(&system);
    let invocation = std::thread::spawn(move || {
        invoking.invoke_controlled(PluginInvocationRequest::new(
            "host-call-delegating",
            "fixture.host-call",
            json!({}),
            delegated_context(Instant::now() + Duration::from_secs(2)),
        ))
    });
    let wait_until = Instant::now() + Duration::from_secs(1);
    while !system
        .invocation_admission_snapshot("timeout")
        .unwrap()
        .executing
    {
        assert!(Instant::now() < wait_until, "nested target did not start");
        std::thread::yield_now();
    }
    assert!(
        system
            .cancel_controlled(&PluginInvocationCancellationRequest {
                plugin_id: Some("host-call-delegating".into()),
                request_id: "delegated-request".into(),
                owner_id: "original-owner".into(),
                session_id: Some("original-session".into()),
                scope_id: Some("knowledge_mcp".into()),
            })
            .unwrap()
    );
    assert_eq!(
        invocation.join().unwrap().unwrap_err().kind(),
        PluginInvocationFailureKind::Cancelled
    );
    assert!(
        !system
            .invocation_admission_snapshot("timeout")
            .unwrap()
            .executing
    );
}
