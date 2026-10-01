use crate::interface::ProjectSnapshotScope;
use crate::local::clock::Clock;
use crate::local::fingerprint::parse_content_fingerprint;
use crate::local::grafeo::blob_refs::SEARCHABLE_ARTIFACT_TEXT_LIMIT_BYTES;
use crate::local::grafeo::change_hooks::ChangeBuffer;
use crate::local::grafeo::graph_row_projection::semantic_element_from_node;
use crate::local::grafeo::graph_rows::{
    ChangeCollector, DuplicateElementRepair, ElementInsertPlan, ElementUpsertPlan,
    ProjectSnapshotDeletePlan, RelationshipWritePlan, apply_element_insert, apply_element_upsert,
    apply_project_snapshot_delete, apply_relationship_write, artifact_dependents,
    artifact_vector_from_node, artifacts_by_ids_selective, candidate_elements_by_identity_keys,
    element_name_vector_from_node, elements_by_ids_selective, nodes_by_label_and_property,
    prepare_element_insert, prepare_element_upsert, prepare_project_snapshot_delete,
    prepare_relationship_sync, prepare_relationship_upserts, project_artifacts_selective,
    semantic_artifact_by_id, semantic_artifacts_for_elements, semantic_element_by_id,
    semantic_element_node_id_for, semantic_elements_for_paths_including_inactive,
    semantic_elements_for_project_including_inactive, semantic_relationships_for_project_edges,
    semantic_relationships_from_many_native_edges, semantic_relationships_from_native_edges,
    semantic_relationships_touching_elements,
};
#[cfg(test)]
use crate::local::grafeo::graph_rows::{
    apply_artifact_upsert, prepare_artifact_upsert, semantic_elements_for_project,
};
use crate::local::grafeo::graph_store::GraphStore;
use crate::local::grafeo::graph_store::GraphTransaction;
use crate::local::grafeo::matching::{IdentityRemapIndex, reconcile_semantic_elements};
use crate::local::grafeo::media::artifacts_for_element_with_inheritance;
#[cfg(test)]
use crate::local::grafeo::media::{active_media_element, annotation_for_media_element};
use crate::local::grafeo::semantic_graph_projection::{
    project as project_renderer_graph, slice_file_projection,
};
use crate::local::grafeo::semantic_snapshot::project_snapshot;
use crate::local::sql::artifact_blobs::ArtifactBlobRepository;
use crate::local::sql::commits::latest_published_commit_version;
use crate::local::sql::commits::published_commit_timestamp;
use crate::local::sql::connections::SqlConnections;
use crate::local::sql::semantic_graph_projections::SemanticGraphProjectionRepository;
use crate::local::sql::validation::require_non_empty;
use crate::{
    ArtifactBlob, ChangeDisposition, DbError, Result, SemanticArtifact, SemanticBatchSyncReport,
    SemanticElement, SemanticPartition, SemanticProjectSnapshot, SemanticRelationship,
    StoredArtifactTextVector, StoredSemanticElementNameVector,
};
use grafeo::{NodeId, Value as GrafeoValue};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Condvar, Mutex, atomic::AtomicU64};
use std::time::Instant;
use tokio::sync::broadcast;

struct PartitionSyncPlan {
    reconciled: crate::local::grafeo::matching::ReconciledElements,
    inactive_elements: Vec<SemanticElement>,
    element_changes: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
}

struct PreparedDelta {
    element_plans: Vec<ElementUpsertPlan>,
    relationship_plan: RelationshipWritePlan,
    duplicate_repair: DuplicateElementRepair,
}

struct PreparedSnapshot {
    delete_plan: ProjectSnapshotDeletePlan,
    element_plans: Vec<ElementInsertPlan>,
    relationship_plan: RelationshipWritePlan,
}

enum PreparedStructureMutation {
    NoOp,
    Snapshot {
        active_elements: Vec<SemanticElement>,
        inactive_elements: Vec<SemanticElement>,
        plan: PreparedSnapshot,
    },
    Delta {
        element_changes: Vec<SemanticElement>,
        plan: PreparedDelta,
    },
}

struct PreparedStructureSync {
    report: SemanticBatchSyncReport,
    remaps: Vec<crate::local::grafeo::matching::IdentityRemap>,
    mutation: PreparedStructureMutation,
}

