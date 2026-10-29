//! Authoritative publication facts are explicit: an omitted scope is never an empty project.
use crate::*;
use serde::{Deserialize, Serialize};

/// The complete structure scope fetched by an adapter, independent of artifact/dependency facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "partition", rename_all = "snake_case")]
pub enum SemanticRevisionStructureCoverage {
    None,
    Project,
    Partition(SemanticPartition),
}

/// An exact membership read for a writer's scope; it does not hydrate the complete graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionSelectionQuery {
    pub project_root: String,
    pub selection_id: String,
    pub structure_coverage: SemanticRevisionStructureCoverage,
    pub artifact_ids: Vec<String>,
}

/// A scope-complete basis with touched bodies, ancestry, dependencies, associations and intersecting pins.
/// Adapters must include every current/observed owned connection and record in the declared scope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionPublicationBasis {
    pub structure_coverage: SemanticRevisionStructureCoverage,
    pub artifact_ids: Vec<String>,
    pub facts: SemanticRevisionState,
}

impl SemanticRevisionPublicationBasis {
    /// Plans against explicitly scoped authoritative facts, preserving all omitted memberships.
    /// Example: `basis.plan_publication(&edit, &timestamp, next_commit)?`.
    pub fn plan_publication(
        &self,
        request: &SemanticRevisionPublication,
        timestamp: &SemanticRevisionTimestamp,
        commit_version: i64,
    ) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
        if self.structure_coverage != structure_coverage(request)
            || self.artifact_ids != requested_artifact_ids(request)
        {
            return Err(SemanticRevisionError::invalid_value(
                &request.operation_id,
                "complete authoritative facts for this publication's exact structure/artifact scope",
            ));
        }
        crate::publication::plan_conditional_publication(
            &self.facts,
            request,
            timestamp,
            commit_version,
        )
    }
}

impl SemanticRevisionState {
    /// Reads the authoritative token only for records/connections a writer may replace.
    /// Example: `state.selection_members(&query)?` before constructing a conditional edit.
    pub fn selection_members(
        &self,
        query: &SemanticRevisionSelectionQuery,
    ) -> SemanticRevisionResult<Option<SemanticRevisionSelectionObservation>> {
        crate::revision_index::ensure_root(&query.project_root, &self.project_root)?;
        if let SemanticRevisionStructureCoverage::Partition(partition) = &query.structure_coverage {
            partition.validate()?;
            crate::revision_index::ensure_root(&partition.project_root, &self.project_root)?;
        }
        let Some(head) = self.selection(&query.selection_id) else {
            return Ok(None);
        };
        let elements = scoped_selected_elements(self, head, &query.structure_coverage);
        let mut records = elements
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let successors =
            crate::revision_index::source_successors(self, &head.selection.source_context);
        let selected = head.records.iter().cloned().collect();
        for id in &query.artifact_ids {
            if let Some(record) = crate::revision_index::selected_successor(
                &successors,
                &SemanticRecordRef::artifact(id),
                &selected,
            )? {
                records.insert(record);
            }
        }
        let scoped = SemanticRevisionSelectionState {
            selection: head.selection.clone(),
            records: records.into_iter().collect(),
            connection_ids: crate::publication::scoped_connections(
                self,
                &head.connection_ids,
                &elements,
            )
            .into_iter()
            .collect(),
        };
        Ok(Some(SemanticRevisionSelectionObservation {
            selection: scoped.selection.clone(),
            expected: scoped.expectation(&self.records),
        }))
    }
}

fn scoped_selected_elements(
    state: &SemanticRevisionState,
    head: &SemanticRevisionSelectionState,
    coverage: &SemanticRevisionStructureCoverage,
) -> std::collections::BTreeSet<SemanticRecordRef> {
    let index = crate::revision_index::record_index(state);
    head.records
        .iter()
        .filter(|record| record.kind == SemanticRecordKind::Element)
        .filter(|record| match coverage {
            SemanticRevisionStructureCoverage::None => false,
            SemanticRevisionStructureCoverage::Project => true,
            SemanticRevisionStructureCoverage::Partition(partition) => index
                .get(*record)
                .and_then(|stored| stored.element_scope.as_ref())
                .is_some_and(|scope| {
                    partition.contains_path(&scope.path)
                        && partition
                            .provider_instance_id
                            .as_ref()
                            .is_none_or(|provider| {
                                scope.provider_instance_id.as_ref() == Some(provider)
                            })
                }),
        })
        .cloned()
        .collect()
}

/// Describes required structure coverage before storage reads. Example: `structure_coverage(&edit)`.
pub fn structure_coverage(
    request: &SemanticRevisionPublication,
) -> SemanticRevisionStructureCoverage {
    match request.structure.as_ref() {
        None => SemanticRevisionStructureCoverage::None,
        Some(structure) => structure.partition.clone().map_or(
            SemanticRevisionStructureCoverage::Project,
            SemanticRevisionStructureCoverage::Partition,
        ),
    }
}

/// Returns stable artifact scope keys for indexed basis loading. Example: `requested_artifact_ids(&edit)`.
pub fn requested_artifact_ids(request: &SemanticRevisionPublication) -> Vec<String> {
    request
        .artifacts
        .iter()
        .map(|write| write.artifact.artifact_id.clone())
        .chain(request.removed_artifact_ids.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}
