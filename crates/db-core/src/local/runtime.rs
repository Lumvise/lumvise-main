use crate::SemanticRecoveryStatus;
use crate::local::clock::{Clock, SystemClock};
use crate::local::grafeo::change_hooks::{ChangeBuffer, ChangeHooks};
use crate::local::grafeo::graph_rows::configure_semantic_indexes;
use crate::local::grafeo::graph_store::GraphStore;
use crate::local::grafeo::storage_manager::StorageManager;
use crate::local::sql::artifact_blobs::ArtifactBlobRepository;
use crate::local::sql::commits::{
    latest_published_commit_version, recover_stale_pending_commits, resolve_failed_commits,
    semantic_recovery_status,
};
use crate::local::sql::connections::SqlConnections;
#[cfg(test)]
use crate::local::sql::plugin_data::PluginDataRepository;
#[cfg(test)]
use crate::local::sql::plugin_deliveries::PluginDeliveryRepository;
#[cfg(test)]
use crate::local::sql::plugin_settings::PluginSettingsRepository;
#[cfg(test)]
use crate::local::sql::settings::PersistentSettingsRepository;
use crate::{DbError, Result};
use grafeo::GrafeoDB;
use lumvise_resource_routing::InvocationControl;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

const STORAGE_CHANGE_BROADCAST_CAPACITY: usize = 1_024;

static IN_MEMORY_GRAPH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct DbCore {
    pub(crate) conn: Arc<SqlConnections>,
    pub(crate) graph: GraphStore,
    pub(crate) storage_change_generation: Arc<AtomicU64>,
    storage_change_signal: Arc<(Mutex<u64>, Condvar)>,
    storage_change_sender: Arc<tokio::sync::broadcast::Sender<u64>>,
    pub(crate) change_buffer: Arc<Mutex<ChangeBuffer>>,
    pub(crate) change_hook_operations: Mutex<()>,
    clock: Arc<dyn Clock>,
}

