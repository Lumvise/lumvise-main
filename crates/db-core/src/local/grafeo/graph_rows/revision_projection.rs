//! Native projection of accepted identities; only policy-approved working artifact bodies change in place.
use super::*;
use crate::local::grafeo::revision_rows::DEFAULT_SELECTION;
use crate::{
    SemanticRecordKind, SemanticRecordRef, SemanticRevisionBody, SemanticRevisionConnection,
    SemanticRevisionPublicationPlan, SemanticRevisionSelectionState, SemanticRevisionState,
    SemanticStoredRevision,
};

mod notifications;
mod working;

pub(crate) struct RevisionProjection {
    nodes: BTreeMap<SemanticRecordRef, Option<NodeId>>,
    inserts: Vec<RevisionNodeInsert>,
    flags: Vec<RevisionProjectionFlag>,
    connections: RelationshipWritePlan,
    dirty: Vec<(SemanticElement, ChangeDisposition)>,
    inherited: Vec<crate::SemanticRevisionAssociation>,
    working: Vec<working::WorkingArtifactProjection>,
    project_root: String,
}

struct RevisionProjectionChanges<'a> {
    records: &'a [SemanticStoredRevision],
    connections: &'a [SemanticRevisionConnection],
    selections: &'a [SemanticRevisionSelectionState],
    associations: &'a [crate::SemanticRevisionAssociation],
}

struct RevisionNodeInsert {
    record: SemanticRecordRef,
    properties: Vec<(&'static str, GrafeoValue)>,
    artifact: Option<SemanticArtifact>,
}

struct RevisionProjectionFlag {
    record: SemanticRecordRef,
    selected: bool,
    lifecycle: Option<String>,
}

impl RevisionProjectionFlag {
    fn new(
        record: SemanticRecordRef,
        selected: bool,
        records: &BTreeMap<&SemanticRecordRef, &SemanticStoredRevision>,
    ) -> Self {
        let lifecycle = records.get(&record).and_then(|stored| match &stored.body {
            SemanticRevisionBody::Element(element) => Some(element.lifecycle.clone()),
            _ => None,
        });
        Self {
            record,
            selected,
            lifecycle,
        }
    }
}

impl RevisionProjection {
    pub(crate) fn prepare(
        database: &GrafeoDB,
        state: &SemanticRevisionState,
        plan: &SemanticRevisionPublicationPlan,
        adopted: bool,
    ) -> Result<Self> {
        Self::prepare_changes(
            database,
            state,
            RevisionProjectionChanges {
                records: &plan.records,
                connections: &plan.connections,
                selections: &plan.selections,
                associations: &plan.associations,
            },
            adopted,
        )
    }

    pub(crate) fn prepare_restore(
        database: &GrafeoDB,
        before: &SemanticRevisionState,
        after: &SemanticRevisionState,
        adopted: bool,
    ) -> Result<Self> {
        Self::prepare_changes(
            database,
            before,
            RevisionProjectionChanges {
                records: &after.records,
                connections: &after.connections,
                selections: &after.selections,
                associations: &after.associations,
            },
            adopted,
        )
    }

