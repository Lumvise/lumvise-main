//! Carries the parent plugin deadline through every semantic storage operation.
use crate::plugin::{
    plugin_host_capabilities::invocation_control, semantic_snapshot_writes::SemanticSnapshotWrites,
};
use lumvise_db_core::{SemanticOperation, SemanticPersistence, SemanticReadiness, SemanticResult};
use lumvise_plugin_runtime::{HostCapabilityError, PluginInvocationContext};
use lumvise_resource_routing::InvocationControl;
use serde_json::Value;

pub(in crate::plugin) fn invoke_controlled(
    semantic: &dyn SemanticPersistence,
    writes: &SemanticSnapshotWrites,
    plugin_id: &str,
    input: Value,
    context: &PluginInvocationContext,
) -> Result<Value, HostCapabilityError> {
    let bridge = invocation_control(context);
    let scoped = InvocationSemanticPersistence {
        semantic,
        control: bridge.control(),
    };
    super::invoke(&scoped, writes, plugin_id, input)
}

// Snapshot staging and ordinary reads both pass through this same adapter.
// Their local defaults must never replace the caller's deadline or cancellation.
struct InvocationSemanticPersistence<'a> {
    semantic: &'a dyn SemanticPersistence,
    control: InvocationControl,
}
impl SemanticPersistence for InvocationSemanticPersistence<'_> {
    fn execute(
        &self,
        operation: SemanticOperation,
        _control: &InvocationControl,
    ) -> lumvise_db_core::Result<SemanticResult> {
        self.semantic.execute(operation, &self.control)
    }
    fn readiness(&self) -> lumvise_db_core::Result<SemanticReadiness> {
        self.semantic.readiness()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_plugin_runtime::PluginInvocationClass;
    use serde_json::json;
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    struct BlockingSemanticPersistence {
        started: mpsc::Sender<InvocationControl>,
    }
    impl SemanticPersistence for BlockingSemanticPersistence {
        fn execute(
            &self,
            _operation: SemanticOperation,
            control: &InvocationControl,
        ) -> lumvise_db_core::Result<SemanticResult> {
            self.started.send(control.clone()).unwrap();
            let guard = Instant::now() + Duration::from_secs(2);
            while !control.is_cancelled() && !control.is_expired() && Instant::now() < guard {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(SemanticResult::Element(None))
        }
        fn readiness(&self) -> lumvise_db_core::Result<SemanticReadiness> {
            Ok(SemanticReadiness { ready: true })
        }
    }

    #[test]
    fn storage_inherits_parent_cancellation_and_deadline() {
        let (started_tx, started_rx) = mpsc::channel();
        let persistence = BlockingSemanticPersistence {
            started: started_tx,
        };
        let context = PluginInvocationContext::new(
            "request",
            "owner",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_millis(500),
        );
        let writes = SemanticSnapshotWrites::new();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                invoke_controlled(
                    &persistence,
                    &writes,
                    "builtin.semantic",
                    json!({"operation":"element", "semantic_element_id":"test"}),
                    &context,
                )
            });
            let control = started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(
                control
                    .deadline()
                    .saturating_duration_since(context.deadline())
                    < Duration::from_millis(5)
            );
            context.cancellation().cancel();
            worker.join().unwrap().unwrap();
            assert!(
                control.is_cancelled(),
                "storage must observe cancellation of its parent plugin invocation"
            );
        });
    }
}
