use super::element_lookup::SemanticElementLookup;
use crate::{
    DbError, Result, SemanticElement, SemanticGraphProjection, SemanticGraphProjectionRequest,
};
use grafeo::{EdgeId, GrafeoDB, NodeId, Session, Value as GrafeoValue};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

pub(crate) struct GraphStore {
    database: Arc<GrafeoDB>,
    graph_gate: RwLock<()>,
    write_admission: Mutex<()>,
    structure_sync: Mutex<()>,
    /// Even values represent a stable graph snapshot; an odd value means a
    /// writer owns the gate. A conditional writer validates the exact even
    /// value before it opens its Grafeo transaction.
    revision: AtomicU64,
    committed_semantic_entities: AtomicUsize,
    last_checkpoint_error: Mutex<Option<String>>,
    work: GraphWork,
    projection_cache: Mutex<VecDeque<CachedProjection>>,
    element_lookup: Mutex<SemanticElementLookup>,
    /// A Grafeo commit whose SQL publication failed leaves graph bytes ahead
    /// of the SQL revision; stable reads stay closed until a later publication
    /// succeeds over the current graph.
    semantic_publication_failure: Mutex<Option<String>>,
}

const SEMANTIC_ENTITIES_PER_CHECKPOINT: usize = 1_000;
const PROJECTION_CACHE_CAPACITY: usize = 8;

struct CachedProjection {
    commit_version: i64,
    request: SemanticGraphProjectionRequest,
    projection: SemanticGraphProjection,
}

pub(crate) struct VersionedGraphPlan<T> {
    revision: u64,
    value: T,
}

pub(crate) enum ConditionalGraphWrite<T> {
    Applied(T),
    Stale,
}

/// Result of work performed after a Grafeo transaction has committed while the
/// graph writer gate is still held.
pub(crate) enum GraphPostCommit<T> {
    Completed(T),
    Failed(DbError),
}

#[derive(Default)]
struct GraphWork {
    #[cfg(test)]
    gate_find_nodes_by_property: AtomicUsize,
    #[cfg(test)]
    stable_read_attempts: AtomicUsize,
    #[cfg(test)]
    checkpoint_probe: Mutex<Option<CheckpointProbe>>,
}

#[cfg(test)]
struct CheckpointProbe {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct GraphGateWork {
    pub(crate) find_nodes_by_property: usize,
    pub(crate) stable_read_attempts: usize,
}

impl<T> VersionedGraphPlan<T> {
    pub(crate) fn value(&self) -> &T {
        &self.value
    }

    pub(crate) fn into_parts(self) -> (u64, T) {
        (self.revision, self.value)
    }
}

pub(crate) struct GraphTransaction<'graph> {
    _database: &'graph GrafeoDB,
    session: Session,
    semantic_node_ids: RefCell<BTreeMap<String, NodeId>>,
    #[cfg(test)]
    work: &'graph GraphWork,
}

impl GraphStore {
    pub(crate) fn new(database: GrafeoDB) -> Self {
        Self {
            database: Arc::new(database),
            graph_gate: RwLock::new(()),
            write_admission: Mutex::new(()),
            structure_sync: Mutex::new(()),
            revision: AtomicU64::new(0),
            committed_semantic_entities: AtomicUsize::new(0),
            last_checkpoint_error: Mutex::new(None),
            projection_cache: Mutex::new(VecDeque::with_capacity(PROJECTION_CACHE_CAPACITY)),
            element_lookup: Mutex::new(SemanticElementLookup::default()),
            semantic_publication_failure: Mutex::new(None),
            work: GraphWork::default(),
        }
    }