impl PreparedStructureSync {
    fn is_noop(&self) -> bool {
        matches!(self.mutation, PreparedStructureMutation::NoOp)
    }
}

pub struct SemanticStorage<'db> {
    pub(crate) control: lumvise_resource_routing::InvocationControl,
    pub(crate) conn: &'db SqlConnections,
    pub(crate) graph: &'db GraphStore,
    pub(crate) storage_change_generation: &'db Arc<AtomicU64>,
    pub(crate) storage_change_signal: &'db Arc<(Mutex<u64>, Condvar)>,
    pub(crate) storage_change_sender: &'db Arc<broadcast::Sender<u64>>,
    pub(crate) change_buffer: &'db Arc<Mutex<ChangeBuffer>>,
    pub(crate) clock: &'db Arc<dyn Clock>,
}

impl<'db> SemanticStorage<'db> {
    pub(crate) fn with_control(
        mut self,
        control: lumvise_resource_routing::InvocationControl,
    ) -> Self {
        self.control = control;
        self
    }

    pub(crate) fn check_active(&self) -> Result<()> {
        crate::local::persistence::verify_control(&self.control)
    }

    pub(crate) fn new(
        conn: &'db SqlConnections,
        graph: &'db GraphStore,
        storage_change_generation: &'db Arc<AtomicU64>,
        storage_change_signal: &'db Arc<(Mutex<u64>, Condvar)>,
        storage_change_sender: &'db Arc<broadcast::Sender<u64>>,
        change_buffer: &'db Arc<Mutex<ChangeBuffer>>,
        clock: &'db Arc<dyn Clock>,
    ) -> Self {
        Self {
            control: lumvise_resource_routing::InvocationControl::sixty_seconds(),
            conn,
            graph,
            storage_change_generation,
            storage_change_signal,
            storage_change_sender,
            clock,
            change_buffer,
        }
    }
}
mod reads;
mod sync;
mod validation;

pub(crate) use validation::*;

mod sync_changes;
use sync_changes::{
    changed_semantic_elements, duplicate_element_ids, inactive_elements, record_sync_changes,
    should_replace_snapshot, sort_relationships,
};

#[cfg(test)]
mod tests {
    use crate::SemanticElement;
    use crate::local::grafeo::graph_row_projection::{
        bool_property, i64_property, string_property,
    };
    use crate::local::grafeo::graph_rows::semantic_elements_for_project_including_inactive;
    use crate::local::runtime::DbCore;
    use crate::local::sql::semantic_graph_projections::SemanticGraphProjectionRepository;
    use grafeo::Value as GrafeoValue;
    use serde_json::json;
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn element_revisions_and_tombstones_preserve_active_reads() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        let deleted = element("deleted", "before");
        let retained = element("retained", "retained");

        storage.upsert_element(&deleted).unwrap();
        storage.upsert_element(&retained).unwrap();
        let created_revision = tracking_state(&db, "deleted").0;
        assert_eq!(
            tracking_state(&db, "deleted"),
            (created_revision, true, -1, "active".to_string())
        );

        let updated = element("deleted", "after");
        storage.upsert_element(&updated).unwrap();
        let updated_revision = tracking_state(&db, "deleted").0;
        assert!(updated_revision > created_revision);
        assert_eq!(
            tracking_state(&db, "deleted"),
            (updated_revision, true, -1, "active".to_string())
        );

        assert!(storage.remove_element("deleted").unwrap());
        let deleted_revision = tracking_state(&db, "deleted").0;
        assert!(deleted_revision > updated_revision);
        assert_eq!(
            tracking_state(&db, "deleted"),
            (
                deleted_revision,
                false,
                deleted_revision,
                "inactive".to_string()
            )
        );

