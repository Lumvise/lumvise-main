//! Public persistence observations for signed source-ingestion fixtures.
use super::*;
use lumvise_db_core::{
    SemanticPartition, SemanticRevisionSelectionQuery, SemanticRevisionStructureCoverage,
    SemanticSelectionExpectation,
};

impl MemoryCapabilityBroker {
    pub fn source_name(&self, root: &str, source: &str) -> String {
        self.plugin_rows("builtin.semantic", "semantic_sources")
            .iter()
            .find(|record| record["project_root"] == root && record["semantic_source_id"] == source)
            .unwrap()["name"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    pub fn revision_history(&self, root: &str) -> lumvise_db_core::SemanticRevisionState {
        let SemanticResult::RevisionHistory(history) =
            self.observed_semantic(SemanticOperation::RevisionHistory {
                project_root: root.into(),
            })
        else {
            panic!("expected durable revision history")
        };
        history
    }

    /// Builds an observation only for test input intentionally authored against today's store.
    /// Conflict/retry fixtures keep their original observation and must not call this method.
    pub fn observe_fixture(&self, mut batch: serde_json::Value) -> serde_json::Value {
        assert!(
            batch["revision_observation"].is_null(),
            "expected a fresh fixture, never a frozen request"
        );
        let root = batch["project_root"].as_str().unwrap();
        let provider = batch["provider_instance_id"].as_str().unwrap();
        let paths = ["replace_paths", "removed_paths"]
            .into_iter()
            .flat_map(|key| batch[key].as_array().into_iter().flatten())
            .map(|path| path.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let artifacts = batch["semantic_artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|artifact| artifact["artifact_id"].as_str().unwrap().to_owned())
            .chain(
                batch["removed_artifact_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|id| id.as_str().unwrap().to_owned()),
            )
            .collect::<Vec<_>>();
        let mut observation = self.revision_observation(root, provider);
        let query = SemanticRevisionSelectionQuery {
            project_root: root.into(),
            selection_id: "working".into(),
            structure_coverage: SemanticRevisionStructureCoverage::Partition(SemanticPartition {
                project_root: root.into(),
                replace_paths: paths,
                provider_instance_id: Some(provider.into()),
            }),
            artifact_ids: artifacts,
        };
        let SemanticResult::RevisionObservation(members) =
            self.observed_semantic(SemanticOperation::RevisionSelectionMembers(query))
        else {
            panic!("expected observed fixture membership")
        };
        observation["expected"] =
            serde_json::json!(members.map_or_else(Default::default, |head| head.expected));
        batch["revision_observation"] = observation;
        batch
    }

    pub fn projected_records(&self, root: &str) -> serde_json::Value {
        let SemanticResult::ProjectSnapshot(snapshot) =
            self.observed_semantic(SemanticOperation::ProjectSnapshot {
                scope: ProjectSnapshotScope::ProjectRoot(root.into()),
                artifact_namespace: None,
            })
        else {
            panic!("expected current records")
        };
        serde_json::json!({"elements":snapshot.elements, "relationships":snapshot.relationships, "artifacts":snapshot.artifacts})
    }

    pub fn selected_revision_snapshot(
        &self,
        root: &str,
    ) -> lumvise_db_core::SemanticRevisionSelectionSnapshot {
        match self.observed_semantic(SemanticOperation::RevisionSelection {
            project_root: root.into(),
            selection_id: "working".into(),
        }) {
            SemanticResult::RevisionSelection(Some(selected)) => selected,
            other => panic!("{other:?}; expected a selected revision snapshot"),
        }
    }

    pub fn revision_blob(&self, reference: &str) -> Option<lumvise_db_core::ArtifactBlob> {
        match self.observed_semantic(SemanticOperation::ArtifactBlobGet {
            content_ref: reference.into(),
        }) {
            SemanticResult::ArtifactBlob(blob) => blob,
            other => panic!("{other:?}; expected exact blob bytes"),
        }
    }

    pub fn revision_observation(&self, root: &str, provider: &str) -> serde_json::Value {
        let SemanticResult::ProjectIdentity(identity) =
            self.observed_semantic(SemanticOperation::ProjectIdentity {
                project_root: root.into(),
            })
        else {
            panic!("expected observed lineage");
        };
        serde_json::json!(lumvise_contracts::SemanticIndexObservation {
            project_id: identity.project_id,
            producer_base: None,
            selection_id: "working".into(),
            source_context: "local".into(),
            expected: self.observed_provider_members(root, provider),
        })
    }

    fn observed_provider_members(
        &self,
        root: &str,
        provider: &str,
    ) -> SemanticSelectionExpectation {
        let partition = SemanticPartition {
            project_root: root.into(),
            replace_paths: vec![],
            provider_instance_id: Some(provider.into()),
        };
        let query = SemanticRevisionSelectionQuery {
            project_root: root.into(),
            selection_id: "working".into(),
            structure_coverage: SemanticRevisionStructureCoverage::Partition(partition),
            artifact_ids: vec![],
        };
        match self.observed_semantic(SemanticOperation::RevisionSelectionMembers(query)) {
            SemanticResult::RevisionObservation(head) => {
                head.map_or_else(Default::default, |head| head.expected)
            }
            other => panic!("{other:?}; expected observed provider membership"),
        }
    }

    fn observed_semantic(&self, operation: SemanticOperation) -> SemanticResult {
        SemanticPersistence::execute(
            self.persistence.as_ref(),
            operation,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap()
    }
}
