use super::*;
use lumvise_db_core::{
    DbError, LocalPersistence, PzSnapshotResult, SemanticOperation, SemanticReadiness,
    SemanticResult,
};
use lumvise_plugin_runtime::PluginInvocationClass;
use std::sync::mpsc;

struct DropProbePersistence {
    completed: mpsc::Sender<String>,
    dropped: mpsc::Sender<()>,
}

impl Drop for DropProbePersistence {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}

impl SemanticPersistence for DropProbePersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        _control: &InvocationControl,
    ) -> std::result::Result<SemanticResult, DbError> {
        match operation {
            SemanticOperation::ProjectRoots => Ok(SemanticResult::ProjectRoots(vec![
                "/project".into(),
                "assistant-space://private".into(),
            ])),
            SemanticOperation::CreatePzSnapshot {
                project_root,
                output_path,
            } => {
                let _ = self.completed.send(project_root);
                Ok(SemanticResult::PzSnapshot(PzSnapshotResult {
                    project_id: "project-id".into(),
                    snapshot_id: "snapshot-id".into(),
                    commit_version: 1,
                    published_at: "2026-01-01T00:00:00Z".into(),
                    output_path: output_path.into(),
                    output_bytes: 1,
                    row_counts: Default::default(),
                }))
            }
            _ => Err(DbError::invalid_value(
                "operation",
                "drop probe snapshot operation",
            )),
        }
    }

    fn readiness(&self) -> std::result::Result<SemanticReadiness, DbError> {
        Ok(SemanticReadiness { ready: true })
    }
}

#[test]
fn controlled_snapshot_uses_current_project_and_watcher_does_not_retain_service() {
    let (completed_tx, completed_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let semantic = Arc::new(DropProbePersistence {
        completed: completed_tx,
        dropped: dropped_tx,
    });
    let persistence = Arc::new(LocalPersistence::in_memory().expect("local persistence"));
    let relational: Arc<dyn RelationalPersistence> = persistence;
    let broker = AppCoreHostCapabilityBroker::new(
        Arc::clone(&semantic) as Arc<dyn SemanticPersistence>,
        relational,
    );
    drop(semantic);

    let context = PluginInvocationContext::new(
        "request-1",
        "owner-1",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_route_identity("session-1", Some("signed-scope-id".into()));
    let output = broker
        .invoke_controlled(
            HostCapabilityRequest {
                plugin_id: "plugin.a".into(),
                invocation_id: "invocation-1".into(),
                call_id: "call-1".into(),
                capability_id: SEMANTIC_SNAPSHOT_CAPABILITY.into(),
                required_version: "1".into(),
                input: json!({"operation": "create"}),
            },
            &context,
        )
        .expect("controlled snapshot create");
    assert!(output.get("operation_id").and_then(Value::as_str).is_some());
    assert_eq!(
        completed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("snapshot worker reached persistence"),
        "/project"
    );

    drop(broker);
    dropped_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("broker drop releases semantic persistence");
}
