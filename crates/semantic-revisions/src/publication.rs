//! Conditional publication: one source of identity, conflict and accepted-head decisions.
use crate::publication_records::{PreparedRevisionRecords, prepare_artifacts, prepare_structure};
use crate::revision_index::*;
use crate::selection::retirement_changes;
use crate::*;
use std::collections::BTreeSet;

impl SemanticRevisionState {
    /// Plans one atomic edit against the exact scope the writer observed.
    /// Example: `state.plan_publication(&edit, &timestamp, next_commit)?`.
    pub fn plan_publication(
        &self,
        request: &SemanticRevisionPublication,
        timestamp: &SemanticRevisionTimestamp,
        commit_version: i64,
    ) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
        plan_conditional_publication(self, request, timestamp, commit_version)
    }
}

pub(crate) fn plan_conditional_publication(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    timestamp: &SemanticRevisionTimestamp,
    commit_version: i64,
) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
    crate::citation_retention::require_general_selection(&request.selection_id)?;
    crate::citation_retention::require_general_selection(&request.operation_id)?;
    crate::citation_retention::require_general_selection(&request.source_context)?;
    plan_owned_publication(state, request, timestamp, commit_version)
}

pub(crate) fn plan_owned_publication(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    timestamp: &SemanticRevisionTimestamp,
    commit_version: i64,
) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
    validate_publication(state, request)?;
    let request_digest = publication_digest(state, request)?;
    if let Some(replay) = state.replay(&request.operation_id, &request_digest)? {
        return Ok(replay);
    }
    if let Some(conflict) = project_identity_conflict(
        state,
        &request.project_id,
        &request.selection_id,
        request.expected.generation,
    ) {
        return Ok(empty_plan(
            state,
            SemanticRevisionPublicationOutcome::Conflict(conflict),
        ));
    }
    validate_publication_basis(state, request, commit_version)?;
    let head = publication_head(state, request)?;
    if let Some(conflict) =
        publication_conflict(state, request, &head, request.expected.generation)?
    {
        return Ok(empty_plan(
            state,
            SemanticRevisionPublicationOutcome::Conflict(conflict),
        ));
    }
    if let Some(conflict) = crate::producer_base::producer_base_conflict(state, request)? {
        return Ok(empty_plan(
            state,
            SemanticRevisionPublicationOutcome::Conflict(conflict),
        ));
    }
    let structure = prepare_structure(state, request, &head, timestamp, commit_version)?;
    let artifacts =
        prepare_artifacts(state, request, &head, &structure, timestamp, commit_version)?;
    finish_publication(
        state,
        request,
        head,
        structure,
        artifacts,
        request_digest,
        timestamp,
        commit_version,
    )
}

fn validate_publication(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
) -> SemanticRevisionResult<()> {
    require_project_identity(&state.project_id)?;
    require_project_identity(&request.project_id)?;
    ensure_root(&request.project_root, &state.project_root)?;
    for id in [
        &request.operation_id,
        &request.selection_id,
        &request.source_context,
    ] {
        if id.trim().is_empty() {
            return Err(SemanticRevisionError::invalid_value(
                id,
                "a non-empty publication identity",
            ));
        }
    }
    crate::producer_base::validate_producer_base(request)?;
    if let Some(structure) = &request.structure {
        if let Some(partition) = &structure.partition {
            partition.validate()?;
            ensure_root(&partition.project_root, &state.project_root)?;
        }
        validate_structure_inputs(state, structure)?;
    }
    unique_nonempty(
        &request
            .artifacts
            .iter()
            .map(|write| write.artifact.artifact_id.clone())
            .collect::<Vec<_>>(),
        "one write per non-empty artifact ID",
    )?;
    unique_nonempty(
        &request.removed_artifact_ids,
        "unique non-empty artifact removals",
    )?;
    for write in &request.artifacts {
        write.artifact.validate()?;
        if write.media_type.trim().is_empty() {
            return Err(SemanticRevisionError::invalid_value(
                &write.media_type,
                "a non-empty artifact media type",
            ));
        }
        if request
            .removed_artifact_ids
            .contains(&write.artifact.artifact_id)
        {
            return Err(SemanticRevisionError::invalid_value(
                &write.artifact.artifact_id,
                "an artifact write or removal, never both",
            ));
        }
    }
    Ok(())
}

