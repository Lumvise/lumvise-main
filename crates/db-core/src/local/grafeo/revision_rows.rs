//! Grafeo row mapping for the shared revision policy. No publication or retention rules live here.
use super::committed_columns::CommittedPropertyRows;
use super::graph_row_projection::string_property;
use super::graph_rows::complete_project_rows;
use super::graph_store::GraphTransaction;
use crate::*;
use grafeo::{GrafeoDB, NodeId, Value};
use std::collections::BTreeMap;

pub(super) const DEFAULT_SELECTION: &str = "working";
pub(super) const DEFAULT_SOURCE_CONTEXT: &str = "local";
const ROW_LABEL: &str = "SemanticRevisionRow";
const ROW_KEY: &str = "revision_row_key";
const ROW_PROJECT: &str = "revision_project_root";
const ROW_PAYLOAD: &str = "revision_row_json";
const RECORD_IDENTITY: &str = "revision_record_identity";
const SELECTION_KEY: &str = "revision_selection_key";
const MEMBERSHIP_IDENTITY: &str = "revision_membership_identity";

#[cfg(test)]
thread_local! {
    static REVISION_BODY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn take_revision_body_reads() -> usize {
    REVISION_BODY_READS.with(|reads| reads.replace(0))
}

use crate::SemanticRevisionMember as RevisionMember;
pub(super) use crate::{
    SemanticRevisionMembership as RevisionMembership, SemanticRevisionMetadata as RevisionRow,
};

trait GrafeoRevisionRows {
    fn key(&self, project_root: &str) -> Result<String>;
    fn at_commit(self, commit_version: i64) -> Self;
    fn indexes(&self, root: &str) -> Result<Vec<(&'static str, String)>>;
}

trait GrafeoRevisionHeader {
    fn into_record(self, graph: &GrafeoDB) -> Result<SemanticStoredRevision>;
    fn read_body(&self, graph: &GrafeoDB) -> Result<SemanticRevisionBody>;
}

impl GrafeoRevisionHeader for SemanticRevisionRecordHeader {
    fn into_record(self, graph: &GrafeoDB) -> Result<SemanticStoredRevision> {
        let body = if self.pruned_at_ms.is_some() {
            SemanticRevisionBody::Pruned
        } else {
            self.read_body(graph)?
        };
        Ok(self.restore(body)?)
    }

    fn read_body(&self, graph: &GrafeoDB) -> Result<SemanticRevisionBody> {
        #[cfg(test)]
        REVISION_BODY_READS.with(|reads| reads.set(reads.get() + 1));
        let (label, property) = match self.record.kind {
            SemanticRecordKind::Element => ("SemanticElement", "semantic_element_id"),
            SemanticRecordKind::Artifact => ("SemanticArtifact", "artifact_id"),
        };
        let nodes = super::graph_rows::nodes_by_label_and_property(
            graph,
            label,
            property,
            &self.record.record_id,
        ).into_iter().filter(|node| {
            matches!(node.get_property(property), Some(Value::String(id)) if id.as_ref() == self.record.record_id)
        }).collect::<Vec<_>>();
        let [node] = nodes.as_slice() else {
            return Err(DbError::invalid_value(
                &self.record.record_id,
                "one available immutable semantic body or an explicit pruned header",
            ));
        };
        match self.record.kind {
            SemanticRecordKind::Element => super::graph_rows::semantic_element_from_node(node)
                .map(SemanticRevisionBody::Element)
                .ok_or_else(|| {
                    DbError::invalid_value(
                        &self.record.record_id,
                        "a complete canonical element body",
                    )
                }),
            SemanticRecordKind::Artifact => super::graph_rows::semantic_artifact_from_node(node)
                .map(SemanticRevisionBody::Artifact)
                .ok_or_else(|| {
                    DbError::invalid_value(
                        &self.record.record_id,
                        "a complete canonical artifact body",
                    )
                }),
        }
    }
}

pub(super) struct LoadedRevisionState {
    pub state: SemanticRevisionState,
    pub adopted: bool,
    row_ids: BTreeMap<String, NodeId>,
}

pub(super) struct PreparedRevisionRows {
    project_root: String,
    upserts: Vec<PreparedRevisionRow>,
    deletions: Vec<NodeId>,
}

struct PreparedRevisionRow {
    key: String,
    node_id: Option<NodeId>,
    payload: String,
    commit_row: Option<RevisionRow>,
    record_identity: Option<String>,
    selection_key: Option<String>,
    membership_identity: Option<String>,
    indexes: Vec<(&'static str, String)>,
}

pub(super) fn load_revision_state(
    graph: &GrafeoDB,
    root: &str,
    commit_version: i64,
    now_ms: i64,
    project_id: Option<&str>,
) -> Result<LoadedRevisionState> {
    let nodes = graph.find_nodes_by_property(ROW_PROJECT, &Value::from(root));
    load_revision_nodes(
        graph,
        root,
        commit_version,
        now_ms,
        nodes,
        project_id,
        BTreeMap::new(),
    )
}

/// Reused bodies must belong to this same stable read lease; optimistic planners pass an empty map.
pub(super) fn load_revision_nodes(
    graph: &GrafeoDB,
    root: &str,
    commit_version: i64,
    now_ms: i64,
    nodes: Vec<NodeId>,
    adoption_identity: Option<&str>,
    mut loaded_records: BTreeMap<SemanticRecordRef, SemanticStoredRevision>,
) -> Result<LoadedRevisionState> {
    let mut state = SemanticRevisionState {
        project_root: root.into(),
        commit_version,
        ..Default::default()
    };
    let mut row_ids = BTreeMap::new();
    let mut memberships = BTreeMap::new();
    for id in nodes {
        let Some(node) = graph.get_node(id).filter(|node| node.has_label(ROW_LABEL)) else {
            continue;
        };
        let key = string_property(&node, ROW_KEY)
            .ok_or_else(|| DbError::invalid_value(id.to_string(), "a revision row key"))?;
        if row_ids.insert(key.clone(), id).is_some() {
            return Err(DbError::invalid_value(
                key,
                "one stored row per revision key",
            ));
        }
        append_row(
            graph,
            &mut state,
            &mut memberships,
            decode_revision_payload(id, node.get_property(ROW_PAYLOAD).cloned())?,
            &mut loaded_records,
        )?;
    }
    hydrate_memberships(&mut state, memberships)?;
    let adopted = adoption_identity.is_some() && row_ids.is_empty();
    if adopted {
        state.project_id = adoption_identity
            .expect("adoption requires its durable identity")
            .into();
        let rows = complete_project_rows(graph, root);
        if !rows.elements.is_empty() || !rows.artifacts.is_empty() || !rows.relationships.is_empty()
        {
            state = SemanticRevisionState::adopt(
                root,
                &state.project_id,
                DEFAULT_SELECTION,
                DEFAULT_SOURCE_CONTEXT,
                rows.elements,
                rows.artifacts,
                rows.relationships,
                commit_version,
                now_ms,
            )?;
        }
    }
    if adoption_identity.is_some_and(|identity| identity != state.project_id) {
        return Err(DbError::invalid_value(
            &state.project_id,
            "revision metadata matching the durable project lineage",
        ));
    }
    state.normalize_order();
    Ok(LoadedRevisionState {
        state,
        adopted,
        row_ids,
    })
}

fn append_row(
    graph: &GrafeoDB,
    state: &mut SemanticRevisionState,
    memberships: &mut BTreeMap<String, Vec<RevisionMember>>,
    row: RevisionRow,
    loaded_records: &mut BTreeMap<SemanticRecordRef, SemanticStoredRevision>,
) -> Result<()> {
    match row {
        RevisionRow::Project(project_id) => state.project_id = project_id,
        // Dependency discovery and assembly share the caller's stable graph lease.
        RevisionRow::Record(value) => {
            state
                .records
                .push(match loaded_records.remove(&value.record) {
                    Some(record) => record,
                    None => value.into_record(graph)?,
                })
        }
        RevisionRow::Connection(value) => state.connections.push(value),
        RevisionRow::Selection(selection) => {
            state.selections.push(SemanticRevisionSelectionState {
                selection,
                records: vec![],
                connection_ids: vec![],
            })
        }
        RevisionRow::Membership(value) => memberships
            .entry(value.selection_id)
            .or_default()
            .push(value.member),
        RevisionRow::History(value) => state.selection_history.push(value),
        RevisionRow::Link(value) => state.revision_links.push(value),
        RevisionRow::Association(value) => state.associations.push(value),
        RevisionRow::Operation(value) => state.accepted_operations.push(value),
        RevisionRow::Retention(value) => state.retention_operations.push(value),
    }
    Ok(())
}

fn hydrate_memberships(
    state: &mut SemanticRevisionState,
    memberships: BTreeMap<String, Vec<RevisionMember>>,
) -> Result<()> {
    for (selection_id, members) in memberships {
        let head = state
            .selections
            .iter_mut()
            .find(|head| head.selection.selection_id == selection_id)
            .ok_or_else(|| {
                DbError::invalid_value(&selection_id, "stored selection metadata for membership")
            })?;
        for member in members {
            match member {
                RevisionMember::Record(record) => head.records.push(record),
                RevisionMember::Connection(id) => head.connection_ids.push(id),
            }
        }
        head.records.sort();
        head.connection_ids.sort();
    }
    Ok(())
}

impl GrafeoRevisionRows for RevisionRow {
    fn key(&self, project_root: &str) -> Result<String> {
        Ok(serde_json::to_string(&(
            project_root,
            self.kind(),
            self.identity()?,
        ))?)
    }

    fn at_commit(mut self, commit_version: i64) -> Self {
        match &mut self {
            Self::Link(link) => link.commit_version = commit_version,
            Self::Operation(operation) => {
                operation.receipt.commit_version = commit_version;
                for link in &mut operation.receipt.revision_links {
                    link.commit_version = commit_version;
                }
            }
            Self::Retention(operation) => operation.receipt.commit_version = commit_version,
            Self::History(delta) => delta.commit_version = commit_version,
            _ => {}
        }
        self
    }

    fn indexes(&self, root: &str) -> Result<Vec<(&'static str, String)>> {
        let mut indexes = vec![];
        match self {
            Self::Selection(selection) => indexes.push((
                "revision_follow_context",
                if selection.mode == SemanticSelectionMode::FollowAccepted {
                    serde_json::to_string(&(root, &selection.source_context))?
                } else {
                    String::new()
                },
            )),
            Self::Record(header) => {
                if let Some(scope) = &header.element_scope {
                    indexes.push(("revision_record_path", scope.path.clone()));
                    if let Some(provider) = &scope.provider_instance_id {
                        indexes.push((
                            "revision_record_provider",
                            serde_json::to_string(&(root, provider))?,
                        ));
                    }
                }
            }
            Self::Link(link) => {
                indexes.push((
                    "revision_link_older",
                    serde_json::to_string(&(root, &link.source_context, &link.older))?,
                ));
                indexes.push((
                    "revision_link_identity",
                    serde_json::to_string(&(root, &link.older))?,
                ));
            }
            Self::History(delta) => {
                indexes.push((
                    "revision_history_selection_key",
                    serde_json::to_string(&(root, &delta.selection.selection_id))?,
                ));
                if let Some(anchor) = &delta.selection.source_anchor {
                    if !anchor.dirty {
                        indexes.push((
                            "revision_source_anchor",
                            serde_json::to_string(&(root, &delta.selection.selection_id, anchor))?,
                        ));
                    }
                }
            }
            Self::Association(association) => {
                indexes.push((
                    "revision_association_element",
                    serde_json::to_string(&(root, &association.semantic_element_id))?,
                ));
                indexes.push((
                    "revision_association_artifact",
                    serde_json::to_string(&(root, &association.artifact_id))?,
                ));
            }
            Self::Connection(connection) => {
                indexes.push((
                    "revision_connection_source",
                    serde_json::to_string(&(root, &connection.relationship.source_element_id))?,
                ));
                if let Some(child) = connection.relationship.owned_element_ids().nth(1) {
                    indexes.push((
                        "revision_containment_target",
                        serde_json::to_string(&(root, child))?,
                    ));
                }
            }
            Self::Membership(RevisionMembership {
                member: RevisionMember::Connection(id),
                ..
            }) => {
                indexes.push(("revision_membership_connection", id.clone()));
            }
            _ => {}
        }
        Ok(indexes)
    }
}

impl LoadedRevisionState {
    pub(super) fn prepare_archive_restore(
        &self,
        after: &SemanticRevisionState,
    ) -> Result<PreparedRevisionRows> {
        let desired = after.metadata_rows();
        let obsolete = self.obsolete_archive_rows(&desired)?;
        let mut rows = self.prepare_rows(desired, obsolete)?;
        for row in &mut rows.upserts {
            // A restored acknowledgement keeps its original commit and operation identity.
            row.commit_row = None;
        }
        Ok(rows)
    }

    fn obsolete_archive_rows(&self, desired: &[RevisionRow]) -> Result<Vec<RevisionRow>> {
        let keys = desired
            .iter()
            .map(|row| row.key(&self.state.project_root))
            .collect::<Result<std::collections::BTreeSet<_>>>()?;
        let mut obsolete = vec![];
        for row in self.state.metadata_rows() {
            if !keys.contains(&row.key(&self.state.project_root)?) {
                obsolete.push(row);
            }
        }
        Ok(obsolete)
    }

    pub(super) fn prepare_publication(
        &self,
        plan: &SemanticRevisionPublicationPlan,
    ) -> Result<PreparedRevisionRows> {
        let mut rows = if self.adopted {
            self.state.metadata_rows()
        } else {
            vec![]
        };
        rows.push(RevisionRow::Project(self.state.project_id.clone()));
        rows.extend(
            plan.records
                .iter()
                .cloned()
                .map(|record| RevisionRow::Record(record.into())),
        );
        let connections = self
            .state
            .connections
            .iter()
            .map(|connection| &connection.connection_id)
            .collect::<std::collections::BTreeSet<_>>();
        rows.extend(
            plan.connections
                .iter()
                .filter(|connection| !connections.contains(&connection.connection_id))
                .cloned()
                .map(RevisionRow::Connection),
        );
        rows.extend(
            plan.selections
                .iter()
                .map(|head| RevisionRow::Selection(head.selection.clone())),
        );
        rows.extend(
            plan.selection_deltas
                .iter()
                .cloned()
                .map(RevisionRow::History),
        );
        let mut removed = vec![];
        for delta in &plan.selection_deltas {
            rows.extend(membership_rows(
                &delta.selection.selection_id,
                &delta.added_records,
                &delta.added_connection_ids,
            ));
            removed.extend(membership_rows(
                &delta.selection.selection_id,
                &delta.removed_records,
                &delta.removed_connection_ids,
            ));
        }
        rows.extend(plan.revision_links.iter().cloned().map(RevisionRow::Link));
        rows.extend(
            plan.associations
                .iter()
                .cloned()
                .map(RevisionRow::Association),
        );
        rows.extend(
            plan.accepted_operation
                .iter()
                .cloned()
                .map(RevisionRow::Operation),
        );
        self.prepare_rows(rows, removed)
    }

    pub(super) fn prepare_collection(
        &self,
        after: &SemanticRevisionState,
        receipt: &SemanticRevisionRetentionReceipt,
    ) -> Result<PreparedRevisionRows> {
        let pruned = receipt
            .pruned_records
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        let mut rows = if self.adopted {
            self.state.metadata_rows()
        } else {
            vec![]
        };
        rows.extend(
            after
                .records
                .iter()
                .filter(|stored| pruned.contains(&stored.record))
                .cloned()
                .map(|record| RevisionRow::Record(record.into())),
        );
        rows.extend(
            after
                .retention_operations
                .iter()
                .filter(|operation| operation.receipt.operation_id == receipt.operation_id)
                .cloned()
                .map(RevisionRow::Retention),
        );
        let surviving = after
            .associations
            .iter()
            .map(|row| &row.association_id)
            .collect::<std::collections::BTreeSet<_>>();
        let mut removed = self
            .state
            .associations
            .iter()
            .filter(|row| !surviving.contains(&row.association_id))
            .cloned()
            .map(RevisionRow::Association)
            .collect::<Vec<_>>();
        let connections = after
            .connections
            .iter()
            .map(|row| &row.connection_id)
            .collect::<std::collections::BTreeSet<_>>();
        removed.extend(
            self.state
                .connections
                .iter()
                .filter(|row| !connections.contains(&row.connection_id))
                .cloned()
                .map(RevisionRow::Connection),
        );
        self.prepare_rows(rows, removed)
    }

    fn prepare_rows(
        &self,
        rows: Vec<RevisionRow>,
        removed: Vec<RevisionRow>,
    ) -> Result<PreparedRevisionRows> {
        let mut keyed = BTreeMap::new();
        for row in rows {
            keyed.insert(row.key(&self.state.project_root)?, row);
        }
        let removed_keys = removed
            .iter()
            .map(|row| row.key(&self.state.project_root))
            .collect::<Result<std::collections::BTreeSet<_>>>()?;
        // Adoption seeds rows that do not exist yet; their removal must cancel the pending insert too.
        keyed.retain(|key, _| !removed_keys.contains(key));
        let upserts = keyed
            .into_iter()
            .map(|(key, row)| {
                let commit_row = matches!(
                    row,
                    RevisionRow::Link(_)
                        | RevisionRow::Operation(_)
                        | RevisionRow::Retention(_)
                        | RevisionRow::History(_)
                )
                .then(|| row.clone());
                let record_identity = match &row {
                    RevisionRow::Record(header) => Some(serde_json::to_string(&header.record)?),
                    _ => None,
                };
                let selection_id = match &row {
                    RevisionRow::Selection(selection) => Some(&selection.selection_id),
                    RevisionRow::Membership(member) => Some(&member.selection_id),
                    _ => None,
                };
                let selection_key = selection_id
                    .map(|id| serde_json::to_string(&(&self.state.project_root, id)))
                    .transpose()?;
                let membership_identity = match &row {
                    RevisionRow::Membership(RevisionMembership {
                        member: RevisionMember::Record(record),
                        ..
                    }) => Some(serde_json::to_string(record)?),
                    _ => None,
                };
                let indexes = row.indexes(&self.state.project_root)?;
                Ok(PreparedRevisionRow {
                    node_id: self.row_ids.get(&key).copied(),
                    key,
                    payload: serde_json::to_string(&row)?,
                    commit_row,
                    record_identity,
                    selection_key,
                    membership_identity,
                    indexes,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let deletions = removed_keys
            .iter()
            .filter_map(|key| self.row_ids.get(key).copied())
            .collect();
        Ok(PreparedRevisionRows {
            project_root: self.state.project_root.clone(),
            upserts,
            deletions,
        })
    }
}

fn membership_rows(
    selection_id: &str,
    records: &[SemanticRecordRef],
    connections: &[String],
) -> Vec<RevisionRow> {
    records
        .iter()
        .cloned()
        .map(RevisionMember::Record)
        .chain(connections.iter().cloned().map(RevisionMember::Connection))
        .map(|member| {
            RevisionRow::Membership(RevisionMembership {
                selection_id: selection_id.into(),
                member,
            })
        })
        .collect()
}

impl PreparedRevisionRows {
    pub(super) fn apply(self, graph: &GraphTransaction<'_>, commit_version: i64) -> Result<()> {
        for node_id in self.deletions {
            graph.delete_node(node_id);
        }
        for row in self.upserts {
            let payload = match row.commit_row {
                Some(value) => serde_json::to_string(&value.at_commit(commit_version))?,
                None => row.payload,
            };
            let node_id = if let Some(node_id) = row.node_id {
                graph.set_node_property(node_id, ROW_PAYLOAD, Value::from(payload))?;
                node_id
            } else {
                graph.create_node_with_props(
                    &[ROW_LABEL],
                    [
                        (ROW_PROJECT, Value::from(self.project_root.clone())),
                        (ROW_KEY, Value::from(row.key)),
                        (ROW_PAYLOAD, Value::from(payload)),
                    ],
                )?
            };
            if let Some(identity) = row.record_identity {
                graph.set_node_property(node_id, RECORD_IDENTITY, Value::from(identity))?;
            }
            if let Some(key) = row.selection_key {
                graph.set_node_property(node_id, SELECTION_KEY, Value::from(key))?;
            }
            if let Some(identity) = row.membership_identity {
                graph.set_node_property(node_id, MEMBERSHIP_IDENTITY, Value::from(identity))?;
            }
            for (property, value) in row.indexes {
                graph.set_node_property(node_id, property, Value::from(value))?;
            }
        }
        Ok(())
    }
}

pub(super) fn revision_project_managed(graph: &GrafeoDB, root: &str) -> Result<bool> {
    Ok(!revision_row_nodes(graph, root, "project", "")?.is_empty())
}

pub(super) fn revision_row_nodes(
    graph: &GrafeoDB,
    root: &str,
    kind: &str,
    identity: &str,
) -> Result<Vec<NodeId>> {
    let key = serde_json::to_string(&(root, kind, identity))?;
    Ok(graph
        .find_nodes_by_property(ROW_KEY, &Value::from(key))
        .into_iter()
        .filter(|id| {
            graph
                .get_node(*id)
                .is_some_and(|node| node.has_label(ROW_LABEL))
        })
        .collect())
}

pub(super) fn revision_row_from_node(graph: &GrafeoDB, id: NodeId) -> Result<RevisionRow> {
    let store = graph.graph_store();
    let payload = store
        .is_node_visible_at_epoch(id, store.current_epoch())
        .then(|| store.get_node_property(id, &ROW_PAYLOAD.into()))
        .flatten();
    decode_revision_payload(id, payload)
}

pub(super) fn revision_rows_from_nodes(
    graph: &GrafeoDB,
    ids: &[NodeId],
) -> Result<Vec<RevisionRow>> {
    let store = graph.graph_store();
    let payloads = store
        .get_node_property_batch(ids, &ROW_PAYLOAD.into())
        .committed_nodes(&*store, ids);
    ids.iter()
        .zip(payloads)
        .map(|(id, payload)| decode_revision_payload(*id, payload))
        .collect()
}

fn decode_revision_payload(id: NodeId, payload: Option<Value>) -> Result<RevisionRow> {
    let Some(Value::String(payload)) = payload else {
        return Err(DbError::invalid_value(
            id.to_string(),
            "a typed revision row payload",
        ));
    };
    Ok(serde_json::from_str(&payload)?)
}

pub(super) fn stored_revision_by_id(
    graph: &GrafeoDB,
    record: &SemanticRecordRef,
) -> Result<Option<(String, SemanticStoredRevision)>> {
    let identity = serde_json::to_string(record)?;
    let nodes = graph.find_nodes_by_property(RECORD_IDENTITY, &Value::from(identity));
    let nodes = nodes
        .into_iter()
        .filter_map(|id| graph.get_node(id))
        .filter(|node| node.has_label(ROW_LABEL))
        .collect::<Vec<_>>();
    if nodes.len() > 1 {
        return Err(DbError::invalid_value(
            &record.record_id,
            "one canonical revision identity across local project roots",
        ));
    }
    let Some(node) = nodes.into_iter().next() else {
        return Ok(None);
    };
    let root = string_property(&node, ROW_PROJECT).ok_or_else(|| {
        DbError::invalid_value(&record.record_id, "a stored revision project root")
    })?;
    let RevisionRow::Record(header) =
        decode_revision_payload(node.id, node.get_property(ROW_PAYLOAD).cloned())?
    else {
        return Err(DbError::invalid_value(
            &record.record_id,
            "a record header at a canonical revision identity",
        ));
    };
    Ok(Some((root, header.into_record(graph)?)))
}

pub(super) fn selected_project_snapshot(
    graph: &GrafeoDB,
    root: &str,
    commit_version: i64,
    published_at: &str,
) -> Result<Option<SemanticProjectSnapshot>> {
    if !revision_project_managed(graph, root)? {
        return Ok(None);
    }
    let state = super::revision_basis::load_selected_revision_state(
        graph,
        root,
        DEFAULT_SELECTION,
        commit_version,
    )?;
    let snapshot = state.snapshot(DEFAULT_SELECTION, published_at)?;
    let (elements, artifacts, relationships) = snapshot.map_or_else(Default::default, |snapshot| {
        (
            snapshot.elements,
            snapshot.artifacts,
            snapshot
                .connections
                .into_iter()
                .map(|edge| edge.relationship)
                .collect(),
        )
    });
    Ok(Some(SemanticProjectSnapshot {
        project_root: root.into(),
        commit_version,
        published_at: published_at.into(),
        elements,
        artifacts,
        relationships,
    }))
}

/// Unconditional producers must be ported before they may touch immutable revision-managed records.
pub(super) fn require_unmanaged_project(graph: &GrafeoDB, project_root: &str) -> Result<()> {
    if !revision_project_managed(graph, project_root)? {
        return Ok(());
    }
    Err(DbError::invalid_value(
        project_root,
        "a conditional PublishRevisions or SelectRevisions operation for a revision-managed project",
    ))
}

pub(super) fn require_unmanaged_element(graph: &GrafeoDB, element_id: &str) -> Result<()> {
    if let Some((root, _)) = stored_revision_by_id(graph, &SemanticRecordRef::element(element_id))?
    {
        return require_unmanaged_project(graph, &root);
    }
    if let Some(element) = super::graph_rows::semantic_element_by_id(graph, element_id) {
        return require_unmanaged_project(graph, &element.project_root);
    }
    Ok(())
}

pub(super) fn require_unmanaged_artifact(graph: &GrafeoDB, artifact_id: &str) -> Result<()> {
    if let Some((root, _)) =
        stored_revision_by_id(graph, &SemanticRecordRef::artifact(artifact_id))?
    {
        return require_unmanaged_project(graph, &root);
    }
    if let Some(artifact) = super::graph_rows::semantic_artifact_by_id(graph, artifact_id) {
        return require_unmanaged_element(graph, &artifact.semantic_element_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
