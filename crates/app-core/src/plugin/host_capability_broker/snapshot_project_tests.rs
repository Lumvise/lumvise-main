use super::*;
use lumvise_db_core::{
    DbError, LocalPersistence, PzSnapshotResult, RelationalPersistence, SemanticOperation,
    SemanticReadiness, SemanticResult,
};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

struct CapturedSnapshot {
    project_root: String,
    output_path: String,
}

struct MultiRootSemanticPersistence {
    created: SyncSender<CapturedSnapshot>,
}

impl SemanticPersistence for MultiRootSemanticPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        _control: &InvocationControl,
    ) -> std::result::Result<SemanticResult, DbError> {
        match operation {
            SemanticOperation::ProjectRoots => Ok(SemanticResult::ProjectRoots(vec![
                "/projects/alpha".into(),
                "/projects/micrograd".into(),
            ])),
            SemanticOperation::CreatePzSnapshot {
                project_root,
                output_path,
            } => {
                self.created
                    .send(CapturedSnapshot {
                        project_root,
                        output_path: output_path.clone(),
                    })
                    .map_err(|_| DbError::invalid_value("snapshot", "connected test receiver"))?;
                Ok(SemanticResult::PzSnapshot(PzSnapshotResult {
                    project_id: "micrograd-id".into(),
                    snapshot_id: "snapshot-id".into(),
                    commit_version: 1,
                    published_at: "2026-01-01T00:00:00Z".into(),
                    output_path: PathBuf::from(output_path),
                    output_bytes: 1,
                    row_counts: Default::default(),
                }))
            }
            _ => Err(DbError::invalid_value(
                "operation",
                "ProjectRoots or CreatePzSnapshot",
            )),
        }
    }

    fn readiness(&self) -> std::result::Result<SemanticReadiness, DbError> {
        Ok(SemanticReadiness { ready: true })
    }
}

fn broker_and_created_snapshots() -> (AppCoreHostCapabilityBroker, Receiver<CapturedSnapshot>) {
    let (created, created_rx) = mpsc::sync_channel(1);
    let semantic = Arc::new(MultiRootSemanticPersistence { created });
    let relational: Arc<dyn RelationalPersistence> =
        Arc::new(LocalPersistence::in_memory().expect("relational adapter"));
    (
        AppCoreHostCapabilityBroker::new(semantic, relational),
        created_rx,
    )
}

fn invoke_create(
    broker: &AppCoreHostCapabilityBroker,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    broker.invoke(HostCapabilityRequest {
        plugin_id: "plugin.snapshot-test".into(),
        invocation_id: "snapshot-invocation".into(),
        call_id: "snapshot-call".into(),
        capability_id: SEMANTIC_SNAPSHOT_CAPABILITY.into(),
        required_version: "1".into(),
        input,
    })
}

fn assert_no_snapshot_was_created(
    broker: AppCoreHostCapabilityBroker,
    created: &Receiver<CapturedSnapshot>,
) {
    drop(broker);
    assert!(matches!(
        created.try_recv(),
        Err(TryRecvError::Disconnected)
    ));
}

#[test]
fn create_exports_the_explicit_root_when_multiple_projects_are_indexed() {
    let (broker, created) = broker_and_created_snapshots();
    let operation = invoke_create(
        &broker,
        json!({"operation":"create","project_root":"/projects/micrograd"}),
    )
    .expect("explicit project snapshot accepted");

    assert_eq!(operation["project_root"], "/projects/micrograd");
    let snapshot = created
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("snapshot worker reached semantic persistence");
    assert_eq!(snapshot.project_root, "/projects/micrograd");
    assert_eq!(
        snapshot.output_path,
        "/projects/micrograd/.lumvise/graph_db.pz"
    );
}

#[test]
fn create_without_a_root_rejects_multiple_projects_without_creating_a_snapshot() {
    let (broker, created) = broker_and_created_snapshots();
    let error = invoke_create(&broker, json!({"operation":"create"}))
        .expect_err("ambiguous project selection rejected");

    assert!(
        error
            .to_string()
            .contains("one active semantic project root")
    );
    assert_no_snapshot_was_created(broker, &created);
}

#[test]
fn create_rejects_a_blank_explicit_root_without_creating_a_snapshot() {
    let (broker, created) = broker_and_created_snapshots();
    let error = invoke_create(&broker, json!({"operation":"create","project_root":"  "}))
        .expect_err("blank project root rejected");

    assert!(error.to_string().contains("non-empty project root"));
    assert_no_snapshot_was_created(broker, &created);
}

#[test]
fn create_rejects_a_non_string_explicit_root_without_creating_a_snapshot() {
    let (broker, created) = broker_and_created_snapshots();
    let error = invoke_create(&broker, json!({"operation":"create","project_root":42}))
        .expect_err("non-string project root rejected");

    assert!(
        error
            .to_string()
            .contains("optional string field `project_root`")
    );
    assert_no_snapshot_was_created(broker, &created);
}