        assert!(storage.element("deleted").unwrap().is_none());
        assert_eq!(
            storage
                .elements_for_project("/repo")
                .unwrap()
                .into_iter()
                .map(|element| element.semantic_element_id)
                .collect::<Vec<_>>(),
            vec!["retained"]
        );
        let mut all_ids = db.graph.read(|graph| {
            semantic_elements_for_project_including_inactive(graph, "/repo")
                .into_iter()
                .map(|element| element.semantic_element_id)
                .collect::<Vec<_>>()
        });
        all_ids.sort();
        assert_eq!(all_ids, vec!["deleted", "retained"]);
    }

    #[test]
    fn project_roots_include_only_active_semantic_elements() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage.upsert_element(&element("repo", "repo")).unwrap();
        let mut other = element("other", "other");
        other.project_root = "/other".to_string();
        storage.upsert_element(&other).unwrap();

        assert_eq!(
            storage.semantic_project_roots().unwrap(),
            vec!["/other", "/repo"]
        );
        storage.remove_element("other").unwrap();
        assert_eq!(storage.semantic_project_roots().unwrap(), vec!["/repo"]);
    }
    #[test]
    fn renderer_graph_projection_is_complete_and_revision_tagged() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage.upsert_element(&element("file", "file")).unwrap();
        let projection = storage
            .project_renderer_graph(&crate::SemanticGraphProjectionRequest {
                project_root: "/repo".into(),
                target_path: None,
                granularity: crate::SemanticGraphGranularity::File,
                recursive: true,
                include_external: false,
                include_first_neighbors: false,
            })
            .unwrap();
        assert_eq!(projection.project_root, "/repo");
        assert_eq!(projection.nodes.len(), 1);
        assert!(projection.edges.is_empty());
        assert!(projection.commit_version > 0);
        assert!(!projection.published_at.is_empty());
    }

    #[test]
    fn on_demand_projection_uses_publication_revision_from_stable_graph_snapshot() {
        let db = Arc::new(DbCore::in_memory().unwrap());
        let storage = db.storage_manager().semantic_storage();
        storage.upsert_element(&element("file", "file")).unwrap();
        let first = storage
            .project_renderer_graph(&crate::SemanticGraphProjectionRequest {
                project_root: "/repo".into(),
                target_path: None,
                granularity: crate::SemanticGraphGranularity::File,
                recursive: true,
                include_external: false,
                include_first_neighbors: false,
            })
            .unwrap();
        db.graph.reset_gate_work();

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer_db = Arc::clone(&db);
        let writer = thread::spawn(move || {
            let storage = writer_db.storage_manager().semantic_storage();
            storage
                .commit_graph_write(|graph, _commit_version, _collector| {
                    graph.create_node_with_props(&["PublicationRace"], [])?;
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap();
        });
        entered_rx.recv().unwrap();

        let reader_db = Arc::clone(&db);
        let reader = thread::spawn(move || {
            let storage = reader_db.storage_manager().semantic_storage();
            storage
                .project_renderer_graph(&super::reads::canonical_file_projection_request("/repo"))
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while db.graph.gate_work().stable_read_attempts == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            db.graph.gate_work().stable_read_attempts > 0,
            "materializer did not reach the stable-read gate"
        );
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        reader.join().unwrap().unwrap();

        let conn = db.conn.read_conn();
        let stored = SemanticGraphProjectionRepository::new(&conn)
            .get("/repo", first.commit_version + 1)
            .unwrap();
        assert_eq!(
            stored.as_ref().map(|projection| projection.commit_version),
            Some(first.commit_version + 1)
        );
    }

    fn tracking_state(db: &DbCore, element_id: &str) -> (i64, bool, i64, String) {
        db.graph.read(|graph| {
            let node = graph
                .find_nodes_by_property("semantic_element_id", &GrafeoValue::from(element_id))
                .into_iter()
                .filter_map(|node_id| graph.get_node(node_id))
                .find(|node| node.has_label("SemanticElement"))
                .unwrap();
            (
                i64_property(&node, "last_changed_revision").unwrap(),
                bool_property(&node, "active"),
                i64_property(&node, "deleted_at").unwrap(),
                string_property(&node, "lifecycle").unwrap(),
            )
        })
    }

    fn element(id: &str, name: &str) -> SemanticElement {
        SemanticElement {
            project_root: "/repo".to_string(),
            semantic_element_id: id.to_string(),
            semantic_source_id: "source".to_string(),
            path: format!("src/{id}.rs"),
            element_kind: "file".to_string(),
            name: name.to_string(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".to_string(),
            match_evidence: None,
            metadata: json!({}),
        }
    }
}

#[cfg(test)]
mod duplicate_identities;