    // One full diff at a time: concurrent planners otherwise repeatedly invalidate
    // each other. Readers still use the independent graph gate.
    pub(crate) fn lock_structure_sync(
        &self,
        control: &lumvise_resource_routing::InvocationControl,
    ) -> Result<std::sync::MutexGuard<'_, ()>> {
        loop {
            crate::local::persistence::verify_control(control)?;
            match self.structure_sync.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
            }
        }
    }

    pub(crate) fn read<R>(&self, read: impl FnOnce(&GrafeoDB) -> R) -> R {
        let graph = self.snapshot();
        read(graph.as_ref())
    }

    pub(crate) fn snapshot(&self) -> Arc<GrafeoDB> {
        Arc::clone(&self.database)
    }

    pub(super) fn search_element_candidates(
        &self,
        project_root: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SemanticElement>> {
        self.stable_read(|graph| {
            let revision = self.revision.load(Ordering::Acquire);
            let ids = self
                .element_lookup
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .select(graph, revision, project_root, query, limit);
            Ok(super::graph_rows::selective_elements(
                graph,
                &ids,
                project_root,
                false,
            ))
        })
    }

    /// Requires the caller's stable graph lease so count and records share a revision.
    pub(super) fn scope_element_count(
        &self,
        graph: &GrafeoDB,
        project_root: &str,
        include_inactive: bool,
    ) -> usize {
        self.element_lookup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .scope_count(
                graph,
                self.revision.load(Ordering::Acquire),
                project_root,
                include_inactive,
            )
    }

    /// Requires the caller's stable graph lease; selected IDs and hydration
    /// must observe the same revision as the surrounding scoped result.
    pub(super) fn location_elements(
        &self,
        graph: &GrafeoDB,
        project_root: &str,
        path: Option<&str>,
        line: i64,
        include_inactive: bool,
    ) -> Vec<SemanticElement> {
        let Some(path) = path else {
            return Vec::new();
        };
        let ids = self
            .element_lookup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .select_path(
                graph,
                self.revision.load(Ordering::Acquire),
                project_root,
                path,
                line,
                include_inactive,
            );
        super::graph_rows::selective_elements(graph, &ids, Some(project_root), true)
    }

    /// Materializes a complete read while graph writers are excluded.
    ///
    /// The callback must return owned data. The graph gate is released before
    /// the result is handed to callers. A failed cross-store publication also
    /// closes this seam until a later successful publication re-establishes a
    /// coherent graph/SQL snapshot.
    pub(crate) fn stable_read<R>(&self, read: impl FnOnce(&GrafeoDB) -> Result<R>) -> Result<R> {
        let wait_started = Instant::now();
        #[cfg(test)]
        self.work
            .stable_read_attempts
            .fetch_add(1, Ordering::Relaxed);
        let _reader = self
            .graph_gate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        metrics::histogram!("lumvise_db_writer_gate_wait_seconds", "gate" => "graph")
            .record(wait_started.elapsed().as_secs_f64());
        let publication_failure = self
            .semantic_publication_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(message) = publication_failure {
            return Err(DbError::Grafeo(format!(
                "semantic graph publication failed: {message}"
            )));
        }
        read(self.database.as_ref())
    }

    pub(crate) fn cached_projection(
        &self,
        commit_version: i64,
        request: &SemanticGraphProjectionRequest,
    ) -> Option<SemanticGraphProjection> {
        let mut cache = self
            .projection_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self
            .semantic_publication_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
        {
            return None;
        }
        let position = cache.iter().position(|entry| {
            entry.commit_version == commit_version && entry.request == *request
        })?;
        let entry = cache.remove(position)?;
        let projection = entry.projection.clone();
        cache.push_back(entry);
        Some(projection)
    }

    pub(crate) fn cache_projection(
        &self,
        commit_version: i64,
        request: SemanticGraphProjectionRequest,
        projection: SemanticGraphProjection,
    ) {
        let mut cache = self
            .projection_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self
            .semantic_publication_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
        {
            return;
        }
        if let Some(position) = cache
            .iter()
            .position(|entry| entry.commit_version == commit_version && entry.request == request)
        {
            cache.remove(position);
        }
        if cache.len() == PROJECTION_CACHE_CAPACITY {
            cache.pop_front();
        }
        cache.push_back(CachedProjection {
            commit_version,
            request,
            projection,
        });
    }

    /// Runs a graph-dependent planner against an unlocked snapshot. It retries
    /// once when a concurrent writer advances the revision, then returns the
    /// observed revision for a conditional writer to validate. Returning a
    /// stale plan after bounded planning prevents a planner that continuously
    /// induces writes from spinning forever outside the writer gate.
    pub(crate) fn plan<T>(
        &self,
        plan: impl Fn(&GrafeoDB) -> Result<T>,
    ) -> Result<VersionedGraphPlan<T>> {
        const MAX_PLANNING_ATTEMPTS: usize = 2;
        let mut attempts = 0;

        loop {
            let revision = self.revision.load(Ordering::Acquire);
            if !revision.is_multiple_of(2) {
                std::hint::spin_loop();
                continue;
            }
            let value = plan(self.database.as_ref())?;
            attempts += 1;
            if self.revision.load(Ordering::Acquire) == revision
                || attempts == MAX_PLANNING_ATTEMPTS
            {
                return Ok(VersionedGraphPlan { revision, value });
            }
        }
    }

    /// Checkpoints a published graph while allowing stable readers to proceed.
    pub(crate) fn checkpoint(&self) -> Result<()> {
        self.checkpoint_published_graph(false)
    }

    fn checkpoint_published_graph(&self, automatic: bool) -> Result<()> {
        // Writers wait here, not on the RwLock: a queued writer must not block new readers.
        let _admission = self
            .write_admission
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _snapshot = self.graph_gate.read().unwrap_or_else(|p| p.into_inner());
        if automatic
            && self.committed_semantic_entities.load(Ordering::Acquire)
                < SEMANTIC_ENTITIES_PER_CHECKPOINT
        {
            return Ok(());
        }
        #[cfg(test)]
        if let Some(probe) = self.work.checkpoint_probe.lock().unwrap().take() {
            probe.entered.send(()).unwrap();
            probe.release.recv().unwrap();
        }
        self.database
            .wal_checkpoint()
            .map_err(|error| DbError::Grafeo(format!("wal_checkpoint failed: {error}")))?;
        self.committed_semantic_entities.store(0, Ordering::Release);
        *self
            .last_checkpoint_error
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        Ok(())
    }

    fn record_semantic_publication_while_writer_gate_held(&self, changed_entities: usize) -> bool {
        if changed_entities == 0 {
            return false;
        }
        self.projection_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        let previous = self.committed_semantic_entities.load(Ordering::Acquire);
        let current = previous.saturating_add(changed_entities);
        self.committed_semantic_entities
            .store(current, Ordering::Release);
        current >= SEMANTIC_ENTITIES_PER_CHECKPOINT
    }

    fn automatic_checkpoint(&self) {
        match self.checkpoint_published_graph(true) {
            Ok(()) => {}
            Err(error) => {
                let message = error.to_string();
                *self
                    .last_checkpoint_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(message.clone());
                metrics::counter!(
                    "lumvise_db_wal_checkpoint_failures_total",
                    "reason" => "automatic"
                )
                .increment(1);
                tracing::error!(
                    changed_entities = self.committed_semantic_entities.load(Ordering::Acquire),
                    error = %message,
                    "automatic semantic graph checkpoint failed after durable publication"
                );
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn write<R>(
        &self,
        write: impl FnOnce(&GraphTransaction<'_>) -> Result<R>,
    ) -> Result<R> {
        match self.write_inner(None, write)? {
            ConditionalGraphWrite::Applied(value) => Ok(value),
            ConditionalGraphWrite::Stale => {
                unreachable!("unconditional graph write cannot be stale")
            }
        }
    }

    /// Applies a plan only when no writer has changed the graph since its
    /// planner observed it. A stale plan never begins a Grafeo transaction.
    #[cfg(test)]
    pub(crate) fn write_if_revision<R>(
        &self,
        expected_revision: u64,
        write: impl FnOnce(&GraphTransaction<'_>) -> Result<R>,
    ) -> Result<ConditionalGraphWrite<R>> {
        self.write_inner(Some(expected_revision), write)
    }

    /// Commits the Grafeo transaction and runs `after_commit` before making the
    /// graph revision visible to versioned readers. Errors from
    /// `after_commit` mean the graph is already durable; the projection cache
    /// is invalidated and stable readers remain closed until a later successful
    /// publication catches SQL up to the current graph.
    pub(crate) fn write_if_revision_semantic_through_post_commit<R, T>(
        &self,
        expected_revision: u64,
        write: impl FnOnce(&GraphTransaction<'_>) -> Result<R>,
        after_commit: impl FnOnce(R) -> Result<T>,
        mutation_count: impl FnOnce(&T) -> usize,
    ) -> Result<ConditionalGraphWrite<GraphPostCommit<T>>> {
        self.write_inner_through_post_commit(
            Some(expected_revision),
            write,
            after_commit,
            mutation_count,
        )
    }

    /// Runs the same publication path without a plan revision check.
    #[cfg(test)]
    pub(crate) fn write_semantic_through_post_commit<R, T>(
        &self,
        write: impl FnOnce(&GraphTransaction<'_>) -> Result<R>,
        after_commit: impl FnOnce(R) -> Result<T>,
        mutation_count: impl FnOnce(&T) -> usize,
    ) -> Result<GraphPostCommit<T>> {
        match self.write_inner_through_post_commit(None, write, after_commit, mutation_count)? {
            ConditionalGraphWrite::Applied(result) => Ok(result),
            ConditionalGraphWrite::Stale => {
                unreachable!("unconditional graph write cannot be stale")
            }
        }
    }

    #[cfg(test)]
    fn write_inner<R>(
        &self,
        expected_revision: Option<u64>,
        write: impl FnOnce(&GraphTransaction<'_>) -> Result<R>,
    ) -> Result<ConditionalGraphWrite<R>> {
        let wait_started = Instant::now();
        let _admission = self
            .write_admission
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _writer = self
            .graph_gate
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        metrics::histogram!("lumvise_db_writer_gate_wait_seconds", "gate" => "graph")
            .record(wait_started.elapsed().as_secs_f64());
        let revision = self.revision.load(Ordering::Acquire);
        if expected_revision.is_some_and(|expected| expected != revision)
            || !revision.is_multiple_of(2)
        {
            return Ok(ConditionalGraphWrite::Stale);
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
        let graph = self.snapshot();
        let transaction = match GraphTransaction::begin(graph.as_ref(), &self.work) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.revision.fetch_add(1, Ordering::Release);
                return Err(error);
            }
        };
        let write_result = write(&transaction);
        let result = transaction.finish(write_result);
        self.revision.fetch_add(1, Ordering::Release);
        result.map(ConditionalGraphWrite::Applied)
    }

    fn write_inner_through_post_commit<R, T>(
        &self,
        expected_revision: Option<u64>,
        write: impl FnOnce(&GraphTransaction<'_>) -> Result<R>,
        after_commit: impl FnOnce(R) -> Result<T>,
        mutation_count: impl FnOnce(&T) -> usize,
    ) -> Result<ConditionalGraphWrite<GraphPostCommit<T>>> {
        let wait_started = Instant::now();
        let _admission = self
            .write_admission
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _writer = self
            .graph_gate
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        metrics::histogram!("lumvise_db_writer_gate_wait_seconds", "gate" => "graph")
            .record(wait_started.elapsed().as_secs_f64());
        let revision = self.revision.load(Ordering::Acquire);
        if expected_revision.is_some_and(|expected| expected != revision)
            || !revision.is_multiple_of(2)
        {
            return Ok(ConditionalGraphWrite::Stale);
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
        let graph = self.snapshot();
        let transaction = match GraphTransaction::begin(graph.as_ref(), &self.work) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.revision.fetch_add(1, Ordering::Release);
                return Err(error);
            }
        };
        let write_result = write(&transaction);
        let write_result = transaction.finish(write_result);
        let mut checkpoint_due = false;
        let result = match write_result {
            Ok(value) => match after_commit(value) {
                Ok(value) => {
                    self.clear_semantic_publication_failure_while_writer_gate_held();
                    checkpoint_due = self
                        .record_semantic_publication_while_writer_gate_held(mutation_count(&value));
                    Ok(ConditionalGraphWrite::Applied(GraphPostCommit::Completed(
                        value,
                    )))
                }
                Err(error) => {
                    self.invalidate_projection_cache_after_publication_failure(&error);
                    Ok(ConditionalGraphWrite::Applied(GraphPostCommit::Failed(
                        error,
                    )))
                }
            },
            Err(error) => Err(error),
        };
        self.revision.fetch_add(1, Ordering::Release);
        drop(_writer);
        drop(_admission);
        if checkpoint_due {
            self.automatic_checkpoint();
        }
        result
    }

    fn invalidate_projection_cache_after_publication_failure(&self, error: &DbError) {
        self.projection_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        *self
            .semantic_publication_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.to_string());
    }

    fn clear_semantic_publication_failure_while_writer_gate_held(&self) {
        *self
            .semantic_publication_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    #[cfg(test)]
    pub(crate) fn reset_gate_work(&self) {
        self.work
            .gate_find_nodes_by_property
            .store(0, Ordering::Relaxed);
        self.work.stable_read_attempts.store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn gate_work(&self) -> GraphGateWork {
        GraphGateWork {
            find_nodes_by_property: self
                .work
                .gate_find_nodes_by_property
                .load(Ordering::Relaxed),
            stable_read_attempts: self.work.stable_read_attempts.load(Ordering::Relaxed),
        }
    }

    #[cfg(test)]
    pub(crate) fn committed_semantic_entities(&self) -> usize {
        self.committed_semantic_entities.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn projection_cache_len(&self) -> usize {
        self.projection_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

impl<'graph> GraphTransaction<'graph> {
    fn begin(database: &'graph GrafeoDB, _work: &'graph GraphWork) -> Result<Self> {
        let mut session = database.session();
        session
            .begin_transaction()
            .map_err(|error| DbError::Grafeo(error.to_string()))?;
        Ok(Self {
            _database: database,
            session,
            semantic_node_ids: RefCell::new(BTreeMap::new()),
            #[cfg(test)]
            work: _work,
        })
    }

    fn finish<R>(mut self, result: Result<R>) -> Result<R> {
        match result {
            Ok(value) => {
                self.session
                    .commit()
                    .map_err(|error| DbError::Grafeo(error.to_string()))?;
                Ok(value)
            }
            Err(error) => {
                if let Err(rollback) = self.session.rollback() {
                    return Err(DbError::Grafeo(format!(
                        "write failed with {error}; rollback also failed with {rollback}"
                    )));
                }
                Err(error)
            }
        }
    }

    pub(crate) fn create_node_with_props<'a>(
        &self,
        labels: &[&str],
        properties: impl IntoIterator<Item = (&'a str, GrafeoValue)>,
    ) -> Result<NodeId> {
        self.session
            .create_node_with_props(labels, properties)
            .map_err(|error| DbError::Grafeo(error.to_string()))
    }

    pub(crate) fn create_semantic_node<'a>(
        &self,
        semantic_element_id: &str,
        properties: impl IntoIterator<Item = (&'a str, GrafeoValue)>,
    ) -> Result<NodeId> {
        let node_id = self.create_node_with_props(&["SemanticElement"], properties)?;
        self.semantic_node_ids
            .borrow_mut()
            .insert(semantic_element_id.to_string(), node_id);
        Ok(node_id)
    }

    pub(crate) fn create_edge_with_props<'a>(
        &self,
        source: NodeId,
        target: NodeId,
        edge_type: &str,
        properties: impl IntoIterator<Item = (&'a str, GrafeoValue)>,
    ) -> Result<EdgeId> {
        self.session
            .create_edge_with_props(source, target, edge_type, properties)
            .map_err(|error| DbError::Grafeo(error.to_string()))
    }

    pub(crate) fn delete_node(&self, node_id: NodeId) -> bool {
        self.session.delete_node(node_id)
    }

    pub(crate) fn delete_edge(&self, edge_id: EdgeId) -> bool {
        self.session.delete_edge(edge_id)
    }

    #[cfg(test)]
    pub(crate) fn find_nodes_by_property(
        &self,
        property: &str,
        value: &GrafeoValue,
    ) -> Vec<NodeId> {
        self.work
            .gate_find_nodes_by_property
            .fetch_add(1, Ordering::Relaxed);
        self._database.find_nodes_by_property(property, value)
    }

    pub(crate) fn get_node(&self, node_id: NodeId) -> Option<grafeo_core::graph::lpg::Node> {
        self.session.get_node(node_id)
    }

    pub(crate) fn get_edge(&self, edge_id: EdgeId) -> Option<grafeo_core::graph::lpg::Edge> {
        self.session.get_edge(edge_id)
    }

    pub(crate) fn set_node_property(
        &self,
        node_id: NodeId,
        property: &str,
        value: GrafeoValue,
    ) -> Result<()> {
        self.session
            .set_node_property(node_id, property, value)
            .map_err(|error| DbError::Grafeo(error.to_string()))
    }

    pub(crate) fn set_edge_property(
        &self,
        edge_id: EdgeId,
        property: &str,
        value: GrafeoValue,
    ) -> Result<()> {
        self.session
            .set_edge_property(edge_id, property, value)
            .map_err(|error| DbError::Grafeo(error.to_string()))
    }

    #[cfg(test)]
    pub(crate) fn iter_edges(&self) -> impl Iterator<Item = grafeo_core::graph::lpg::Edge> + '_ {
        self._database.iter_edges()
    }

    #[cfg(test)]
    pub(crate) fn semantic_node_ids(&self) -> BTreeMap<String, NodeId> {
        self.semantic_node_ids.borrow().clone()
    }

    pub(crate) fn semantic_node_id(&self, semantic_element_id: &str) -> Option<NodeId> {
        self.semantic_node_ids
            .borrow()
            .get(semantic_element_id)
            .copied()
    }
}

#[cfg(test)]
mod tests;
