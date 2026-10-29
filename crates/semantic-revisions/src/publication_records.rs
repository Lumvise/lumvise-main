//! Element revision allocation, working artifact updates and exact endpoint remapping.
use crate::revision_index::*;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(crate) struct PreparedRevisionRecords {
    pub records: Vec<SemanticStoredRevision>,
    pub selected: Vec<SemanticRecordRef>,
    pub replaced: Vec<SemanticRecordRef>,
    pub remaps: Vec<SemanticRevisionRemap>,
    pub links: Vec<SemanticRevisionLink>,
    pub connections: Vec<SemanticRevisionConnection>,
    pub removed_connections: BTreeSet<String>,
    pub associations: Vec<SemanticRevisionAssociation>,
}

pub(crate) fn prepare_structure(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    head: &SemanticRevisionSelectionState,
    timestamp: &SemanticRevisionTimestamp,
    commit_version: i64,
) -> SemanticRevisionResult<PreparedRevisionRecords> {
    let Some(structure) = &request.structure else {
        return Ok(PreparedRevisionRecords::default());
    };
    let existing = selected_elements(state, &head.records)
        .into_iter()
        .filter(|element| {
            structure
                .partition
                .as_ref()
                .is_none_or(|partition| partition.contains_element(element))
        })
        .collect::<Vec<_>>();
    let reconciled = reconcile_structure(
        state,
        request,
        head,
        &existing,
        &structure.elements,
        timestamp,
    )?;
    let mut plan = allocate_elements(
        state,
        request,
        &existing,
        reconciled,
        timestamp,
        commit_version,
    )?;
    plan.replaced = existing
        .iter()
        .map(|element| SemanticRecordRef::element(&element.semantic_element_id))
        .collect();
    plan.removed_connections = owned_connection_ids(state, head, &plan.replaced);
    plan.connections = prepare_connections(state, request, head, &plan, &structure.relationships)?;
    Ok(plan)
}

fn reconcile_structure(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    head: &SemanticRevisionSelectionState,
    existing: &[SemanticElement],
    incoming: &[SemanticElement],
    timestamp: &SemanticRevisionTimestamp,
) -> SemanticRevisionResult<Vec<(String, SemanticElement)>> {
    let selected = head.records.iter().cloned().collect::<BTreeSet<_>>();
    let successors = source_successors(state, &request.source_context);
    let mut aliases = BTreeMap::new();
    for element in incoming {
        let original = SemanticRecordRef::element(&element.semantic_element_id);
        if let Some(accepted) = selected_successor(&successors, &original, &selected)? {
            aliases.insert(original.record_id, accepted.record_id);
        }
    }
    let normalized = incoming
        .iter()
        .cloned()
        .map(|mut element| {
            element.semantic_element_id = aliases
                .get(&element.semantic_element_id)
                .cloned()
                .unwrap_or(element.semantic_element_id);
            element.parent_element_id = element
                .parent_element_id
                .map(|id| aliases.get(&id).cloned().unwrap_or(id));
            element
        })
        .collect::<Vec<_>>();
    unique_nonempty(
        &normalized
            .iter()
            .map(|element| element.semantic_element_id.clone())
            .collect::<Vec<_>>(),
        "one incoming record per selected revision family",
    )?;
    let reconciled =
        SemanticStructureReconciliation::between(existing, &normalized, &timestamp.rfc3339);
    let resolved = reconciled
        .remaps
        .iter()
        .map(|remap| (&remap.incoming_id, &remap.resolved_id))
        .collect::<BTreeMap<_, _>>();
    Ok(incoming
        .iter()
        .zip(reconciled.active_elements)
        .map(|(original, mut element)| {
            if let Some(parent) = &element.parent_element_id {
                element.parent_element_id = Some(
                    resolved
                        .get(parent)
                        .map_or_else(|| parent.clone(), |id| (*id).clone()),
                );
            }
            (original.semantic_element_id.clone(), element)
        })
        .collect())
}

fn allocate_elements(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    existing: &[SemanticElement],
    incoming: Vec<(String, SemanticElement)>,
    timestamp: &SemanticRevisionTimestamp,
    commit_version: i64,
) -> SemanticRevisionResult<PreparedRevisionRecords> {
    let old = existing
        .iter()
        .map(|element| (&element.semantic_element_id, element))
        .collect::<BTreeMap<_, _>>();
    let mut changed = changed_element_ids(state, &incoming, &old);
    propagate_changed_parents(&incoming, &mut changed);
    let mut ids = BTreeMap::new();
    for (_, element) in &incoming {
        let previous = SemanticRecordRef::element(&element.semantic_element_id);
        let id = if changed.contains(&element.semantic_element_id) {
            revision_id(
                &state.project_id,
                &request.source_context,
                &request.operation_id,
                &previous,
            )?
        } else {
            element.semantic_element_id.clone()
        };
        ids.insert(element.semantic_element_id.clone(), id);
    }
    finish_elements(
        state,
        request,
        incoming,
        &old,
        &ids,
        timestamp,
        commit_version,
    )
}

