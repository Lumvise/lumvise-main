//! Owns exclusive graph publication. SQL participants acquire their writer only
//! inside `publish_exclusively`; transaction and checkpoint details stay here.

use super::{ConditionalGraphWrite, GraphPostCommit, GraphState, GraphTransaction};
use crate::Result;
use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::{MutexGuard, RwLockWriteGuard};
use std::time::Instant;

pub(crate) struct GraphPublication<'graph> {
    store: &'graph GraphState,
    checkpoint_due: Cell<bool>,
}

impl GraphState {
    /// Acquires the graph gate before a participant can acquire SQL.
    /// For example, semantic publication opens its SQL transaction in this callback.
    pub(crate) fn publish_exclusively<R>(
        &self,
        publish: impl FnOnce(&GraphPublication<'_>) -> Result<R>,
    ) -> Result<R> {
        let (admission, writer) = self.acquire_publication_gate();
        let publication = GraphPublication {
            store: self,
            checkpoint_due: Cell::new(false),
        };
        let result = publish(&publication);
        drop(writer);
        drop(admission);
        if publication.checkpoint_due.get() {
            self.schedule_automatic_checkpoint();
        }
        result
    }

    fn acquire_publication_gate(&self) -> (MutexGuard<'_, ()>, RwLockWriteGuard<'_, ()>) {
        #[cfg(test)]
        self.work
            .publication_attempts
            .fetch_add(1, Ordering::Release);
        let wait_started = Instant::now();
        let admission = self
            .write_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let writer = self
            .graph_gate
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        metrics::histogram!("lumvise_db_writer_gate_wait_seconds", "gate" => "graph")
            .record(wait_started.elapsed().as_secs_f64());
        (admission, writer)
    }
}

impl GraphPublication<'_> {
    /// Commits one graph transaction and its SQL publication under this lease.
    /// For example, a stale prepared revision returns before opening SQL.
    ///
    /// Commits Grafeo and runs `after_commit` before making the graph revision
    /// visible to versioned readers. Errors from `after_commit` mean the graph
    /// is already durable; the projection cache is invalidated and stable reads
    /// remain closed until a later publication catches SQL up to the graph.
    pub(crate) fn commit<R, T>(
        &self,
        expected_revision: Option<u64>,
        write: impl FnOnce(&GraphTransaction<'_>) -> Result<R>,
        after_commit: impl FnOnce(R) -> Result<T>,
        mutation_count: impl FnOnce(&T) -> usize,
    ) -> Result<ConditionalGraphWrite<GraphPostCommit<T>>> {
        if !self.begin_revision(expected_revision) {
            return Ok(ConditionalGraphWrite::Stale);
        }
        let result = self.commit_graph(write, after_commit, mutation_count);
        self.store.revision.fetch_add(1, Ordering::Release);
        result.map(ConditionalGraphWrite::Applied)
    }

    fn begin_revision(&self, expected_revision: Option<u64>) -> bool {
        let revision = self.store.revision.load(Ordering::Acquire);
        if expected_revision.is_some_and(|expected| expected != revision)
            || !revision.is_multiple_of(2)
        {
            return false;
        }
        self.store.revision.fetch_add(1, Ordering::AcqRel);
        true
    }

    fn commit_graph<R, T>(
        &self,
        write: impl FnOnce(&GraphTransaction<'_>) -> Result<R>,
        after_commit: impl FnOnce(R) -> Result<T>,
        mutation_count: impl FnOnce(&T) -> usize,
    ) -> Result<GraphPostCommit<T>> {
        let graph = self.store.snapshot();
        let transaction = GraphTransaction::begin(graph.as_ref(), &self.store.work)?;
        let write_result = write(&transaction);
        if transaction.elements_changed.get() {
            self.store.element_revision.fetch_add(1, Ordering::Release);
        }
        let value = self.store.finish_indexed(transaction, write_result)?;
        Ok(self.publish_sql(value, after_commit, mutation_count))
    }

    fn publish_sql<R, T>(
        &self,
        value: R,
        after_commit: impl FnOnce(R) -> Result<T>,
        mutation_count: impl FnOnce(&T) -> usize,
    ) -> GraphPostCommit<T> {
        let value = match after_commit(value) {
            Ok(value) => value,
            Err(error) => {
                self.store
                    .invalidate_projection_cache_after_publication_failure(&error);
                return GraphPostCommit::Failed(error);
            }
        };
        self.store
            .clear_semantic_publication_failure_while_writer_gate_held();
        let due = self
            .store
            .record_semantic_publication_while_writer_gate_held(mutation_count(&value));
        self.checkpoint_due.set(self.checkpoint_due.get() || due);
        GraphPostCommit::Completed(value)
    }
}