fn validate_publication_basis(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    commit_version: i64,
) -> SemanticRevisionResult<()> {
    if commit_version <= state.commit_version {
        return Err(SemanticRevisionError::invalid_value(
            commit_version.to_string(),
            format!("commit after {}", state.commit_version),
        ));
    }
    if let Some(structure) = &request.structure {
        if let Some(partition) = &structure.partition {
            let references = state
                .records
                .iter()
                .map(|stored| stored.record.clone())
                .collect::<Vec<_>>();
            partition
                .validate_ownership(&selected_elements(state, &references), &structure.elements)?;
        }
    }
    Ok(())
}

fn validate_structure_inputs(
    state: &SemanticRevisionState,
    structure: &SemanticRevisionStructure,
) -> SemanticRevisionResult<()> {
    unique_nonempty(
        &structure
            .elements
            .iter()
            .map(|element| element.semantic_element_id.clone())
            .collect::<Vec<_>>(),
        "one incoming element per non-empty ID",
    )?;
    for element in &structure.elements {
        element.validate()?;
        ensure_root(&element.project_root, &state.project_root)?;
        if structure
            .partition
            .as_ref()
            .is_some_and(|partition| !partition.contains_element(element))
        {
            return Err(SemanticRevisionError::invalid_value(
                &element.semantic_element_id,
                "an element inside the authoritative provider/path partition",
            ));
        }
    }
    for edge in &structure.relationships {
        edge.validate()?;
        ensure_root(&edge.project_root, &state.project_root)?;
    }
    Ok(())
}

fn publication_head(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
) -> SemanticRevisionResult<SemanticRevisionSelectionState> {
    crate::selection::require_context_selection(
        state,
        &request.selection_id,
        &request.source_context,
        SemanticSelectionMode::FollowAccepted,
    )?;
    if let Some(head) = state.selection(&request.selection_id) {
        if head.selection.source_context != request.source_context
            || head.selection.mode == SemanticSelectionMode::Pinned
        {
            return Err(SemanticRevisionError::invalid_value(
                &request.selection_id,
                "a follow-accepted selection in the publication's source context; explicitly update a pin first",
            ));
        }
        return Ok(head.clone());
    }
    Ok(SemanticRevisionSelectionState {
        selection: SemanticRevisionSelection {
            project_root: state.project_root.clone(),
            selection_id: request.selection_id.clone(),
            source_context: request.source_context.clone(),
            mode: SemanticSelectionMode::FollowAccepted,
            generation: 0,
            source_anchor: None,
        },
        records: vec![],
        connection_ids: vec![],
    })
}

fn scoped_elements(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    refs: &[SemanticRecordRef],
) -> BTreeSet<SemanticRecordRef> {
    let Some(structure) = &request.structure else {
        return BTreeSet::new();
    };
    let index = record_index(state);
    refs.iter()
        .filter(|reference| reference.kind == SemanticRecordKind::Element)
        .filter(|reference| {
            structure.partition.as_ref().is_none_or(|partition| {
                index
                    .get(*reference)
                    .and_then(|record| record.element_scope.as_ref())
                    .is_some_and(|scope| {
                        partition.contains_path(&scope.path)
                            && partition
                                .provider_instance_id
                                .as_ref()
                                .is_none_or(|provider| {
                                    scope.provider_instance_id.as_ref() == Some(provider)
                                })
                    })
            })
        })
        .cloned()
        .collect()
}

pub(crate) fn scoped_connections(
    state: &SemanticRevisionState,
    ids: &[String],
    sources: &BTreeSet<SemanticRecordRef>,
) -> BTreeSet<String> {
    let ids = ids.iter().collect::<BTreeSet<_>>();
    state
        .connections
        .iter()
        .filter(|edge| {
            ids.contains(&edge.connection_id)
                && edge
                    .relationship
                    .owned_element_ids()
                    .any(|id| sources.contains(&SemanticRecordRef::element(id)))
        })
        .map(|edge| edge.connection_id.clone())
        .collect()
}

fn artifact_scope(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    refs: &[SemanticRecordRef],
) -> SemanticRevisionResult<BTreeSet<SemanticRecordRef>> {
    let selected = refs.iter().cloned().collect::<BTreeSet<_>>();
    let successors = source_successors(state, &request.source_context);
    let mut scope = BTreeSet::new();
    for id in request
        .artifacts
        .iter()
        .map(|write| &write.artifact.artifact_id)
        .chain(request.removed_artifact_ids.iter())
    {
        if let Some(record) =
            selected_successor(&successors, &SemanticRecordRef::artifact(id), &selected)?
        {
            scope.insert(record);
        }
    }
    Ok(scope)
}

