//! Existing current records enter revision storage without ID or content rewriting.
use crate::connection_identities::connection_records;
use crate::revision_index::{
    digest, ensure_root, require_project_identity, stored_body, unique_nonempty,
};
use crate::*;
use std::collections::BTreeSet;

impl SemanticRevisionState {
    /// Adopts existing rows once, preserving IDs and giving unselected history a fresh grace period.
    /// Example: `SemanticRevisionState::adopt("/repo", "project-uuid", "working", "git:main", elements, artifacts, edges, 1, 0)?`.
    pub fn adopt(
        project_root: &str,
        project_id: &str,
        selection_id: &str,
        source_context: &str,
        elements: Vec<SemanticElement>,
        artifacts: Vec<SemanticArtifact>,
        relationships: Vec<SemanticRelationship>,
        commit_version: i64,
        now_ms: i64,
    ) -> SemanticRevisionResult<Self> {
        require_project_identity(project_id)?;
        unique_nonempty(
            &elements
                .iter()
                .map(|element| element.semantic_element_id.clone())
                .collect::<Vec<_>>(),
            "unique existing element identities",
        )?;
        unique_nonempty(
            &artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.clone())
                .collect::<Vec<_>>(),
            "unique existing artifact identities",
        )?;
        let mut state = Self {
            project_root: project_root.into(),
            project_id: project_id.into(),
            commit_version,
            ..Self::default()
        };
        let mut selected = vec![];
        for element in elements {
            element.validate()?;
            ensure_root(&element.project_root, project_root)?;
            let reference = SemanticRecordRef::element(&element.semantic_element_id);
            let active = element.lifecycle != "inactive";
            if active {
                selected.push(reference.clone());
            }
            state.records.push(stored_body(
                reference,
                SemanticRevisionBody::Element(element),
                active,
                now_ms,
            ));
        }
        for artifact in artifacts {
            artifact.validate()?;
            let reference = SemanticRecordRef::artifact(&artifact.artifact_id);
            state.associations.push(SemanticRevisionAssociation {
                association_id: digest(&(
                    &artifact.artifact_id,
                    &artifact.semantic_element_id,
                    source_context,
                ))?,
                artifact_id: artifact.artifact_id.clone(),
                semantic_element_id: artifact.semantic_element_id.clone(),
                source_context: source_context.into(),
                created_at_ms: now_ms,
                inherited: false,
            });
            selected.push(reference.clone());
            state.records.push(stored_body(
                reference,
                SemanticRevisionBody::Artifact(artifact),
                true,
                now_ms,
            ));
        }
        for edge in &relationships {
            edge.validate()?;
            ensure_root(&edge.project_root, project_root)?;
        }
        state.connections = connection_records(&relationships)?;
        let active = selected.iter().collect::<BTreeSet<_>>();
        let selected_connections = state
            .connections
            .iter()
            .filter(|edge| {
                active.contains(&SemanticRecordRef::element(
                    &edge.relationship.source_element_id,
                )) && active.contains(&SemanticRecordRef::element(
                    &edge.relationship.target_element_id,
                ))
            })
            .map(|edge| edge.connection_id.clone())
            .collect();
        state.selections.push(SemanticRevisionSelectionState {
            selection: SemanticRevisionSelection {
                project_root: project_root.into(),
                selection_id: selection_id.into(),
                source_context: source_context.into(),
                mode: SemanticSelectionMode::FollowAccepted,
                generation: 1,
                source_anchor: None,
            },
            records: selected,
            connection_ids: selected_connections,
        });
        crate::selection::validate_selected_records(
            &state,
            &state.selections[0].records,
            &state.selections[0].connection_ids,
        )?;
        state
            .selection_history
            .push(SemanticRevisionSelectionDelta::between(
                None,
                &state.selections[0],
                commit_version,
            ));
        state.normalize_order();
        crate::artifact_working::freeze_initial_dependencies(&mut state);
        Ok(state)
    }
}
