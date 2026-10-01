//! Desktop storage wake adapter. Only `wait_for_revision` crosses into the bridge;
//! cursor validation and persistence result decoding remain internal here.

use lumvise_db_core::{SemanticOperation, SemanticPersistence, SemanticResult};
use lumvise_resource_routing::InvocationControl;

/// Returns the changed head, or the same revision after a bounded idle wait.
/// Example: `wait_for_revision(persistence, last_applied_revision)`.
pub(super) fn wait_for_revision(
    semantic: &dyn SemanticPersistence,
    after_revision: i64,
) -> Result<i64, String> {
    if after_revision < 0 {
        return Err(format!(
            "storage revision {after_revision}: expected a non-negative revision"
        ));
    }
    let head = execute_revision(semantic, SemanticOperation::SemanticRevision)?;
    if head != after_revision {
        return Ok(head);
    }
    execute_revision(
        semantic,
        SemanticOperation::WaitForSemanticRevision {
            after_revision,
            timeout_ms: Some(15_000),
        },
    )
}

fn execute_revision(
    semantic: &dyn SemanticPersistence,
    operation: SemanticOperation,
) -> Result<i64, String> {
    match semantic.execute(operation, &InvocationControl::sixty_seconds()) {
        Ok(SemanticResult::SemanticRevision { commit_version }) => Ok(commit_version),
        Ok(result) => Err(format!(
            "storage revision result {result:?}: expected SemanticRevision"
        )),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppCore, AppCoreDesktopBridge};
    use lumvise_db_core::SemanticElement;
    use lumvise_frontend_core::DesktopSemanticGraphBridge;
    use serde_json::json;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn storage_revision_rejects_negative_and_returns_reset_head() {
        let app = Arc::new(AppCore::in_memory().unwrap());
        let bridge = AppCoreDesktopBridge::without_runtime(app);
        let error = bridge.wait_for_storage_revision(-1).unwrap_err();
        assert!(error.contains("-1"));
        assert!(error.contains("non-negative"));
        assert_eq!(bridge.wait_for_storage_revision(i64::MAX).unwrap(), 0);
    }

    #[test]
    fn storage_revision_wait_wakes_after_a_semantic_commit() {
        let app = Arc::new(AppCore::in_memory().unwrap());
        let bridge = AppCoreDesktopBridge::without_runtime(Arc::clone(&app));
        let (completed, received) = mpsc::channel();
        let waiting = std::thread::spawn(move || {
            completed.send(bridge.wait_for_storage_revision(0)).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
        commit_element(&app);
        let head = received
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(head > 0);
        waiting.join().unwrap();
    }

    fn commit_element(app: &AppCore) {
        let element = SemanticElement {
            project_root: "/revision-test".into(),
            semantic_element_id: "changed-file".into(),
            semantic_source_id: "source".into(),
            path: "src/changed.rs".into(),
            element_kind: "file".into(),
            name: "changed.rs".into(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".into(),
            match_evidence: None,
            metadata: json!({}),
        };
        app.semantic
            .execute(
                SemanticOperation::SyncStructure {
                    project_root: element.project_root.clone(),
                    elements: vec![element],
                    relationships: Vec::new(),
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
    }
}
