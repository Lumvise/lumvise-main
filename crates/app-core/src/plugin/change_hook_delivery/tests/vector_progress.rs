use super::*;
use crate::workspace_activity::{
    ActivityEntry, ActivitySnapshot, ActivityStatus, WorkspaceActivity,
};
use std::sync::{Arc, Mutex};

struct ObservedBatchVectorizer {
    activity: Arc<WorkspaceActivity>,
    snapshots: Arc<Mutex<Vec<ActivitySnapshot>>>,
    malformed: bool,
}
impl ArtifactTextVectorizer for ObservedBatchVectorizer {
    fn vectorize_artifact_text(
        &self,
        text: &str,
        task: lumvise_db_core::TextEmbeddingTask,
    ) -> lumvise_db_core::Result<ArtifactTextVector> {
        // Provider installation probes identity once; actual worker items must use the batch port.
        assert_eq!(text, "lumvise-vector-engine-identity-probe");
        TestVectorizer.vectorize_artifact_text(text, task)
    }
    fn vectorize_artifact_texts(
        &self,
        texts: &[&str],
        task: lumvise_db_core::TextEmbeddingTask,
    ) -> lumvise_db_core::Result<Vec<ArtifactTextVector>> {
        assert_eq!(task, lumvise_db_core::TextEmbeddingTask::Document);
        self.snapshots
            .lock()
            .unwrap()
            .push(
                self.activity
                    .wait_for_changes("/repo", None, std::time::Duration::ZERO),
            );
        if self.malformed {
            return Ok(Vec::new());
        }
        texts
            .iter()
            .map(|text| TestVectorizer.vectorize_artifact_text(text, task))
            .collect()
    }
}

struct ObservingVectorPersistence {
    inner: Arc<dyn lumvise_db_core::SemanticPersistence>,
    activity: Arc<WorkspaceActivity>,
    publication: Arc<Mutex<Vec<ActivityEntry>>>,
}
impl lumvise_db_core::SemanticPersistence for ObservingVectorPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::Result<SemanticResult> {
        if matches!(
            operation,
            SemanticOperation::StoreElementNameVectors { .. }
                | SemanticOperation::StoreArtifactTextVectors { .. }
        ) {
            self.publication.lock().unwrap().extend(
                self.activity
                    .wait_for_changes("/repo", None, std::time::Duration::ZERO)
                    .entries
                    .unwrap(),
            );
        }
        self.inner.execute(operation, control)
    }
    fn readiness(&self) -> lumvise_db_core::Result<lumvise_db_core::SemanticReadiness> {
        self.inner.readiness()
    }
}

struct BatchFixture {
    app: crate::AppCore,
    snapshots: Arc<Mutex<Vec<ActivitySnapshot>>>,
    publication: Arc<Mutex<Vec<ActivityEntry>>>,
}
impl BatchFixture {
    fn new(malformed: bool) -> Self {
        let mut app = crate::AppCore::in_memory().unwrap();
        seed_vectors(&app);
        let activity = app.plugin_host_services.activity();
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let publication = Arc::new(Mutex::new(Vec::new()));
        app.plugin_endpoints()
            .set_plugin_vectorizer(Box::new(ObservedBatchVectorizer {
                activity: activity.clone(),
                snapshots: snapshots.clone(),
                malformed,
            }))
            .unwrap();
        app.semantic = Arc::new(ObservingVectorPersistence {
            inner: app.semantic.clone(),
            activity,
            publication: publication.clone(),
        });
        Self {
            app,
            snapshots,
            publication,
        }
    }
}

fn seed_vectors(app: &crate::AppCore) {
    execute(
        app,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element("Named section")],
            relationships: vec![],
        },
    );
    for number in 0..17 {
        let mut record = artifact("Some documentation");
        record.artifact_id = format!("artifact-{number}");
        execute(
            app,
            SemanticOperation::UpsertArtifact {
                artifact: record,
                media_type: "text/plain".into(),
            },
        );
    }
}

#[test]
fn worker_reports_actual_batch_progress_then_saving_before_successful_publication() {
    let fixture = BatchFixture::new(false);
    let report = fixture
        .app
        .plugin_endpoints()
        .run_change_hook_cycle(1)
        .unwrap();
    assert!(report.hooks.iter().any(|run| run.acknowledged));
    let snapshots = fixture.snapshots.lock().unwrap();
    assert_eq!(snapshots.len(), 2);
    let first = &snapshots[0].entries.as_ref().unwrap()[0];
    let second = &snapshots[1].entries.as_ref().unwrap()[0];
    assert_eq!(first.progress.as_ref().unwrap().total, 18);
    assert_eq!(first.progress.as_ref().unwrap().completed, 0);
    assert_eq!(second.progress.as_ref().unwrap().completed, 16);
    assert_eq!(second.status, ActivityStatus::Running);
    let publication = fixture.publication.lock().unwrap();
    assert_eq!(publication.len(), 2);
    assert!(
        publication
            .iter()
            .all(|entry| entry.status == ActivityStatus::Running
                && entry.detail.as_deref() == Some("Saving vectors")
                && entry.progress.as_ref().unwrap().completed == 18)
    );
    let final_state = fixture
        .app
        .plugin_host_services
        .activity()
        .wait_for_changes("/repo", None, std::time::Duration::ZERO);
    assert_eq!(
        final_state.entries.unwrap()[0].status,
        ActivityStatus::Succeeded
    );
}

#[test]
fn malformed_batch_is_failed_without_saving_or_acknowledging_work() {
    let fixture = BatchFixture::new(true);
    let report = fixture
        .app
        .plugin_endpoints()
        .run_change_hook_cycle(1)
        .unwrap();
    assert!(report.hooks.is_empty());
    assert!(fixture.publication.lock().unwrap().is_empty());
    let final_state = fixture
        .app
        .plugin_host_services
        .activity()
        .wait_for_changes("/repo", None, std::time::Duration::ZERO);
    let entry = &final_state.entries.unwrap()[0];
    assert_eq!(entry.status, ActivityStatus::Failed);
    assert_eq!(entry.progress.as_ref().unwrap().completed, 0);
    let SemanticResult::ChangeHookRegistrations(hooks) =
        execute(&fixture.app, SemanticOperation::ChangeHookRegistrations)
    else {
        panic!("hook registrations")
    };
    assert_eq!(
        hooks
            .iter()
            .find(|hook| hook.hook_name.starts_with(VECTOR_HOOK_PREFIX))
            .unwrap()
            .watermark,
        0
    );
}
