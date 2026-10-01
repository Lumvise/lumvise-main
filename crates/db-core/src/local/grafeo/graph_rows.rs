use crate::ChangeDisposition;
use crate::local::grafeo::change_hooks::DirtyElement;
#[cfg(test)]
use crate::local::grafeo::graph_row_projection::bool_property;
pub(crate) use crate::local::grafeo::graph_row_projection::{
    artifact_vector_from_node, artifact_vector_props, element_name_vector_from_node,
    element_name_vector_props, semantic_artifact_from_node, semantic_element_from_node,
    storage_alias_properties, string_property,
};
use crate::local::grafeo::graph_row_projection::{
    edge_string_property, semantic_relationship_from_edge,
};
use crate::local::grafeo::graph_store::GraphTransaction;
use crate::{
    ArtifactTextVector, DbError, Result, SemanticArtifact, SemanticElement, SemanticMatchEvidence,
    SemanticRelationship,
};
use grafeo::{EdgeId, GrafeoDB, NodeId, Value as GrafeoValue};
use grafeo_core::graph::{Direction, GraphStore as GrafeoGraphStore};
use lumvise_contracts::{
    ArtifactDependency, ArtifactDependencyTarget, validate_artifact_dependencies,
};
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[cfg(test)]
thread_local! {
    static SELECTIVE_NODE_BATCH_CALLS: Cell<usize> = const { Cell::new(0) };
    static SELECTIVE_EDGE_BATCH_CALLS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_selective_batch_counters() {
    SELECTIVE_NODE_BATCH_CALLS.set(0);
    SELECTIVE_EDGE_BATCH_CALLS.set(0);
}

#[cfg(test)]
pub(crate) fn selective_batch_counters() -> (usize, usize) {
    (
        SELECTIVE_NODE_BATCH_CALLS.get(),
        SELECTIVE_EDGE_BATCH_CALLS.get(),
    )
}

#[cfg(test)]
fn record_selective_node_batch_call() {
    SELECTIVE_NODE_BATCH_CALLS.set(SELECTIVE_NODE_BATCH_CALLS.get() + 1);
}

#[cfg(test)]
fn record_selective_edge_batch_call() {
    SELECTIVE_EDGE_BATCH_CALLS.set(SELECTIVE_EDGE_BATCH_CALLS.get() + 1);
}

mod relationship_updates;
pub(crate) use relationship_updates::{
    RelationshipWritePlan, apply_relationship_write, prepare_relationship_sync,
    prepare_relationship_upserts,
};
mod artifact_writes;
mod duplicate_elements;
mod element_hierarchy;
mod element_writes;
pub(crate) use duplicate_elements::DuplicateElementRepair;
mod records;
mod relationships;
mod subtree_selection;
pub(super) use subtree_selection::subtree_elements;

pub(super) use artifact_writes::*;
pub(super) use element_hierarchy::semantic_elements_for_paths_including_inactive;
pub(super) use element_writes::*;
pub(super) use records::*;
pub(super) use relationships::*;
const PROJECT_ROOT_PROPERTY: &str = "project_root";
const SEMANTIC_ELEMENT_ID_PROPERTY: &str = "semantic_element_id";
const ARTIFACT_ID_PROPERTY: &str = "artifact_id";
const PATH_PROPERTY: &str = "path";
const ELEMENT_KIND_PROPERTY: &str = "element_kind";
const LAST_CHANGED_REVISION_PROPERTY: &str = "last_changed_revision";
const ACTIVE_PROPERTY: &str = "active";
pub(crate) const SEMANTIC_ELEMENT_ID_PROPERTY_FOR_HOOKS: &str = SEMANTIC_ELEMENT_ID_PROPERTY;
pub(crate) const LAST_CHANGED_REVISION_PROPERTY_FOR_HOOKS: &str = LAST_CHANGED_REVISION_PROPERTY;
pub(crate) const ACTIVE_PROPERTY_FOR_HOOKS: &str = ACTIVE_PROPERTY;
#[cfg(test)]
pub(crate) const DELETED_AT_PROPERTY_FOR_HOOKS: &str = DELETED_AT_PROPERTY;
const DELETED_AT_PROPERTY: &str = "deleted_at";
pub(super) const PARENT_ELEMENT_ID_PROPERTY: &str = "parent_element_id";
const ARTIFACT_DEPENDENCY_EDGE_TYPE: &str = "artifact_dependency";
const ARTIFACT_DEPENDENCY_TARGET_KIND_PROPERTY: &str = "target_kind";
const ARTIFACT_DEPENDENCY_TARGET_ID_PROPERTY: &str = "target_id";
const SEMANTIC_ARTIFACT_EDGE_TYPE: &str = "semantic_artifact";
const CONTAINS_RELATIONSHIP_KIND: &str = "contains";
const CONTENT_FINGERPRINT_PROPERTY: &str = "content_fingerprint";
const IDENTITY_KIND_NAME_PROPERTY: &str = "identity_kind_name";
const IDENTITY_KIND_FILE_NAME_PROPERTY: &str = "identity_kind_file_name";
const IDENTITY_BACKFILL_MARKER_LABEL: &str = "SemanticIndexState";
const IDENTITY_BACKFILL_VERSION_PROPERTY: &str = "identity_backfill_version";
const IDENTITY_BACKFILL_VERSION: i64 = 1;

/// Registers the equality-lookup indexes `CandidateSourceElements` depends on,
/// backfilling derived identity properties onto `SemanticElement` nodes
/// written before they existed so recall is identical immediately after
/// upgrade - never a temporary gap for already-indexed elements.
pub(crate) fn configure_semantic_indexes(database: &GrafeoDB) -> Result<()> {
    database.create_property_index(IDENTITY_BACKFILL_VERSION_PROPERTY);
    backfill_identity_properties(database)?;
    database.create_property_index(PROJECT_ROOT_PROPERTY);
    database.create_property_index(SEMANTIC_ELEMENT_ID_PROPERTY);
    database.create_property_index(ARTIFACT_ID_PROPERTY);
    database.create_property_index(PATH_PROPERTY);
    database.create_property_index(ACTIVE_PROPERTY);
    database.create_property_index(ELEMENT_KIND_PROPERTY);
    database.create_property_index(PARENT_ELEMENT_ID_PROPERTY);
    database.create_property_index(CONTENT_FINGERPRINT_PROPERTY);
    database.create_property_index(IDENTITY_KIND_NAME_PROPERTY);
    database.create_property_index(IDENTITY_KIND_FILE_NAME_PROPERTY);
    Ok(())
}

fn identity_kind_name_value(element: &SemanticElement) -> String {
    format!(
        "{}\u{1}{}",
        element.element_kind,
        element.name.to_ascii_lowercase()
    )
}

fn identity_kind_file_name_value(element: &SemanticElement) -> String {
    format!(
        "{}\u{1}{}",
        element.element_kind,
        element.path.rsplit('/').next().unwrap_or_default()
    )
}

/// A durable marker node (never touched by ordinary semantic writes) records
/// that the identity backfill below already ran. The version property is
/// indexed before this check runs, so a healthy graph file never repeats the
/// full-node scan on later opens - this is one indexed point lookup.
fn identity_backfill_marker_present(database: &GrafeoDB) -> bool {
    database
        .find_nodes_by_property(
            IDENTITY_BACKFILL_VERSION_PROPERTY,
            &GrafeoValue::Int64(IDENTITY_BACKFILL_VERSION),
        )
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .any(|node| node.has_label(IDENTITY_BACKFILL_MARKER_LABEL))
}

/// Writes `identity_kind_name`/`identity_kind_file_name` onto every existing
/// `SemanticElement` node through a raw Grafeo session - deliberately
/// bypassing `GraphTransaction`/`ChangeCollector`, so this never bumps
/// `last_changed_revision`, never touches `active`/`deleted_at`, and never
/// publishes a StorageTrigger/ChangeHook batch. The computation is a pure
/// function of already-stored `element_kind`/`name`/`path`, so re-running it
/// (e.g. after a crash before the marker commits) is idempotent.
fn backfill_identity_properties(database: &GrafeoDB) -> Result<()> {
    if identity_backfill_marker_present(database) {
        return Ok(());
    }
    let pending = database
        .iter_nodes()
        .filter(|node| node.has_label("SemanticElement"))
        .filter_map(|node| {
            let element = semantic_element_from_node(&node)?;
            Some((
                node.id,
                identity_kind_name_value(&element),
                identity_kind_file_name_value(&element),
            ))
        })
        .collect::<Vec<_>>();
    let mut session = database.session();
    session
        .begin_transaction()
        .map_err(|error| DbError::Grafeo(error.to_string()))?;
    for (node_id, kind_name, kind_file_name) in pending {
        session
            .set_node_property(
                node_id,
                IDENTITY_KIND_NAME_PROPERTY,
                GrafeoValue::from(kind_name),
            )
            .map_err(|error| DbError::Grafeo(error.to_string()))?;
        session
            .set_node_property(
                node_id,
                IDENTITY_KIND_FILE_NAME_PROPERTY,
                GrafeoValue::from(kind_file_name),
            )
            .map_err(|error| DbError::Grafeo(error.to_string()))?;
    }
    session
        .create_node_with_props(
            &[IDENTITY_BACKFILL_MARKER_LABEL],
            [(
                IDENTITY_BACKFILL_VERSION_PROPERTY,
                GrafeoValue::from(IDENTITY_BACKFILL_VERSION),
            )],
        )
        .map_err(|error| DbError::Grafeo(error.to_string()))?;
    session
        .commit()
        .map_err(|error| DbError::Grafeo(error.to_string()))?;
    Ok(())
}

pub(crate) struct ElementUpsertPlan {
    existing_node_ids: Vec<NodeId>,
    properties: Vec<(&'static str, GrafeoValue)>,
}

pub(crate) struct ElementInsertPlan {
    properties: Vec<(&'static str, GrafeoValue)>,
}

impl ElementUpsertPlan {
    pub(crate) fn relationship_node_id(&self, semantic_element_id: &str) -> NodeId {
        self.existing_node_ids
            .first()
            .copied()
            .unwrap_or_else(|| semantic_element_node_id_for(semantic_element_id))
    }
}

pub(crate) struct VectorReplacePlan {
    node_ids: Vec<NodeId>,
    properties: Vec<(&'static str, GrafeoValue)>,
}

pub(crate) struct ArtifactDependencyRebind {
    source_node_id: NodeId,
    target_kind: String,
    target_id: String,
}

pub(crate) struct ArtifactUpsertPlan {
    artifact_node_ids: Vec<NodeId>,
    artifact_edge_ids: Vec<EdgeId>,
    dependency_edge_ids: Vec<EdgeId>,
    dependency_targets: Vec<(ArtifactDependency, NodeId)>,
    inbound_dependency_rebinds: Vec<ArtifactDependencyRebind>,
    vector_node_ids: Vec<NodeId>,
    owner_node_id: NodeId,
    pub(crate) owner_element: SemanticElement,
    artifact_properties: Vec<(&'static str, GrafeoValue)>,
    vector_properties: Option<Vec<(&'static str, GrafeoValue)>>,
}

pub(crate) struct ProjectSnapshotDeletePlan {
    relationship_edge_ids: Vec<EdgeId>,
    element_node_ids: Vec<NodeId>,
    element_vector_node_ids: Vec<NodeId>,
}

pub(crate) struct ArtifactDeletionPlan {
    artifact_node_ids: Vec<NodeId>,
    artifact_vector_node_ids: Vec<NodeId>,
    artifact_edge_ids: Vec<EdgeId>,
    owner_node_id: Option<NodeId>,
    pub(crate) owner_element: Option<SemanticElement>,
    pub(crate) artifact: Option<SemanticArtifact>,
}

pub(crate) struct ElementDeactivationPlan {
    element_node_ids: Vec<NodeId>,
    element_vector_node_ids: Vec<NodeId>,
    pub(crate) entity_kind: String,
    relationship_edge_ids: Vec<EdgeId>,
    artifact_node_ids: Vec<NodeId>,
    artifact_vector_node_ids: Vec<NodeId>,
    pub(crate) exists: bool,
    pub(crate) artifacts: Vec<SemanticArtifact>,
    pub(crate) project_root: String,
}

#[cfg(test)]
mod tests;

pub(super) fn project_member_ids(
    graph: &GrafeoDB,
    project_root: &str,
    include_inactive: bool,
) -> HashSet<String> {
    let ids = graph.find_nodes_by_property("project_root", &GrafeoValue::from(project_root));
    let keys = [
        "semantic_element_id",
        "lifecycle",
        "semantic_source_id",
        "path",
        "element_kind",
        "name",
    ]
    .map(Into::into);
    let rows = graph
        .graph_store()
        .get_nodes_properties_selective_batch(&ids, &keys);
    rows.iter()
        .filter(|row| {
            include_inactive
                || row
                    .get("lifecycle")
                    .and_then(value_string)
                    .is_none_or(|value| value == "active")
        })
        .filter(|row| {
            ["semantic_source_id", "path", "element_kind", "name"]
                .iter()
                .all(|key| row.get(*key).and_then(value_string).is_some())
        })
        .filter_map(|row| row.get("semantic_element_id").and_then(value_string))
        .collect()
}
