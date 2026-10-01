use crate::local::clock::Clock;
use crate::local::grafeo::change_hooks::ChangeBuffer;
use crate::local::grafeo::graph_store::GraphStore;
use crate::local::grafeo::semantic_storage::SemanticStorage;
use crate::local::sql::connections::SqlConnections;
use std::sync::{Arc, atomic::AtomicU64};
use tokio::sync::broadcast;

pub struct StorageManager<'db> {
    conn: &'db SqlConnections,
    graph: &'db GraphStore,
    storage_change_generation: &'db Arc<AtomicU64>,
    storage_change_signal: &'db Arc<(std::sync::Mutex<u64>, std::sync::Condvar)>,
    storage_change_sender: &'db Arc<broadcast::Sender<u64>>,
    change_buffer: &'db Arc<std::sync::Mutex<ChangeBuffer>>,
    clock: &'db Arc<dyn Clock>,
}

impl<'db> StorageManager<'db> {
    pub(crate) fn new(
        conn: &'db SqlConnections,
        graph: &'db GraphStore,
        storage_change_generation: &'db Arc<AtomicU64>,
        storage_change_signal: &'db Arc<(std::sync::Mutex<u64>, std::sync::Condvar)>,
        storage_change_sender: &'db Arc<broadcast::Sender<u64>>,
        change_buffer: &'db Arc<std::sync::Mutex<ChangeBuffer>>,
        clock: &'db Arc<dyn Clock>,
    ) -> Self {
        Self {
            conn,
            graph,
            storage_change_generation,
            storage_change_signal,
            storage_change_sender,
            clock,
            change_buffer,
        }
    }

    /// Returns the semantic storage repository.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// let _storage = db.storage_manager().semantic_storage();
    /// ```
    pub fn semantic_storage(&self) -> SemanticStorage<'db> {
        SemanticStorage::new(
            self.conn,
            self.graph,
            self.storage_change_generation,
            self.storage_change_signal,
            self.storage_change_sender,
            self.change_buffer,
            self.clock,
        )
    }

    /// Returns (or creates) the durable project lineage identity.
    pub fn project_identity(&self, project_root: &str) -> crate::Result<crate::ProjectIdentity> {
        crate::local::sql::project_lineages::ProjectLineageRepository::new(self.conn)
            .get_or_create(project_root)
    }
}
