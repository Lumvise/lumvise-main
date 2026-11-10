//! Embedding producers report actual batch work; the existing activity stream owns delivery.
use super::*;

pub(crate) struct EmbeddingBatch {
    activity: Arc<WorkspaceActivity>,
    id: String,
    completed: usize,
    total: usize,
}

impl WorkspaceActivity {
    /// Starts observed vector work, e.g. `activity.track_embeddings("/repo", 50)`.
    pub(crate) fn track_embeddings(self: &Arc<Self>, root: &str, total: usize) -> EmbeddingBatch {
        let id = format!("embedding-{}", uuid::Uuid::new_v4());
        let entry = embedding_entry(id.clone(), root, total);
        self.record_embedding(entry);
        EmbeddingBatch {
            activity: Arc::clone(self),
            id,
            completed: 0,
            total,
        }
    }

    fn record_embedding(&self, entry: ActivityEntry) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.entries.retain(|previous| {
            previous.kind != ActivityKind::Embedding
                || previous.project_root != entry.project_root
                || !previous.status.is_terminal()
        });
        state.entries.push_back(entry);
        self.publish(&mut state);
    }

    fn embedding_progress(&self, id: &str, completed: usize, total: usize) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| entry.id == id && !entry.status.is_terminal())
        else {
            return;
        };
        entry.progress = Some(ActivityProgress { completed, total });
        self.publish(&mut state);
    }
}

fn embedding_entry(id: String, root: &str, total: usize) -> ActivityEntry {
    ActivityEntry {
        id,
        project_root: Some(root.into()),
        title: "Embedding vectors".into(),
        kind: ActivityKind::Embedding,
        status: ActivityStatus::Running,
        semantic_element_id: None,
        document_id: None,
        detail: None,
        progress: Some(ActivityProgress {
            completed: 0,
            total,
        }),
    }
}

impl EmbeddingBatch {
    pub(crate) fn advance(&mut self, count: usize) {
        assert!(
            count <= self.total - self.completed,
            "completed increment {count}; expected at most {} remaining vectors",
            self.total - self.completed
        );
        self.completed += count;
        self.activity
            .embedding_progress(&self.id, self.completed, self.total);
    }

    pub(crate) fn saving(&self) {
        self.activity.update(
            &self.id,
            ActivityStatus::Running,
            Some("Saving vectors".into()),
        );
    }

    pub(crate) fn succeeded(&self) {
        self.activity
            .update(&self.id, ActivityStatus::Succeeded, None);
    }
}

impl Drop for EmbeddingBatch {
    fn drop(&mut self) {
        // Never copy provider errors here: they can contain the source being embedded.
        self.activity.update(
            &self.id,
            ActivityStatus::Failed,
            Some("Embedding batch failed; will retry".into()),
        );
    }
}
