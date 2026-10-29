//! Owns consistent selection projections and exact evidence closure. Adapters call
//! `snapshot`; required records never become primary members or publication grants.
use crate::required_revisions::RevisionDependencyIndex;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

impl SemanticRevisionState {
    /// Reads one head, its contextual attachments and exact required evidence.
    /// Example: `state.snapshot("working", "2026-10-24T00:00:00Z")?`.
    pub fn snapshot(
        &self,
        selection_id: &str,
        published_at: &str,
    ) -> SemanticRevisionResult<Option<SemanticRevisionSelectionSnapshot>> {
        let snapshot = self
            .selection(selection_id)
            .map(|head| SelectionReadProjection::new(self, head).snapshot(published_at))
            .transpose()?;
        if selection_id == "public" {
            if let Some(snapshot) = &snapshot {
                require_published_dependencies(snapshot)?;
            }
        }
        Ok(snapshot)
    }
}

fn require_published_dependencies(
    snapshot: &SemanticRevisionSelectionSnapshot,
) -> SemanticRevisionResult<()> {
    let selected = snapshot.expectation.records.iter().collect::<BTreeSet<_>>();
    if let Some(record) = snapshot
        .required_records
        .iter()
        .find(|record| !selected.contains(&record.record))
    {
        return Err(SemanticRevisionError::invalid_value(
            &record.record.record_id,
            "explicitly published dependency membership before public disclosure",
        ));
    }
    Ok(())
}

struct SelectionReadProjection<'a> {
    state: &'a SemanticRevisionState,
    head: &'a SemanticRevisionSelectionState,
    dependencies: RevisionDependencyIndex<'a>,
    selected: BTreeSet<SemanticRecordRef>,
    hidden_associations: BTreeSet<String>,
}

impl<'a> SelectionReadProjection<'a> {
    fn new(state: &'a SemanticRevisionState, head: &'a SemanticRevisionSelectionState) -> Self {
        let selected = head.records.iter().cloned().collect::<BTreeSet<_>>();
        let hidden_associations = state
            .associations
            .iter()
            .filter(|association| {
                association.source_context != head.selection.source_context
                    || !selected.contains(&SemanticRecordRef::element(
                        &association.semantic_element_id,
                    ))
            })
            .map(|association| association.association_id.clone())
            .collect();
        Self {
            state,
            head,
            dependencies: RevisionDependencyIndex::new(state),
            selected,
            hidden_associations,
        }
    }

    fn snapshot(
        &self,
        published_at: &str,
    ) -> SemanticRevisionResult<SemanticRevisionSelectionSnapshot> {
        let (elements, artifacts) = self.current_bodies()?;
        let connections = self.connections()?;
        let required_records = self.required_records(&connections)?;
        let associations = self.associations(&required_records);
        Ok(SemanticRevisionSelectionSnapshot {
            selection: self.head.selection.clone(),
            expectation: self.head.expectation(&self.state.records),
            commit_version: self.state.commit_version,
            published_at: published_at.into(),
            elements,
            artifacts,
            connections,
            required_records,
            associations,
        })
    }

    fn current_bodies(
        &self,
    ) -> SemanticRevisionResult<(Vec<SemanticElement>, Vec<SemanticArtifact>)> {
        let (mut elements, mut artifacts) = (vec![], vec![]);
        for record in &self.head.records {
            match &self.dependencies.require(record)?.body {
                SemanticRevisionBody::Element(element) => elements.push(element.clone()),
                SemanticRevisionBody::Artifact(artifact) => artifacts.push(artifact.clone()),
                SemanticRevisionBody::Pruned => unreachable!(),
            }
        }
        Ok((elements, artifacts))
    }

    fn required_records(
        &self,
        connections: &[SemanticRevisionConnection],
    ) -> SemanticRevisionResult<Vec<SemanticStoredRevision>> {
        let mut pending = self.head.records.clone();
        for connection in connections {
            pending.extend([
                SemanticRecordRef::element(&connection.relationship.source_element_id),
                SemanticRecordRef::element(&connection.relationship.target_element_id),
            ]);
        }
        let mut visited = BTreeSet::new();
        while let Some(reference) = pending.pop() {
            if !visited.insert(reference.clone()) {
                continue;
            }
            let stored = self.dependencies.require(&reference)?;
            pending.extend(
                self.dependencies
                    .requirements(stored, &self.hidden_associations),
            );
        }
        visited
            .difference(&self.selected)
            .map(|reference| self.dependencies.require(reference).cloned())
            .collect()
    }

    fn associations(
        &self,
        required: &[SemanticStoredRevision],
    ) -> Vec<SemanticRevisionAssociation> {
        let included = self
            .selected
            .iter()
            .chain(required.iter().map(|row| &row.record))
            .collect::<BTreeSet<_>>();
        self.state
            .associations
            .iter()
            .filter(|association| {
                !self
                    .hidden_associations
                    .contains(&association.association_id)
                    && included.contains(&SemanticRecordRef::artifact(&association.artifact_id))
            })
            .cloned()
            .collect()
    }

    fn connections(&self) -> SemanticRevisionResult<Vec<SemanticRevisionConnection>> {
        let definitions = self
            .state
            .connections
            .iter()
            .map(|connection| (connection.connection_id.as_str(), connection))
            .collect::<BTreeMap<_, _>>();
        self.head
            .connection_ids
            .iter()
            .map(|id| {
                definitions
                    .get(id.as_str())
                    .map(|connection| (*connection).clone())
                    .ok_or_else(|| {
                        SemanticRevisionError::invalid_value(
                            id,
                            "an exact selected connection definition",
                        )
                    })
            })
            .collect()
    }
}
