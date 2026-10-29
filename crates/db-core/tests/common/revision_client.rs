//! Shared public-boundary fixture: observe the lineage before the selected membership token.
use lumvise_db_core::{
    SemanticElement, SemanticOperation, SemanticPersistence, SemanticResult,
    SemanticRevisionPublication, SemanticRevisionStructure, SemanticSelectionExpectation,
};
use lumvise_resource_routing::InvocationControl;

pub struct RevisionTestClient<'a> {
    persistence: &'a dyn SemanticPersistence,
    root: &'a str,
    context: &'a str,
}

impl<'a> RevisionTestClient<'a> {
    /// Binds an existing public persistence. Example: `RevisionTestClient::new(&local, "/repo", "local")`.
    pub fn new(persistence: &'a dyn SemanticPersistence, root: &'a str, context: &'a str) -> Self {
        Self {
            persistence,
            root,
            context,
        }
    }

    /// Captures the observed identity and selection. Example: `client.publication("edit", elements)`.
    pub fn publication(
        &self,
        operation_id: &str,
        elements: Vec<SemanticElement>,
    ) -> SemanticRevisionPublication {
        let project_id = self.project_id();
        let expected = self.expectation();
        SemanticRevisionPublication {
            operation_id: operation_id.into(),
            producer_base: None,
            project_root: self.root.into(),
            project_id,
            selection_id: "working".into(),
            source_context: self.context.into(),
            expected,
            structure: Some(SemanticRevisionStructure {
                partition: None,
                elements,
                relationships: vec![],
            }),
            artifacts: vec![],
            removed_artifact_ids: vec![],
            source_anchor: None,
        }
    }

    /// Reads the stable lineage. Example: `client.project_id()` before an edit begins.
    pub fn project_id(&self) -> String {
        match self.execute(SemanticOperation::ProjectIdentity {
            project_root: self.root.into(),
        }) {
            SemanticResult::ProjectIdentity(identity) => identity.project_id,
            other => panic!("{other:?}; expected the durable project lineage"),
        }
    }

    fn expectation(&self) -> SemanticSelectionExpectation {
        match self.execute(SemanticOperation::RevisionSelection {
            project_root: self.root.into(),
            selection_id: "working".into(),
        }) {
            SemanticResult::RevisionSelection(head) => head
                .map_or_else(SemanticSelectionExpectation::default, |head| {
                    head.expectation
                }),
            other => panic!("{other:?}; expected the observed working selection"),
        }
    }

    fn execute(&self, operation: SemanticOperation) -> SemanticResult {
        self.persistence
            .execute(operation, &InvocationControl::sixty_seconds())
            .unwrap()
    }
}
