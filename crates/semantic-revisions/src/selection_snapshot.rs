//! Owns consistent selection projections and exact evidence closure. Adapters call
//! `snapshot` or `archive_projection`; required records never become primary members or grants.
use crate::required_revisions::RevisionDependencyIndex;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

impl SemanticRevisionState {
    /// Projects only available archive bodies while metadata retains acknowledged loss and exact membership.
    /// Example: `state.archive_projection("working")?` alongside `state.archive_metadata_rows()`.
    pub fn archive_projection(
        &self,
        selection_id: &str,
    ) -> SemanticRevisionResult<Option<SemanticRevisionArchiveProjection>> {
        self.validate_archive_state()?;
        self.selection(selection_id)
            .map(|head| SelectionReadProjection::new(self, head).archive_projection())
            .transpose()
    }
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
        require_published_reference(&record.record, false)?;
    }
    Ok(())
}

pub(crate) fn require_published_reference(
    reference: &SemanticRecordRef,
    selected: bool,
) -> SemanticRevisionResult<()> {
    if !selected {
        return Err(SemanticRevisionError::invalid_value(
            &reference.record_id,
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
        let (elements, artifacts) = self.current_bodies(false)?;
        let connections = self.connections()?;
        let required_records = self.required_records(&connections, false)?;
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
        preserve_unavailable: bool,
    ) -> SemanticRevisionResult<(Vec<SemanticElement>, Vec<SemanticArtifact>)> {
        let (mut elements, mut artifacts) = (vec![], vec![]);
        for record in &self.head.records {
            let stored = if preserve_unavailable {
                self.dependencies.require_retained(record)?
            } else {
                self.dependencies.require(record)?
            };
            match &stored.body {
                SemanticRevisionBody::Element(element) => elements.push(element.clone()),
                SemanticRevisionBody::Artifact(artifact) => artifacts.push(artifact.clone()),
                SemanticRevisionBody::Unavailable(_) if preserve_unavailable => {}
                SemanticRevisionBody::Pruned | SemanticRevisionBody::Unavailable(_) => {
                    unreachable!()
                }
            }
        }
        Ok((elements, artifacts))
    }

    fn archive_projection(&self) -> SemanticRevisionResult<SemanticRevisionArchiveProjection> {
        let (elements, artifacts) = self.current_bodies(true)?;
        let mut connections = vec![];
        for edge in self.connections()? {
            let source = self
                .dependencies
                .require_retained(&SemanticRecordRef::element(
                    &edge.relationship.source_element_id,
                ))?;
            let target = self
                .dependencies
                .require_retained(&SemanticRecordRef::element(
                    &edge.relationship.target_element_id,
                ))?;
            if source.body.is_available() && target.body.is_available() {
                connections.push(edge);
            }
        }
        Ok(SemanticRevisionArchiveProjection {
            elements,
            artifacts,
            required_records: self.required_records(&connections, true)?,
            connections,
        })
    }

    fn required_records(
        &self,
        connections: &[SemanticRevisionConnection],
        preserve_unavailable: bool,
    ) -> SemanticRevisionResult<Vec<SemanticStoredRevision>> {
        let visited = self.required_references(connections, preserve_unavailable)?;
        let mut records = visited
            .difference(&self.selected)
            .map(|reference| {
                self.dependency_record(reference, preserve_unavailable)
                    .cloned()
            })
            .collect::<SemanticRevisionResult<Vec<_>>>()?;
        records.retain(|record| record.body.is_available());
        Ok(records)
    }

    fn required_references(
        &self,
        connections: &[SemanticRevisionConnection],
        preserve_unavailable: bool,
    ) -> SemanticRevisionResult<BTreeSet<SemanticRecordRef>> {
        let mut pending = self.pending_dependencies(connections);
        let mut visited = BTreeSet::new();
        while let Some(reference) = pending.pop() {
            if !visited.insert(reference.clone()) {
                continue;
            }
            let stored = self.dependency_record(&reference, preserve_unavailable)?;
            pending.extend(
                self.dependencies
                    .requirements(stored, &self.hidden_associations),
            );
        }
        Ok(visited)
    }

    fn pending_dependencies(
        &self,
        connections: &[SemanticRevisionConnection],
    ) -> Vec<SemanticRecordRef> {
        let mut pending = self.head.records.clone();
        pending.extend(connections.iter().flat_map(|connection| {
            [
                SemanticRecordRef::element(&connection.relationship.source_element_id),
                SemanticRecordRef::element(&connection.relationship.target_element_id),
            ]
        }));
        pending
    }

    fn dependency_record(
        &self,
        reference: &SemanticRecordRef,
        preserve_unavailable: bool,
    ) -> SemanticRevisionResult<&'a SemanticStoredRevision> {
        if preserve_unavailable {
            return self.dependencies.require_retained(reference);
        }
        self.dependencies.require(reference)
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
