//! Named project fixture shared by public revision-policy conformance tests.
#![allow(dead_code)]
use lumvise_semantic_revisions::*;
use serde_json::json;

pub(crate) const ROOT: &str = "/cooperation";
pub(crate) const PROJECT_ID: &str = "cooperating-project";

pub(crate) fn element(id: &str, path: &str, body: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: ROOT.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: path.into(),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(3),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({ "body": body }),
    }
}

pub(crate) fn artifact(id: &str, owner: &str, title: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: id.into(),
        semantic_element_id: owner.into(),
        artifact_kind: "annotation".into(),
        title: title.into(),
        content_ref: None,
        content: Some(title.into()),
        searchable_text: None,
        content_size_bytes: None,
        dependencies: vec![],
        metadata: json!({}),
    }
}

pub(crate) fn timestamp(ms: i64) -> SemanticRevisionTimestamp {
    SemanticRevisionTimestamp {
        unix_ms: ms,
        rfc3339: "2026-10-24T00:00:00Z".into(),
    }
}

pub(crate) struct ProjectFixture {
    pub(crate) state: SemanticRevisionState,
    pub(crate) now: i64,
}

impl ProjectFixture {
    pub(crate) fn new() -> Self {
        Self {
            state: SemanticRevisionState {
                project_root: ROOT.into(),
                project_id: PROJECT_ID.into(),
                ..Default::default()
            },
            now: 1000,
        }
    }

    pub(crate) fn edit(
        &self,
        id: &str,
        elements: Vec<SemanticElement>,
    ) -> SemanticRevisionPublication {
        SemanticRevisionPublication {
            operation_id: id.into(),
            producer_base: None,
            project_root: ROOT.into(),
            project_id: self.state.project_id.clone(),
            selection_id: "working".into(),
            source_context: "git:main".into(),
            expected: self
                .state
                .selection("working")
                .map_or_else(SemanticSelectionExpectation::default, |head| {
                    head.expectation(&self.state.records)
                }),
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

    pub(crate) fn publish(
        &mut self,
        request: &SemanticRevisionPublication,
    ) -> SemanticRevisionPublicationReceipt {
        let plan = self.plan(request);
        self.now += 1000;
        match self.state.apply_publication(plan).unwrap() {
            SemanticRevisionPublicationOutcome::Accepted(receipt) => receipt,
            other => panic!("{other:?}; expected accepted publication"),
        }
    }

    pub(crate) fn plan(
        &self,
        request: &SemanticRevisionPublication,
    ) -> SemanticRevisionPublicationPlan {
        self.state
            .plan_publication(request, &timestamp(self.now), self.state.commit_version + 1)
            .unwrap()
    }

    pub(crate) fn selected(&self) -> SemanticRevisionSelectionSnapshot {
        self.state
            .snapshot("working", "2026-10-24T00:00:00Z")
            .unwrap()
            .unwrap()
    }

    pub(crate) fn initial(&mut self) {
        let request = self.edit(
            "initial",
            vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
        );
        self.publish(&request);
    }

    pub(crate) fn collect(&mut self) -> SemanticRevisionRetentionReceipt {
        let policy = SemanticRevisionRetentionPolicy::Age {
            history_age_seconds: 1,
        };
        let plan = self
            .state
            .plan_retention("collect", &policy, 20_000)
            .unwrap();
        self.state
            .apply_retention(&plan, &policy, 20_000, self.state.commit_version + 1)
            .unwrap()
    }
}

pub(crate) fn accepted_id(receipt: &SemanticRevisionPublicationReceipt, id: &str) -> String {
    receipt
        .remaps
        .iter()
        .find(|remap| remap.incoming.record_id == id)
        .unwrap()
        .accepted
        .record_id
        .clone()
}

pub(crate) fn path_edit(request: &mut SemanticRevisionPublication, path: &str) {
    request.structure.as_mut().unwrap().partition = Some(SemanticPartition {
        project_root: ROOT.into(),
        replace_paths: vec![path.into()],
        provider_instance_id: None,
    });
}
