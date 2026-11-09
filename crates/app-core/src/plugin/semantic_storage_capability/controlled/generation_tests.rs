//! A backend may finish after cancellation; the public host still must refuse delivery.
use lumvise_db_core::{
    ElementTextVectorGenerationState, ElementTextVectorGenerationStatus, LocalPersistence,
    RelationalPersistence, SemanticOperation, SemanticPersistence, SemanticReadiness,
    SemanticResult,
};
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityRequest, PluginInvocationClass, PluginInvocationContext,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

struct BlockingGenerationPersistence {
    local: Arc<LocalPersistence>,
    started: mpsc::Sender<InvocationControl>,
    completed: AtomicUsize,
}

impl SemanticPersistence for BlockingGenerationPersistence {
    fn readiness(&self) -> lumvise_db_core::Result<SemanticReadiness> {
        SemanticPersistence::readiness(self.local.as_ref())
    }
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::Result<SemanticResult> {
        let key = match operation {
            SemanticOperation::StageElementTextVectorChunk { key, .. }
            | SemanticOperation::CommitElementTextVectorGeneration { key } => key,
            other => return SemanticPersistence::execute(self.local.as_ref(), other, control),
        };
        self.started.send(control.clone()).unwrap();
        let guard = Instant::now() + Duration::from_secs(2);
        while !control.is_cancelled() && !control.is_expired() && Instant::now() < guard {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.completed.fetch_add(1, Ordering::SeqCst);
        Ok(SemanticResult::ElementTextVectorGeneration(
            ElementTextVectorGenerationStatus {
                key,
                state: ElementTextVectorGenerationState::Committed,
                next_chunk: 1,
                chunk_count: 1,
                published_count: 1,
            },
        ))
    }
}

fn request(operation: &str) -> HostCapabilityRequest {
    let mut input = json!({"operation":operation,"key":{"project_root":"/repo",
        "semantic_element_id":"section","generation_id":"generation"}});
    if operation == "stage_element_text_vector_chunk" {
        input["chunk"] = json!(0);
        input["vector"] = json!([1.0, 0.0]);
    }
    HostCapabilityRequest {
        plugin_id: "builtin.semantic".into(),
        invocation_id: "generation".into(),
        call_id: "stage".into(),
        capability_id: "storage.semantic".into(),
        required_version: "^1.0".into(),
        input,
    }
}

fn rejected_after_parent_ends(operation: &str, cancel: bool) {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let (started, received) = mpsc::channel();
    let persistence = Arc::new(BlockingGenerationPersistence {
        local: local.clone(),
        started,
        completed: AtomicUsize::new(0),
    });
    let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn RelationalPersistence> = local;
    let broker = crate::AppCoreHostCapabilityBroker::new(semantic, relational);
    let context = PluginInvocationContext::new(
        "generation",
        "owner",
        PluginInvocationClass::Foreground,
        Instant::now()
            + if cancel {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(100)
            },
    );
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| broker.invoke_controlled(request(operation), &context));
        let forwarded = received.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            forwarded.deadline() <= context.deadline(),
            "host replaced inherited deadline"
        );
        if cancel {
            context.cancellation().cancel();
        }
        let error = worker.join().unwrap().unwrap_err().to_string();
        let expected = if cancel { "cancelled" } else { "deadline" };
        assert!(error.contains(expected), "{operation}: {error}");
        assert_eq!(
            persistence.completed.load(Ordering::SeqCst),
            1,
            "backend returned a status before host delivery was refused"
        );
        assert!(if cancel {
            forwarded.is_cancelled()
        } else {
            forwarded.is_expired()
        });
    });
}

#[test]
fn stage_and_commit_forward_parent_cancellation_and_refuse_successful_status() {
    for operation in [
        "stage_element_text_vector_chunk",
        "commit_element_text_vector_generation",
    ] {
        rejected_after_parent_ends(operation, true);
    }
}

#[test]
fn stage_and_commit_forward_parent_deadline_and_refuse_late_successful_status() {
    for operation in [
        "stage_element_text_vector_chunk",
        "commit_element_text_vector_generation",
    ] {
        rejected_after_parent_ends(operation, false);
    }
}