impl DbCore {
    /// Opens a DB Core database file and initializes the schema.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticPersistence, RelationalPersistence};
    ///
    /// let directory = tempfile::tempdir().unwrap();
    /// let persistence = LocalPersistence::open(directory.path().join("db.sqlite")).unwrap();
    /// assert!(SemanticPersistence::readiness(&persistence).unwrap().ready);
    /// assert!(RelationalPersistence::readiness(&persistence).unwrap().ready);
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_clock(path, Arc::new(SystemClock))
    }

    /// Opens DB Core with an injected clock for deterministic lease behavior.
    pub fn open_with_clock(path: impl AsRef<Path>, clock: Arc<dyn Clock>) -> Result<Self> {
        Self::open_composed(path, clock)
    }

    fn open_composed(path: impl AsRef<Path>, clock: Arc<dyn Clock>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        create_database_parent(&path)?;
        let conn = Arc::new(timed_startup_phase("sqlite_open", || {
            SqlConnections::open(&path)
        })?);
        let graph_path = graph_path_for_database(&path);
        let graph = open_graph_database(&graph_path)?;
        let graph_store = GraphStore::new(graph);
        timed_startup_phase("graph_checkpoint", || graph_store.checkpoint())?;
        {
            let write_conn = conn.write_conn();
            timed_startup_phase("pending_commit_recovery", || {
                recover_stale_pending_commits(&write_conn)
            })?;
            timed_startup_phase("failed_commit_resolution", || {
                resolve_failed_commits(&write_conn)
            })?;
        }
        let latest_published_revision = {
            let read_conn = conn.read_conn();
            latest_published_commit_version(&read_conn)?
        };
        let db = Self {
            conn,
            graph: graph_store,
            storage_change_generation: Arc::new(AtomicU64::new(0)),
            storage_change_signal: Arc::new((Mutex::new(0), Condvar::new())),
            storage_change_sender: Arc::new(
                tokio::sync::broadcast::channel(STORAGE_CHANGE_BROADCAST_CAPACITY).0,
            ),
            clock,
            change_buffer: Arc::new(Mutex::new(ChangeBuffer::new(latest_published_revision))),
            change_hook_operations: Mutex::new(()),
        };
        Ok(db)
    }

    /// Opens an in-memory DB Core database for deterministic tests.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticPersistence, RelationalPersistence};
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// assert!(SemanticPersistence::readiness(&persistence).unwrap().ready);
    /// assert!(RelationalPersistence::readiness(&persistence).unwrap().ready);
    /// ```
    pub fn in_memory() -> Result<Self> {
        Self::in_memory_with_clock(Arc::new(SystemClock))
    }

    /// Opens an in-memory database with an injected clock.
    pub fn in_memory_with_clock(clock: Arc<dyn Clock>) -> Result<Self> {
        let conn = Arc::new(SqlConnections::in_memory()?);
        let graph_path = in_memory_graph_path();
        let graph = open_graph_database(&graph_path)?;
        let graph_store = GraphStore::new(graph);
        graph_store.checkpoint()?;
        let db = Self {
            conn,
            graph: graph_store,
            storage_change_generation: Arc::new(AtomicU64::new(0)),
            storage_change_signal: Arc::new((Mutex::new(0), Condvar::new())),
            storage_change_sender: Arc::new(
                tokio::sync::broadcast::channel(STORAGE_CHANGE_BROADCAST_CAPACITY).0,
            ),
            clock,
            change_buffer: Arc::new(Mutex::new(ChangeBuffer::new(0))),
            change_hook_operations: Mutex::new(()),
        };
        Ok(db)
    }

    #[cfg(all(test, debug_assertions))]
    /// Holds the SQL writer connection while a test exercises concurrent readers.
    ///
    /// # Example
    ///
    /// ```ignore
    /// db.hold_sql_writer_for_test(|| {});
    /// ```
    pub fn hold_sql_writer_for_test<R>(&self, hold: impl FnOnce() -> R) -> R {
        let _conn = self.conn.write_conn();
        hold()
    }

    /// Returns the SQL persistent settings repository.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// db.persistent_settings().set_json("app", "theme", &serde_json::json!("dark")).unwrap();
    /// ```
    #[cfg(test)]
    pub fn persistent_settings(&self) -> PersistentSettingsRepository<'_> {
        PersistentSettingsRepository::new(&self.conn)
    }

    /// Returns the SQL plugin settings repository.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// db.plugin_settings().set_config("assistant", true, &serde_json::json!({})).unwrap();
    /// ```
    #[cfg(test)]
    pub fn plugin_settings(&self) -> PluginSettingsRepository<'_> {
        PluginSettingsRepository::new(&self.conn)
    }

    /// Returns the SQL plugin-owned data repository.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// db.plugin_data().ensure_table("builtin.assistant", "cache", &serde_json::json!({})).unwrap();
    /// ```
    #[cfg(test)]
    pub fn plugin_data(&self) -> PluginDataRepository<'_> {
        PluginDataRepository::new(&self.conn)
    }

    /// Returns durable compiled-plugin background registration and delivery state.
    ///
    /// # Example
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// assert!(db.plugin_deliveries().active_registrations().unwrap().is_empty());
    /// ```
    #[cfg(test)]
    pub fn plugin_deliveries(&self) -> PluginDeliveryRepository<'_> {
        PluginDeliveryRepository::new(&self.conn)
    }

    /// Returns the SQL artifact blob repository.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::ArtifactBlobPut {
    ///         content_ref: "blob://a".into(),
    ///         artifact_id: "a".into(),
    ///         media_type: "text/plain".into(),
    ///         content: b"x".to_vec(),
    ///     },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::ArtifactBlob(Some(blob)) if blob.content.as_slice() == b"x"));
    /// ```
    pub fn artifact_blobs(&self) -> ArtifactBlobRepository<'_> {
        ArtifactBlobRepository::with_clock(&self.conn, Arc::clone(&self.clock))
    }

    /// Returns the Grafeo storage manager boundary.
    pub fn storage_manager(&self) -> StorageManager<'_> {
        StorageManager::new(
            &self.conn,
            &self.graph,
            &self.storage_change_generation,
            &self.storage_change_signal,
            &self.storage_change_sender,
            &self.change_buffer,
            &self.clock,
        )
    }
    /// Creates a deterministic graph-only PZ v1 snapshot at `output_path`.
    #[cfg(test)]
    pub fn create_semantic_snapshot(
        &self,
        project_root: impl AsRef<str>,
        output_path: impl AsRef<Path>,
    ) -> Result<crate::PzSnapshotResult> {
        self.create_semantic_snapshot_controlled(
            project_root,
            output_path,
            &InvocationControl::sixty_seconds(),
        )
    }

    /// Creates a PZ snapshot while observing an App/Plugin invocation control.
    pub fn create_semantic_snapshot_controlled(
        &self,
        project_root: impl AsRef<str>,
        output_path: impl AsRef<Path>,
        control: &InvocationControl,
    ) -> Result<crate::PzSnapshotResult> {
        crate::local::pz::create_semantic_snapshot_controlled(
            &self.storage_manager(),
            project_root.as_ref(),
            output_path,
            control,
        )
    }

    /// Imports a PZ snapshot while observing an App/Plugin invocation control.
    pub fn import_semantic_snapshot_controlled(
        &self,
        project_root: impl AsRef<str>,
        input_path: impl AsRef<Path>,
        control: &InvocationControl,
    ) -> Result<crate::PzImportResult> {
        crate::local::pz::import_semantic_snapshot_controlled(
            &self.storage_manager(),
            project_root.as_ref(),
            input_path,
            control,
        )
    }

    /// Returns the durable, level-triggered semantic graph change-hook module.
    pub fn change_hooks(&self) -> ChangeHooks<'_> {
        ChangeHooks::new(self)
    }

    /// Returns the in-process generation advanced after each published graph commit.
    #[cfg(test)]
    pub fn storage_change_generation(&self) -> u64 {
        self.storage_change_generation.load(Ordering::Acquire)
    }

    /// Waits until a published semantic revision exceeds `after_revision`.
    ///
    /// The signal mutex is held while sampling the published head, so a commit
    /// cannot land between that observation and the condition-variable wait.
    pub fn wait_for_semantic_revision(
        &self,
        after_revision: i64,
        timeout: Option<Duration>,
    ) -> Result<i64> {
        if after_revision < 0 {
            return Err(DbError::invalid_value(
                after_revision.to_string(),
                "non-negative revision",
            ));
        }
        let (generation, changed) = self.storage_change_signal.as_ref();
        let seen = generation
            .lock()
            .map_err(|_| DbError::Grafeo("storage change signal poisoned".to_string()))?;
        let head = self.latest_published_commit_version()?;
        if head > after_revision {
            return Ok(head);
        }
        let seen = match timeout {
            Some(timeout) => {
                changed
                    .wait_timeout(seen, timeout)
                    .map_err(|_| {
                        DbError::Grafeo("storage change signal wait poisoned".to_string())
                    })?
                    .0
            }
            None => changed
                .wait(seen)
                .map_err(|_| DbError::Grafeo("storage change signal wait poisoned".to_string()))?,
        };
        drop(seen);
        self.latest_published_commit_version()
    }

    /// Returns the latest published coherent semantic commit version.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::SemanticRevision,
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::SemanticRevision { commit_version: 0 }));
    /// ```
    pub fn latest_published_commit_version(&self) -> Result<i64> {
        let conn = self.conn.read_conn();
        latest_published_commit_version(&conn)
    }

    /// Reports unresolved semantic commits that require recovery before writes.
    ///
    /// Callers observe semantic readiness through `SemanticPersistence`; detailed
    /// recovery state remains internal.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticPersistence};
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// assert!(SemanticPersistence::readiness(&persistence).unwrap().ready);
    /// ```
    pub fn semantic_recovery_status(&self) -> Result<Option<SemanticRecoveryStatus>> {
        let conn = self.conn.read_conn();
        semantic_recovery_status(&conn)
    }

    /// Performs database maintenance: checkpoints and recompacts the graph file to reclaim freed space.
    /// Safe to call concurrently with reads; blocks active writers briefly.
    ///
    /// # Example
    ///
    /// ```ignore
    /// db.maintenance()?;
    /// ```
    pub fn maintenance(&self) -> Result<()> {
        self.graph.checkpoint()
    }
}