pub(crate) fn publication_conflict(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    head: &SemanticRevisionSelectionState,
    expected_generation: i64,
) -> SemanticRevisionResult<Option<SemanticRevisionConflict>> {
    let mut actual = scoped_elements(state, request, &head.records);
    let mut expected = scoped_elements(state, request, &request.expected.records);
    let actual_edges = scoped_connections(state, &head.connection_ids, &actual);
    let expected_edges = scoped_connections(state, &request.expected.connection_ids, &expected);
    actual.extend(artifact_scope(state, request, &head.records)?);
    expected.extend(artifact_scope(state, request, &request.expected.records)?);
    let mut changed_edits =
        crate::artifact_working::conflicting_edits(state, &request.expected, &actual);
    changed_edits.extend(crate::artifact_working::conflicting_dependencies(
        state, request,
    ));
    if actual == expected
        && actual_edges == expected_edges
        && changed_edits.is_empty()
        && expected_generation <= head.selection.generation
    {
        return Ok(None);
    }
    Ok(Some(SemanticRevisionConflict {
        project_root: state.project_root.clone(),
        expected_project_id: request.project_id.clone(),
        actual_project_id: state.project_id.clone(),
        selection_id: request.selection_id.clone(),
        expected_generation,
        actual_generation: head.selection.generation,
        records: actual
            .symmetric_difference(&expected)
            .cloned()
            .chain(changed_edits)
            .collect(),
        connection_ids: actual_edges
            .symmetric_difference(&expected_edges)
            .cloned()
            .collect(),
    }))
}

fn finish_publication(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    mut head: SemanticRevisionSelectionState,
    structure: PreparedRevisionRecords,
    artifacts: PreparedRevisionRecords,
    request_digest: String,
    timestamp: &SemanticRevisionTimestamp,
    commit_version: i64,
) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
    replace_publication_membership(
        &mut head,
        &structure,
        &artifacts,
        request.source_anchor.as_ref(),
    )?;
    let producer =
        crate::producer_base::advance_producer_base(state, request, &structure, &artifacts)?;
    let links = structure
        .links
        .iter()
        .chain(artifacts.links.iter())
        .cloned()
        .collect::<Vec<_>>();
    let receipt = SemanticRevisionPublicationReceipt {
        producer_base: producer.as_ref().map(|base| base.selection.clone()),
        operation_id: request.operation_id.clone(),
        selection: head.selection.clone(),
        accepted_scope: accepted_publication_scope(
            state,
            head.selection.generation,
            &structure,
            &artifacts,
        ),
        commit_version,
        remaps: structure
            .remaps
            .iter()
            .chain(artifacts.remaps.iter())
            .cloned()
            .collect(),
        revision_links: links.clone(),
    };
    let mut plan = empty_plan(
        state,
        SemanticRevisionPublicationOutcome::Accepted(receipt.clone()),
    );
    plan.selections.push(head);
    plan.selections.extend(producer);
    plan.selection_deltas = plan
        .selections
        .iter()
        .map(|next| {
            SemanticRevisionSelectionDelta::between(
                state.selection(&next.selection.selection_id),
                next,
                commit_version,
            )
        })
        .collect();
    let incoming = structure
        .records
        .iter()
        .chain(artifacts.records.iter())
        .cloned()
        .collect::<Vec<_>>();
    plan.records = retirement_changes(state, &plan.selections, &incoming, timestamp.unix_ms);
    plan.connections = structure.connections;
    plan.revision_links = links;
    plan.associations = inherited_associations(
        state,
        request,
        &plan,
        &artifacts.associations,
        timestamp.unix_ms,
    )?;
    let checkpoints = crate::artifact_working::checkpoints(request, &artifacts.remaps);
    crate::artifact_working::freeze_retained_states(state, &mut plan, &checkpoints);
    plan.accepted_operation = Some(SemanticRevisionAcceptedOperation {
        request_root: request.project_root.clone(),
        request_digest,
        receipt,
    });
    Ok(plan)
}