fn changed_element_ids(
    state: &SemanticRevisionState,
    incoming: &[(String, SemanticElement)],
    selected: &BTreeMap<&String, &SemanticElement>,
) -> BTreeSet<String> {
    let index = record_index(state);
    incoming
        .iter()
        .filter_map(|(_, element)| {
            let previous = selected
                .get(&element.semantic_element_id)
                .copied()
                .or_else(|| {
                    index
                        .get(&SemanticRecordRef::element(&element.semantic_element_id))
                        .and_then(|stored| match &stored.body {
                            SemanticRevisionBody::Element(old) => Some(old),
                            _ => None,
                        })
                });
            previous
                .filter(|old| !same_element_body(element, old))
                .map(|_| element.semantic_element_id.clone())
        })
        .collect()
}

fn same_element_body(incoming: &SemanticElement, previous: &SemanticElement) -> bool {
    let mut comparable = incoming.clone();
    match (&mut comparable.match_evidence, &previous.match_evidence) {
        (Some(incoming), Some(previous)) => incoming.matched_at = previous.matched_at.clone(),
        (evidence @ None, Some(previous)) => *evidence = Some(previous.clone()),
        _ => {}
    }
    &comparable == previous
}

fn propagate_changed_parents(
    incoming: &[(String, SemanticElement)],
    changed: &mut BTreeSet<String>,
) {
    if changed.is_empty() {
        return;
    }
    let children = revision_children(incoming);
    let mut pending = changed.iter().cloned().collect::<Vec<_>>();
    while let Some(parent) = pending.pop() {
        for child in children.get(parent.as_str()).into_iter().flatten() {
            if changed.insert((*child).to_owned()) {
                pending.push((*child).to_owned());
            }
        }
    }
}

fn revision_children(incoming: &[(String, SemanticElement)]) -> BTreeMap<&str, Vec<&str>> {
    let mut children = BTreeMap::<_, Vec<_>>::new();
    for (_, element) in incoming {
        if let Some(parent) = element.parent_element_id.as_deref() {
            children
                .entry(parent)
                .or_default()
                .push(element.semantic_element_id.as_str());
        }
    }
    children
}

fn finish_elements(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    incoming: Vec<(String, SemanticElement)>,
    old: &BTreeMap<&String, &SemanticElement>,
    ids: &BTreeMap<String, String>,
    timestamp: &SemanticRevisionTimestamp,
    commit_version: i64,
) -> SemanticRevisionResult<PreparedRevisionRecords> {
    let index = record_index(state);
    let mut plan = PreparedRevisionRecords::default();
    for (original, mut element) in incoming {
        let previous = SemanticRecordRef::element(&element.semantic_element_id);
        let accepted = SemanticRecordRef::element(&ids[&element.semantic_element_id]);
        element.semantic_element_id = accepted.record_id.clone();
        element.parent_element_id = element
            .parent_element_id
            .map(|parent| ids.get(&parent).cloned().unwrap_or(parent));
        if accepted == previous {
            if let Some(previous) = old.get(&accepted.record_id) {
                element.match_evidence = previous.match_evidence.clone();
            }
        }
        record_body(
            &mut plan,
            &index,
            accepted.clone(),
            SemanticRevisionBody::Element(element),
            timestamp.unix_ms,
        )?;
        plan.remaps.push(SemanticRevisionRemap {
            incoming: SemanticRecordRef::element(original),
            accepted: accepted.clone(),
        });
        if accepted != previous && index.contains_key(&previous) {
            plan.links.push(revision_link(
                request,
                previous,
                accepted.clone(),
                timestamp.unix_ms,
                commit_version,
            ));
        }
        plan.selected.push(accepted);
    }
    Ok(plan)
}

pub(crate) fn record_body(
    plan: &mut PreparedRevisionRecords,
    index: &BTreeMap<SemanticRecordRef, &SemanticStoredRevision>,
    reference: SemanticRecordRef,
    body: SemanticRevisionBody,
    now_ms: i64,
) -> SemanticRevisionResult<()> {
    if let Some(old) = index.get(&reference) {
        if old.body == body {
            return Ok(());
        }
        return Err(SemanticRevisionError::invalid_value(
            &reference.record_id,
            "an unused revision ID or its identical immutable payload",
        ));
    }
    plan.records
        .push(stored_body(reference, body, true, now_ms));
    Ok(())
}