fn create_database_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn graph_path_for_database(database_path: &Path) -> PathBuf {
    let mut graph_path = database_path.to_path_buf();
    let file_name = database_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("lumvise.db");
    graph_path.set_file_name(format!("{file_name}.semantic.grafeo"));
    graph_path
}

fn in_memory_graph_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let sequence = IN_MEMORY_GRAPH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "lumvise-db-core-{}-{nanos}-{sequence}.semantic.grafeo",
        std::process::id()
    ))
}

fn open_graph_database(path: &Path) -> Result<GrafeoDB> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let graph = timed_startup_phase("graph_open_and_replay", || {
        GrafeoDB::with_config(graph_config(path))
            .map_err(|error| crate::DbError::Grafeo(error.to_string()))
    })?;
    timed_startup_phase("semantic_indexes", || configure_semantic_indexes(&graph))?;
    Ok(graph)
}

/// Every committed transaction is durable on return: the WAL fsyncs once per
/// commit record. Grafeo's default batch cadence (100 ms / 1000 records)
/// instead flushed dozens of times inside one bulk snapshot commit.
fn graph_config(path: &Path) -> grafeo::Config {
    grafeo::Config::persistent(path).with_wal_durability(grafeo::DurabilityMode::Sync)
}

fn timed_startup_phase<T>(phase: &'static str, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let started = std::time::Instant::now();
    tracing::info!(target: "db-core::startup", event = "database_startup_phase_started", phase);
    let result = operation();
    tracing::info!(target: "db-core::startup", event = "database_startup_phase_finished", phase,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, success = result.is_ok());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SemanticElement;
    use serde_json::json;
    use std::process::Command;

    const RECOVERY_CHILD_DATABASE: &str = "LUMVISE_RECOVERY_CHECKPOINT_CHILD_DATABASE";

    #[test]
    fn startup_phase_preserves_success_and_failure() {
        assert_eq!(timed_startup_phase("test_success", || Ok(42)).unwrap(), 42);
        let error = timed_startup_phase::<()>("test_failure", || {
            Err(DbError::invalid_value("fixture", "readable database"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("fixture"));
        assert!(error.to_string().contains("readable database"));
    }

    #[test]
    fn graph_wal_syncs_once_per_commit() {
        assert_eq!(
            graph_config(Path::new("graph.grafeo")).wal_durability,
            grafeo::DurabilityMode::Sync
        );
    }

    #[test]
    fn open_checkpoints_recovered_wal_before_returning() {
        if let Ok(database_path) = std::env::var(RECOVERY_CHILD_DATABASE) {
            write_uncheckpointed_child(Path::new(&database_path));
        }

        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("recovery.db");
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local::runtime::tests::open_checkpoints_recovered_wal_before_returning",
            ])
            .env(RECOVERY_CHILD_DATABASE, &database_path)
            .status()
            .unwrap();
        assert!(status.success(), "crash fixture process failed: {status}");

        let recovered = DbCore::open(&database_path).unwrap();
        assert!(
            recovered
                .storage_manager()
                .semantic_storage()
                .element("crash-file")
                .unwrap()
                .is_some()
        );

        let graph_path = graph_path_for_database(&database_path);
        let checkpoint_copy = directory.path().join("recovered-main-only.grafeo");
        std::fs::copy(graph_path, &checkpoint_copy).unwrap();
        let main_only = GrafeoDB::open(checkpoint_copy).unwrap();
        assert_eq!(
            main_only
                .find_nodes_by_property("semantic_element_id", &grafeo::Value::from("crash-file"),)
                .len(),
            1,
            "open must flush recovered WAL state into the main graph before readiness"
        );
    }

    fn write_uncheckpointed_child(database_path: &Path) -> ! {
        let db = DbCore::open(database_path).unwrap();
        db.storage_manager()
            .semantic_storage()
            .upsert_element(&SemanticElement {
                project_root: "/recovery".into(),
                semantic_element_id: "crash-file".into(),
                semantic_source_id: "source".into(),
                path: "src/crash.rs".into(),
                element_kind: "file".into(),
                name: "crash.rs".into(),
                parent_element_id: None,
                content_fingerprint: Some("fp1:0000000000000001:crash".into()),
                start_line: Some(1),
                end_line: Some(2),
                lifecycle: "active".into(),
                match_evidence: None,
                metadata: json!({}),
            })
            .unwrap();
        std::process::exit(0)
    }
}
