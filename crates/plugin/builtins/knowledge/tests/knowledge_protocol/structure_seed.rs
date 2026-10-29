//! Legacy fixtures remain unversioned until product publication adopts them.
use super::*;

impl MemoryStorageBroker {
    pub(super) fn add_source_after_publication(&self, element: SemanticElement) {
        let control = InvocationControl::sixty_seconds();
        let execute = |operation| {
            SemanticPersistence::execute(self.persistence.as_ref(), operation, &control).unwrap()
        };
        let root = element.project_root.clone();
        let SemanticResult::RevisionSelection(Some(mut head)) =
            execute(SemanticOperation::RevisionSelection {
                project_root: root.clone(),
                selection_id: "working".into(),
            })
        else {
            panic!("expected revision-managed fixture selection")
        };
        assert!(
            !head
                .elements
                .iter()
                .any(|stored| stored.semantic_element_id == element.semantic_element_id)
        );
        let SemanticResult::ProjectIdentity(identity) =
            execute(SemanticOperation::ProjectIdentity {
                project_root: root.clone(),
            })
        else {
            panic!("expected fixture lineage")
        };
        head.elements.push(element);
        let result = execute(SemanticOperation::PublishRevisions(
            lumvise_db_core::SemanticRevisionPublication {
                operation_id: format!("fixture-source:{}:{}", root, head.selection.generation),
                project_root: root,
                project_id: identity.project_id,
                selection_id: "working".into(),
                source_context: "local".into(),
                producer_base: None,
                expected: head.expectation,
                structure: Some(lumvise_db_core::SemanticRevisionStructure {
                    partition: None,
                    elements: head.elements,
                    relationships: head
                        .connections
                        .into_iter()
                        .map(|edge| edge.relationship)
                        .collect(),
                }),
                artifacts: vec![],
                removed_artifact_ids: vec![],
                source_anchor: None,
            },
        ));
        assert!(
            matches!(
                result,
                SemanticResult::RevisionPublication(
                    lumvise_db_core::SemanticRevisionPublicationOutcome::Accepted(_)
                )
            ),
            "{result:?}"
        );
    }

    fn active_project_structure(
        &self,
        project_root: &str,
    ) -> (
        Vec<SemanticElement>,
        Vec<lumvise_db_core::SemanticRelationship>,
    ) {
        let result = SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::ProjectSnapshot {
                scope: ProjectSnapshotScope::ProjectRoot(project_root.to_owned()),
                artifact_namespace: None,
            },
            &InvocationControl::sixty_seconds(),
        );
        match result {
            Ok(SemanticResult::ProjectSnapshot(snapshot)) => (
                snapshot
                    .elements
                    .into_iter()
                    .filter(|element| element.lifecycle != "inactive")
                    .collect(),
                snapshot.relationships,
            ),
            Ok(other) => panic!(
                "expected ProjectSnapshot result while loading active project structure, got {other:?}"
            ),
            Err(error) => {
                panic!("expected ProjectSnapshot operation to succeed while seeding: {error}")
            }
        }
    }

    pub(super) fn sync_elements(&self, project_root: &str, incoming: Vec<SemanticElement>) {
        let (mut elements, relationships) = self.active_project_structure(project_root);
        for element in incoming {
            if let Some(existing) = elements
                .iter_mut()
                .find(|existing| existing.semantic_element_id == element.semantic_element_id)
            {
                *existing = element;
            } else {
                elements.push(element);
            }
        }
        let result = SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::SyncStructure {
                project_root: project_root.to_owned(),
                elements,
                relationships,
            },
            &InvocationControl::sixty_seconds(),
        );
        match result {
            Ok(SemanticResult::SyncStructure(_)) => {}
            Ok(other) => panic!(
                "expected SyncStructure result while seeding semantic elements, got {other:?}"
            ),
            Err(error) => {
                panic!("expected SyncStructure operation to succeed while seeding: {error}")
            }
        }
    }
}