pub(crate) fn replace_publication_membership(
    head: &mut SemanticRevisionSelectionState,
    structure: &PreparedRevisionRecords,
    artifacts: &PreparedRevisionRecords,
    source_anchor: Option<&SemanticSourceAnchor>,
) -> SemanticRevisionResult<()> {
    let previous = head.clone();
    let removed = structure
        .replaced
        .iter()
        .chain(artifacts.replaced.iter())
        .collect::<BTreeSet<_>>();
    head.records.retain(|record| !removed.contains(record));
    head.records.extend(
        structure
            .selected
            .iter()
            .chain(artifacts.selected.iter())
            .cloned(),
    );
    head.records.sort();
    head.records.dedup();
    head.connection_ids
        .retain(|id| !structure.removed_connections.contains(id));
    head.connection_ids.extend(
        structure
            .connections
            .iter()
            .map(|edge| edge.connection_id.clone()),
    );
    head.connection_ids.sort();
    head.connection_ids.dedup();
    head.selection.generation = head.selection.generation.checked_add(1).ok_or_else(|| {
        SemanticRevisionError::invalid_value(
            head.selection.generation.to_string(),
            "an advanceable selection generation",
        )
    })?;
    if let Some(anchor) = source_anchor {
        head.selection.source_anchor = Some(anchor.clone());
    } else if !same_source_selection(&previous, &head) {
        if let Some(anchor) = &mut head.selection.source_anchor {
            anchor.dirty = true;
        }
    }
    Ok(())
}

fn accepted_publication_scope(
    state: &SemanticRevisionState,
    generation: i64,
    structure: &PreparedRevisionRecords,
    artifacts: &PreparedRevisionRecords,
) -> SemanticSelectionExpectation {
    SemanticSelectionExpectation {
        artifact_edits: crate::artifact_working::edit_versions(
            &artifacts.selected,
            &state
                .records
                .iter()
                .chain(&artifacts.records)
                .cloned()
                .collect::<Vec<_>>(),
        ),
        generation,
        records: structure
            .selected
            .iter()
            .chain(&artifacts.selected)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        connection_ids: structure
            .connections
            .iter()
            .map(|connection| connection.connection_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}

fn publication_digest(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
) -> SemanticRevisionResult<String> {
    let root = state
        .accepted_operations
        .iter()
        .find(|operation| operation.receipt.operation_id == request.operation_id)
        .map_or(request.project_root.as_str(), |operation| {
            operation.request_root.as_str()
        });
    if root == request.project_root {
        return digest(&("publication", request));
    }
    // An archive can rebind the project path without changing the acknowledged request's identity.
    let mut canonical = request.clone();
    canonical.project_root = root.into();
    if let Some(structure) = &mut canonical.structure {
        if let Some(partition) = &mut structure.partition {
            partition.project_root = root.into();
        }
        for element in &mut structure.elements {
            element.project_root = root.into();
        }
        for relationship in &mut structure.relationships {
            relationship.project_root = root.into();
        }
    }
    digest(&("publication", canonical))
}

fn same_source_selection(
    left: &SemanticRevisionSelectionState,
    right: &SemanticRevisionSelectionState,
) -> bool {
    let elements = |head: &SemanticRevisionSelectionState| {
        head.records
            .iter()
            .filter(|record| record.kind == SemanticRecordKind::Element)
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    elements(left) == elements(right)
        && left.connection_ids.iter().collect::<BTreeSet<_>>()
            == right.connection_ids.iter().collect::<BTreeSet<_>>()
}

fn inherited_associations(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    plan: &SemanticRevisionPublicationPlan,
    direct: &[SemanticRevisionAssociation],
    now_ms: i64,
) -> SemanticRevisionResult<Vec<SemanticRevisionAssociation>> {
    let mut associations = direct.to_vec();
    let selected = plan
        .selections
        .iter()
        .find(|head| head.selection.selection_id == request.selection_id)
        .into_iter()
        .flat_map(|head| head.records.iter())
        .filter(|record| record.kind == SemanticRecordKind::Artifact)
        .map(|record| &record.record_id)
        .collect::<BTreeSet<_>>();
    for link in &plan.revision_links {
        if link.older.kind != SemanticRecordKind::Element {
            continue;
        }
        for old in state.associations.iter().filter(|association| {
            association.source_context == request.source_context
                && association.semantic_element_id == link.older.record_id
                && selected.contains(&association.artifact_id)
        }) {
            let id = digest(&(
                &old.artifact_id,
                &link.newer.record_id,
                &request.source_context,
            ))?;
            associations.push(SemanticRevisionAssociation {
                association_id: id,
                artifact_id: old.artifact_id.clone(),
                semantic_element_id: link.newer.record_id.clone(),
                source_context: request.source_context.clone(),
                created_at_ms: now_ms,
                inherited: true,
            });
        }
    }
    Ok(associations)
}