    fn prepare_changes(
        database: &GrafeoDB,
        state: &SemanticRevisionState,
        changes: RevisionProjectionChanges<'_>,
        adopted: bool,
    ) -> Result<Self> {
        let before = state.selection(DEFAULT_SELECTION);
        let after = changes
            .selections
            .iter()
            .find(|head| head.selection.selection_id == DEFAULT_SELECTION)
            .or(before);
        let old = selected_records(before);
        let selected = selected_records(after);
        let mut changed = old
            .symmetric_difference(&selected)
            .cloned()
            .collect::<Vec<_>>();
        let previous_bodies = state
            .records
            .iter()
            .map(|row| (&row.record, &row.body))
            .collect::<BTreeMap<_, _>>();
        changed.extend(
            changes
                .records
                .iter()
                .filter(|row| {
                    selected.contains(&row.record)
                        && previous_bodies
                            .get(&row.record)
                            .is_some_and(|old| **old != row.body)
                })
                .map(|row| row.record.clone()),
        );
        changed.sort();
        changed.dedup();
        let notification_owners =
            notifications::artifact_notification_owners(state, &changes, before, after, &changed);
        let records = state
            .records
            .iter()
            .chain(changes.records.iter())
            .map(|stored| (&stored.record, stored))
            .collect::<BTreeMap<_, _>>();
        let before_edges = selected_connections(state, &[], before);
        let after_edges = selected_connections(state, &changes.connections, after);
        let mut needed = changed.clone();
        let working = working::prepare(database, state, &changes)?;
        needed.extend(working.iter().flat_map(|update| update.dependencies()));
        needed.extend(notification_owners.iter().cloned());
        let inherited = changes
            .associations
            .iter()
            .filter(|association| {
                association.inherited
                    && !state
                        .associations
                        .iter()
                        .any(|old| old.association_id == association.association_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        needed.extend(inherited.iter().flat_map(|association| {
            [
                SemanticRecordRef::element(&association.semantic_element_id),
                SemanticRecordRef::artifact(&association.artifact_id),
            ]
        }));
        if adopted {
            needed.extend(state.records.iter().map(|stored| stored.record.clone()));
        }
        needed.extend(
            changes
                .records
                .iter()
                .filter(|stored| !matches!(stored.body, SemanticRevisionBody::Pruned))
                .map(|stored| stored.record.clone()),
        );
        if before_edges != after_edges {
            needed.extend(after_edges.iter().flat_map(|edge| {
                [
                    SemanticRecordRef::element(&edge.relationship.source_element_id),
                    SemanticRecordRef::element(&edge.relationship.target_element_id),
                ]
            }));
        }
        let (mut nodes, inserts) = prepare_nodes(database, &state.project_root, &records, needed)?;
        for record in &changed {
            if !nodes.contains_key(record) {
                nodes.insert(
                    record.clone(),
                    existing_projection_node(database, record, &state.project_root)?,
                );
            }
        }
        let known = nodes
            .iter()
            .filter(|(record, _)| record.kind == SemanticRecordKind::Element)
            .map(|(record, node)| {
                (
                    record.record_id.clone(),
                    node.unwrap_or_else(|| semantic_element_node_id_for(&record.record_id)),
                )
            })
            .collect();
        let connections = prepare_revision_connections(
            database,
            &state.project_root,
            &before_edges,
            &after_edges,
            &known,
            adopted,
        )?;
        let mut flags = changed
            .into_iter()
            .map(|record| {
                let active = selected.contains(&record);
                RevisionProjectionFlag::new(record, active, &records)
            })
            .collect::<Vec<_>>();
        let dirty = projection_dirty_elements(
            &records,
            &flags,
            connections
                .changed_element_node_ids()
                .keys()
                .chain(notification_owners.iter().map(|owner| &owner.record_id)),
        );
        notifications::include_notification_nodes(
            database,
            &state.project_root,
            &mut nodes,
            &dirty,
        )?;
        for insert in &inserts {
            if !flags.iter().any(|flag| flag.record == insert.record) {
                flags.push(RevisionProjectionFlag::new(
                    insert.record.clone(),
                    selected.contains(&insert.record),
                    &records,
                ));
            }
        }
        if adopted {
            for record in &state.records {
                if !flags.iter().any(|flag| flag.record == record.record) {
                    flags.push(RevisionProjectionFlag::new(
                        record.record.clone(),
                        selected.contains(&record.record),
                        &records,
                    ));
                }
            }
        }
        Ok(Self {
            nodes,
            inserts,
            flags,
            connections,
            dirty,
            inherited,
            working,
            project_root: state.project_root.clone(),
        })
    }

    pub(crate) fn apply(
        mut self,
        graph: &GraphTransaction<'_>,
        commit_version: i64,
        collector: &mut ChangeCollector,
    ) -> Result<()> {
        for insert in &self.inserts {
            let node = match insert.record.kind {
                SemanticRecordKind::Element => graph
                    .create_semantic_node(&insert.record.record_id, insert.properties.clone())?,
                SemanticRecordKind::Artifact => graph
                    .create_node_with_props(&["SemanticArtifact"], insert.properties.clone())?,
            };
            self.nodes.insert(insert.record.clone(), Some(node));
        }
        for insert in &self.inserts {
            if let Some(artifact) = &insert.artifact {
                link_revision_artifact(graph, &self.nodes, artifact)?;
            }
        }
        for update in self.working {
            update.apply(graph, &self.nodes)?;
        }
        for association in &self.inherited {
            let owner = required_node(
                &self.nodes,
                &SemanticRecordRef::element(&association.semantic_element_id),
            )?;
            let artifact = required_node(
                &self.nodes,
                &SemanticRecordRef::artifact(&association.artifact_id),
            )?;
            graph.create_edge_with_props(
                owner,
                artifact,
                SEMANTIC_ARTIFACT_EDGE_TYPE,
                [
                    (
                        SEMANTIC_ELEMENT_ID_PROPERTY,
                        GrafeoValue::from(association.semantic_element_id.clone()),
                    ),
                    (
                        ARTIFACT_ID_PROPERTY,
                        GrafeoValue::from(association.artifact_id.clone()),
                    ),
                    (
                        "revision_association_id",
                        GrafeoValue::from(association.association_id.clone()),
                    ),
                ],
            )?;
        }
        for flag in self.flags {
            let record = &flag.record;
            let node = self.nodes.get(&record).copied().flatten().ok_or_else(|| {
                DbError::invalid_value(&record.record_id, "a materialized selected revision")
            })?;
            graph.set_node_property(node, "revision_selected", GrafeoValue::from(flag.selected))?;
            graph.set_node_property(
                node,
                "project_root",
                GrafeoValue::from(self.project_root.clone()),
            )?;
            if record.kind == SemanticRecordKind::Element {
                let lifecycle = flag.lifecycle.as_deref().ok_or_else(|| {
                    DbError::invalid_value(&record.record_id, "the canonical element lifecycle")
                })?;
                let active = flag.selected && lifecycle != "inactive";
                graph.set_node_property(node, "active", GrafeoValue::from(active))?;
                graph.set_node_property(
                    node,
                    "lifecycle",
                    GrafeoValue::from(if flag.selected { lifecycle } else { "inactive" }),
                )?;
                graph.set_node_property(
                    node,
                    "last_changed_revision",
                    GrafeoValue::from(commit_version),
                )?;
                graph.set_node_property(
                    node,
                    "deleted_at",
                    GrafeoValue::from(if active { -1 } else { commit_version }),
                )?;
            }
        }
        apply_relationship_write(graph, self.connections)?;
        for (element, disposition) in self.dirty {
            let owner = required_node(
                &self.nodes,
                &SemanticRecordRef::element(&element.semantic_element_id),
            )?;
            graph.set_node_property(
                owner,
                LAST_CHANGED_REVISION_PROPERTY,
                GrafeoValue::from(commit_version),
            )?;
            collector.record_element(&element, disposition);
        }
        Ok(())
    }
}

fn selected_records(head: Option<&SemanticRevisionSelectionState>) -> BTreeSet<SemanticRecordRef> {
    head.into_iter()
        .flat_map(|head| head.records.iter().cloned())
        .collect()
}

fn selected_connections(
    state: &SemanticRevisionState,
    incoming: &[SemanticRevisionConnection],
    head: Option<&SemanticRevisionSelectionState>,
) -> Vec<SemanticRevisionConnection> {
    let ids = head
        .into_iter()
        .flat_map(|head| head.connection_ids.iter())
        .collect::<BTreeSet<_>>();
    state
        .connections
        .iter()
        .chain(incoming.iter())
        .filter(|edge| ids.contains(&edge.connection_id))
        .map(|edge| (edge.connection_id.clone(), edge.clone()))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

type RevisionNodeIndex = BTreeMap<SemanticRecordRef, Option<NodeId>>;

fn prepare_nodes(
    database: &GrafeoDB,
    root: &str,
    records: &BTreeMap<&SemanticRecordRef, &SemanticStoredRevision>,
    mut pending: Vec<SemanticRecordRef>,
) -> Result<(RevisionNodeIndex, Vec<RevisionNodeInsert>)> {
    let mut nodes = BTreeMap::new();
    let mut inserts = vec![];
    while let Some(record) = pending.pop() {
        if nodes.contains_key(&record) {
            continue;
        }
        let stored = records.get(&record).ok_or_else(|| {
            DbError::invalid_value(&record.record_id, "an available canonical revision")
        })?;
        let existing = existing_projection_node(database, &record, root)?;
        nodes.insert(record.clone(), existing);
        if existing.is_some() {
            continue;
        }
        pending.extend(stored.body.intrinsic_dependencies());
        if let SemanticRevisionBody::Artifact(artifact) = &stored.body {
            let owner = SemanticRecordRef::element(&artifact.semantic_element_id);
            if records
                .get(&owner)
                .is_some_and(|record| !matches!(record.body, SemanticRevisionBody::Pruned))
            {
                pending.push(owner);
            }
        }
        inserts.push(prepare_node_insert(stored, root)?);
    }
    Ok((nodes, inserts))
}

fn prepare_node_insert(stored: &SemanticStoredRevision, root: &str) -> Result<RevisionNodeInsert> {
    let (mut properties, artifact) = match &stored.body {
        SemanticRevisionBody::Element(element) => {
            (element_properties_with_change_state(element, 0)?, None)
        }
        SemanticRevisionBody::Artifact(artifact) => {
            (artifact_properties(artifact)?, Some(artifact.clone()))
        }
        SemanticRevisionBody::Pruned => {
            return Err(DbError::invalid_value(
                &stored.record.record_id,
                "an available body before materializing a revision",
            ));
        }
    };
    properties.push(("project_root", GrafeoValue::from(root)));
    Ok(RevisionNodeInsert {
        record: stored.record.clone(),
        properties,
        artifact,
    })
}

pub(crate) fn existing_projection_node(
    database: &GrafeoDB,
    record: &SemanticRecordRef,
    root: &str,
) -> Result<Option<NodeId>> {
    if let Some((owner, _)) =
        crate::local::grafeo::revision_rows::stored_revision_by_id(database, record)?
    {
        if owner != root {
            return Err(DbError::invalid_value(
                &record.record_id,
                format!("a canonical revision owned by project `{root}`"),
            ));
        }
    }

    let (label, property) = match record.kind {
        SemanticRecordKind::Element => ("SemanticElement", "semantic_element_id"),
        SemanticRecordKind::Artifact => ("SemanticArtifact", "artifact_id"),
    };
    let nodes = node_ids_by_label_and_property(database, label, property, &record.record_id);
    if nodes.len() > 1 {
        return Err(DbError::invalid_value(
            &record.record_id,
            "one native projection per canonical revision",
        ));
    }
    let Some(id) = nodes.first().copied() else {
        return Ok(None);
    };
    let node = database.get_node(id).ok_or_else(|| {
        DbError::invalid_value(&record.record_id, "the prepared native revision node")
    })?;
    let project = if record.kind == SemanticRecordKind::Element {
        string_property(&node, "project_root")
    } else {
        string_property(&node, "project_root").or_else(|| {
            string_property(&node, "semantic_element_id")
                .and_then(|owner| {
                    node_ids_by_label_and_property(
                        database,
                        "SemanticElement",
                        "semantic_element_id",
                        &owner,
                    )
                    .first()
                    .copied()
                })
                .and_then(|owner| database.get_node(owner))
                .and_then(|owner| string_property(&owner, "project_root"))
        })
    };
    let project = match project {
        Some(project) => Some(project),
        None => crate::local::grafeo::revision_rows::stored_revision_by_id(database, record)?
            .map(|(root, _)| root),
    };
    if project.as_deref() != Some(root) {
        return Err(DbError::invalid_value(
            &record.record_id,
            format!("a revision owned by project `{root}`"),
        ));
    }
    Ok(Some(id))
}

pub(crate) fn prepare_revision_body_collection(
    database: &GrafeoDB,
    root: &str,
    records: &[SemanticRecordRef],
) -> Result<Vec<NodeId>> {
    let mut nodes = BTreeSet::new();
    for record in records {
        nodes.extend(existing_projection_node(database, record, root)?);
        let (label, property) = match record.kind {
            SemanticRecordKind::Element => ("SemanticElementNameVector", "semantic_element_id"),
            SemanticRecordKind::Artifact => ("SemanticArtifactVector", "artifact_id"),
        };
        nodes.extend(node_ids_by_label_and_property(
            database,
            label,
            property,
            &record.record_id,
        ));
    }
    Ok(nodes.into_iter().collect())
}

fn link_revision_artifact(
    graph: &GraphTransaction<'_>,
    nodes: &RevisionNodeIndex,
    artifact: &SemanticArtifact,
) -> Result<()> {
    let source = required_node(nodes, &SemanticRecordRef::artifact(&artifact.artifact_id))?;
    if let Some(owner) = nodes
        .get(&SemanticRecordRef::element(&artifact.semantic_element_id))
        .copied()
        .flatten()
    {
        create_artifact_owner_edge(
            graph,
            owner,
            source,
            &artifact.artifact_id,
            &artifact.semantic_element_id,
        )?;
    }
    for dependency in &artifact.dependencies {
        let reference = match &dependency.target {
            ArtifactDependencyTarget::SemanticElement {
                semantic_element_id,
            } => SemanticRecordRef::element(semantic_element_id),
            ArtifactDependencyTarget::Artifact { artifact_id, .. } => {
                SemanticRecordRef::artifact(artifact_id)
            }
        };
        create_artifact_dependency_edge(
            graph,
            source,
            required_node(nodes, &reference)?,
            dependency,
        )?;
    }
    Ok(())
}

fn required_node(nodes: &RevisionNodeIndex, record: &SemanticRecordRef) -> Result<NodeId> {
    nodes
        .get(record)
        .copied()
        .flatten()
        .ok_or_else(|| DbError::invalid_value(&record.record_id, "a prepared exact revision node"))
}

fn projection_dirty_elements<'a>(
    records: &BTreeMap<&SemanticRecordRef, &SemanticStoredRevision>,
    flags: &[RevisionProjectionFlag],
    changed_owner_ids: impl Iterator<Item = &'a String>,
) -> Vec<(SemanticElement, ChangeDisposition)> {
    let mut changed = changed_owner_ids
        .map(|id| (SemanticRecordRef::element(id), ChangeDisposition::Upserted))
        .collect::<BTreeMap<_, _>>();
    for flag in flags {
        let record = &flag.record;
        if record.kind == SemanticRecordKind::Element {
            changed.insert(
                record.clone(),
                if flag.selected {
                    ChangeDisposition::Upserted
                } else {
                    ChangeDisposition::Removal
                },
            );
        }
    }
    changed
        .into_iter()
        .filter_map(|(record, disposition)| {
            records.get(&record).and_then(|stored| match &stored.body {
                SemanticRevisionBody::Element(element) => Some((element.clone(), disposition)),
                _ => None,
            })
        })
        .collect()
}