pub(crate) fn revision_link(
    request: &SemanticRevisionPublication,
    older: SemanticRecordRef,
    newer: SemanticRecordRef,
    now_ms: i64,
    commit_version: i64,
) -> SemanticRevisionLink {
    SemanticRevisionLink {
        project_root: request.project_root.clone(),
        source_context: request.source_context.clone(),
        older,
        newer,
        created_at_ms: now_ms,
        commit_version,
    }
}

fn require_available(
    available: &BTreeSet<SemanticRecordRef>,
    reference: &SemanticRecordRef,
) -> SemanticRevisionResult<()> {
    if available.contains(reference) {
        return Ok(());
    }
    Err(SemanticRevisionError::invalid_value(
        &reference.record_id,
        "an available exact parent, endpoint or dependency revision in this project",
    ))
}

fn owned_connection_ids(
    state: &SemanticRevisionState,
    head: &SemanticRevisionSelectionState,
    owned: &[SemanticRecordRef],
) -> BTreeSet<String> {
    let owned = owned.iter().collect::<BTreeSet<_>>();
    let selected = head.connection_ids.iter().collect::<BTreeSet<_>>();
    state
        .connections
        .iter()
        .filter(|edge| {
            selected.contains(&edge.connection_id)
                && (owned.contains(&SemanticRecordRef::element(
                    &edge.relationship.source_element_id,
                )) || (edge.relationship.relationship_kind == "contains"
                    && owned.contains(&SemanticRecordRef::element(
                        &edge.relationship.target_element_id,
                    ))))
        })
        .map(|edge| edge.connection_id.clone())
        .collect()
}

fn prepare_connections(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    head: &SemanticRevisionSelectionState,
    plan: &PreparedRevisionRecords,
    relationships: &[SemanticRelationship],
) -> SemanticRevisionResult<Vec<SemanticRevisionConnection>> {
    let ids = plan
        .remaps
        .iter()
        .map(|remap| {
            (
                remap.incoming.record_id.clone(),
                remap.accepted.record_id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut edges = relationships
        .iter()
        .cloned()
        .map(|mut edge| {
            edge.source_element_id = ids
                .get(&edge.source_element_id)
                .cloned()
                .unwrap_or(edge.source_element_id);
            edge.target_element_id = ids
                .get(&edge.target_element_id)
                .cloned()
                .unwrap_or(edge.target_element_id);
            edge
        })
        .collect::<Vec<_>>();
    let mut containment = edges
        .iter()
        .filter(|edge| edge.relationship_kind == "contains")
        .map(|edge| {
            (
                edge.source_element_id.clone(),
                edge.target_element_id.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let stored = state
        .records
        .iter()
        .chain(plan.records.iter())
        .map(|record| (&record.record, record))
        .collect::<BTreeMap<_, _>>();
    for reference in &plan.selected {
        if let Some(SemanticStoredRevision {
            body: SemanticRevisionBody::Element(element),
            ..
        }) = stored.get(reference).copied()
        {
            if let Some(parent) = &element.parent_element_id {
                add_containment(
                    &mut edges,
                    &mut containment,
                    &request.project_root,
                    parent,
                    &element.semantic_element_id,
                );
            }
        }
    }
    let available = state
        .records
        .iter()
        .chain(plan.records.iter())
        .filter(|stored| !matches!(stored.body, SemanticRevisionBody::Pruned))
        .map(|stored| stored.record.clone())
        .collect::<BTreeSet<_>>();
    let owned = plan.selected.iter().cloned().collect::<BTreeSet<_>>();
    validate_connections(&edges, &available, &owned)?;
    crate::connection_identities::reconcile_connections(
        &state.connections,
        &head.connection_ids,
        &edges,
    )
}

fn add_containment(
    edges: &mut Vec<SemanticRelationship>,
    containment: &mut BTreeSet<(String, String)>,
    root: &str,
    parent: &str,
    child: &str,
) {
    if !containment.insert((parent.into(), child.into())) {
        return;
    }
    edges.push(SemanticRelationship {
        project_root: root.into(),
        source_element_id: parent.into(),
        target_element_id: child.into(),
        relationship_kind: "contains".into(),
        label: "contains".into(),
        metadata: serde_json::json!({ "derived_from": "parent_element_id" }),
    });
}

fn validate_connections(
    edges: &[SemanticRelationship],
    available: &BTreeSet<SemanticRecordRef>,
    owned: &BTreeSet<SemanticRecordRef>,
) -> SemanticRevisionResult<()> {
    for edge in edges {
        let source = SemanticRecordRef::element(&edge.source_element_id);
        let target = SemanticRecordRef::element(&edge.target_element_id);
        require_available(available, &source)?;
        require_available(available, &target)?;
        if !owned.contains(&source)
            && !(edge.relationship_kind == "contains" && owned.contains(&target))
        {
            return Err(SemanticRevisionError::invalid_value(
                &edge.source_element_id,
                "a connection owned by this authoritative structure scope",
            ));
        }
    }
    Ok(())
}

struct ArtifactRevisionIndex<'a> {
    required: BTreeSet<SemanticRecordRef>,
    selected: BTreeSet<SemanticRecordRef>,
    records: BTreeMap<SemanticRecordRef, &'a SemanticStoredRevision>,
    successors: BTreeMap<&'a SemanticRecordRef, Vec<&'a SemanticRecordRef>>,
    associations: BTreeMap<&'a str, Vec<&'a SemanticRevisionAssociation>>,
}

impl<'a> ArtifactRevisionIndex<'a> {
    fn new(
        state: &'a SemanticRevisionState,
        head: &SemanticRevisionSelectionState,
        request: &SemanticRevisionPublication,
    ) -> Self {
        let selected = head.records.iter().cloned().collect();
        let associations =
            selected_artifact_associations(state, &selected, &request.source_context);
        Self {
            required: crate::artifact_working::publication_dependencies(request),
            selected,
            associations,
            records: record_index(state),
            successors: source_successors(state, &request.source_context),
        }
    }

    fn matches_owner(
        &self,
        artifact: &str,
        owner: &str,
        project_owner: impl Fn(&str) -> SemanticRevisionResult<String>,
    ) -> SemanticRevisionResult<bool> {
        for association in self.associations.get(artifact).into_iter().flatten() {
            if project_owner(&association.semantic_element_id)? == owner {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn selected_artifact_associations<'a>(
    state: &'a SemanticRevisionState,
    selected: &BTreeSet<SemanticRecordRef>,
    context: &str,
) -> BTreeMap<&'a str, Vec<&'a SemanticRevisionAssociation>> {
    let mut associations: BTreeMap<&str, Vec<&SemanticRevisionAssociation>> = BTreeMap::new();
    for association in &state.associations {
        if association.source_context == context
            && selected.contains(&SemanticRecordRef::element(
                &association.semantic_element_id,
            ))
        {
            associations
                .entry(&association.artifact_id)
                .or_default()
                .push(association);
        }
    }
    associations
}

pub(crate) fn prepare_artifacts(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    head: &SemanticRevisionSelectionState,
    structure: &PreparedRevisionRecords,
    timestamp: &SemanticRevisionTimestamp,
    commit_version: i64,
) -> SemanticRevisionResult<PreparedRevisionRecords> {
    let mut plan = PreparedRevisionRecords::default();
    let facts = ArtifactRevisionIndex::new(state, head, request);
    for write in &request.artifacts {
        prepare_artifact(
            &state.project_id,
            request,
            write,
            &facts,
            structure,
            &mut plan,
            timestamp.unix_ms,
            commit_version,
        )?;
    }
    for id in &request.removed_artifact_ids {
        if let Some(previous) = selected_successor(
            &facts.successors,
            &SemanticRecordRef::artifact(id),
            &facts.selected,
        )? {
            plan.replaced.push(previous);
        }
    }
    validate_publication_dependencies(state, structure, &plan)?;
    Ok(plan)
}

fn prepare_artifact(
    project_id: &str,
    request: &SemanticRevisionPublication,
    write: &SemanticArtifactRevisionWrite,
    facts: &ArtifactRevisionIndex<'_>,
    structure: &PreparedRevisionRecords,
    plan: &mut PreparedRevisionRecords,
    now_ms: i64,
    commit_version: i64,
) -> SemanticRevisionResult<()> {
    let original = SemanticRecordRef::artifact(&write.artifact.artifact_id);
    let previous = selected_successor(&facts.successors, &original, &facts.selected)?
        .unwrap_or(original.clone());
    let mut artifact = write.stored_artifact(project_id)?;
    artifact.artifact_id = previous.record_id.clone();
    let project_owner =
        |id: &str| selected_artifact_owner(id, &facts.successors, &facts.selected, structure);
    let owner = project_owner(&artifact.semantic_element_id)?;
    artifact.semantic_element_id = owner.clone();
    artifact = unchanged_artifact_body(artifact, &previous, &owner, facts, project_owner)?;
    let changed = facts
        .records
        .get(&previous)
        .is_some_and(|old| old.body != SemanticRevisionBody::Artifact(artifact.clone()));
    let working = !write.checkpoint
        && facts
            .records
            .get(&previous)
            .is_some_and(|old| crate::artifact_working::can_edit(old, &artifact, &facts.required));
    let accepted = if changed && !working {
        SemanticRecordRef::artifact(revision_id(
            project_id,
            &request.source_context,
            &request.operation_id,
            &previous,
        )?)
    } else {
        previous.clone()
    };
    artifact.artifact_id = accepted.record_id.clone();
    if changed && working {
        plan.records.push(crate::artifact_working::update_working(
            facts.records[&previous],
            artifact,
        )?);
    } else {
        record_body(
            plan,
            &facts.records,
            accepted.clone(),
            SemanticRevisionBody::Artifact(artifact),
            now_ms,
        )?;
    }
    plan.remaps.push(SemanticRevisionRemap {
        incoming: original,
        accepted: accepted.clone(),
    });
    plan.replaced.push(previous.clone());
    plan.selected.push(accepted.clone());
    if accepted != previous {
        plan.links.push(revision_link(
            request,
            previous,
            accepted.clone(),
            now_ms,
            commit_version,
        ));
    }
    plan.associations.push(SemanticRevisionAssociation {
        association_id: digest(&(&accepted.record_id, &owner, &request.source_context))?,
        artifact_id: accepted.record_id,
        semantic_element_id: owner,
        source_context: request.source_context.clone(),
        created_at_ms: now_ms,
        inherited: false,
    });
    Ok(())
}

fn selected_artifact_owner(
    id: &str,
    successors: &BTreeMap<&SemanticRecordRef, Vec<&SemanticRecordRef>>,
    selected: &BTreeSet<SemanticRecordRef>,
    structure: &PreparedRevisionRecords,
) -> SemanticRevisionResult<String> {
    if let Some(remap) = structure.remaps.iter().find(|remap| {
        remap.incoming.kind == SemanticRecordKind::Element && remap.incoming.record_id == id
    }) {
        return Ok(remap.accepted.record_id.clone());
    }
    let current = selected_successor(successors, &SemanticRecordRef::element(id), selected)?
        .map_or_else(|| id.to_owned(), |owner| owner.record_id);
    Ok(structure
        .links
        .iter()
        .find(|link| {
            link.older.kind == SemanticRecordKind::Element && link.older.record_id == current
        })
        .map_or(current.clone(), |link| link.newer.record_id.clone()))
}

fn unchanged_artifact_body(
    incoming: SemanticArtifact,
    previous_record: &SemanticRecordRef,
    owner: &str,
    facts: &ArtifactRevisionIndex<'_>,
    project_owner: impl Fn(&str) -> SemanticRevisionResult<String>,
) -> SemanticRevisionResult<SemanticArtifact> {
    let Some(SemanticRevisionBody::Artifact(previous)) = facts
        .records
        .get(previous_record)
        .map(|stored| &stored.body)
    else {
        return Ok(incoming);
    };
    if project_owner(&previous.semantic_element_id)? != owner
        && !facts.matches_owner(&previous.artifact_id, owner, &project_owner)?
    {
        return Ok(incoming);
    }
    let mut comparable = incoming.clone();
    comparable.semantic_element_id = previous.semantic_element_id.clone();
    Ok(if &comparable == previous {
        comparable
    } else {
        incoming
    })
}

fn validate_publication_dependencies(
    state: &SemanticRevisionState,
    structure: &PreparedRevisionRecords,
    plan: &PreparedRevisionRecords,
) -> SemanticRevisionResult<()> {
    let available = state
        .records
        .iter()
        .chain(structure.records.iter())
        .chain(plan.records.iter())
        .filter(|stored| !matches!(stored.body, SemanticRevisionBody::Pruned))
        .map(|stored| stored.record.clone())
        .collect::<BTreeSet<_>>();
    for stored in structure.records.iter().chain(&plan.records) {
        if let SemanticRevisionBody::Artifact(artifact) = &stored.body {
            require_available(
                &available,
                &SemanticRecordRef::element(&artifact.semantic_element_id),
            )?;
        }
        for target in stored.body.intrinsic_dependencies() {
            require_available(&available, &target)?;
        }
    }
    Ok(())
}
