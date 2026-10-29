use super::*;
use crate::plugin::knowledge_authority::semantic_access::KnowledgeSemanticAccess;
use lumvise_db_core::{
    SemanticArtifactRevisionWrite, SemanticRecordRef, SemanticRevisionBody,
    SemanticRevisionPublication, SemanticRevisionPublicationOutcome, SemanticRevisionStructure,
    VectorSearchResult,
};
use std::collections::BTreeMap;

const ROOT: &str = "agent-space://scout";

impl AgentAuthorityFixture {
    fn raw(&self, operation: SemanticOperation) -> SemanticResult {
        SemanticPersistence::execute(
            self.store.as_ref(),
            operation,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap()
    }

    fn read(&self, operation: SemanticOperation) -> lumvise_db_core::Result<SemanticResult> {
        KnowledgeSemanticAccess {
            semantic: self.store.as_ref(),
            authority: &self.authority,
            context: &self.context,
            control: &InvocationControl::sixty_seconds(),
        }
        .execute(operation)
    }

    fn seed_documents(&self) -> BTreeMap<String, String> {
        self.bind("scout", None).unwrap();
        let scope = self.current().unwrap();
        let access = json!({"space_id":scope.space.unwrap().space_id,"required_projects":[{
            "project_root":"/removed","project_id":"removed-lineage","storage_authority":scope.storage_authority}]});
        let element = |id, metadata| {
            serde_json::from_value(json!({"project_root":ROOT,"semantic_element_id":id,
            "semantic_source_id":"fixture","path":id,"element_kind":"virtual_document","name":id,
            "lifecycle":"active","metadata":metadata}))
            .unwrap()
        };
        let SemanticResult::ProjectIdentity(identity) =
            self.raw(SemanticOperation::ProjectIdentity {
                project_root: ROOT.into(),
            })
        else {
            panic!("project identity")
        };
        let artifact = serde_json::from_value(json!({
            "artifact_id":"secret-body","semantic_element_id":"secret","artifact_kind":"knowledge_document",
            "title":"Private title","content":"Private body","metadata":{"knowledge_document":{"access":access}}})).unwrap();
        let result = self.raw(SemanticOperation::PublishRevisions(
            SemanticRevisionPublication {
                operation_id: "seed-documents".into(),
                project_root: ROOT.into(),
                project_id: identity.project_id,
                selection_id: "working".into(),
                source_context: "local".into(),
                producer_base: None,
                expected: Default::default(),
                structure: Some(SemanticRevisionStructure {
                    partition: None,
                    elements: vec![
                        element("independent", json!({})),
                        element(
                            "secret",
                            json!({"indexer_metadata":{"knowledge_document":{"access":access}}}),
                        ),
                    ],
                    relationships: vec![],
                }),
                artifacts: vec![SemanticArtifactRevisionWrite {
                    artifact,
                    media_type: "text/plain".into(),
                    checkpoint: false,
                }],
                removed_artifact_ids: vec![],
                source_anchor: None,
            },
        ));
        let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) = result
        else {
            panic!("accepted fixture publication")
        };
        receipt
            .remaps
            .into_iter()
            .map(|remap| (remap.incoming.record_id, remap.accepted.record_id))
            .collect()
    }
}

#[test]
fn revoked_document_titles_bodies_and_exact_history_are_hidden_from_owning_agent() {
    let fixture = AgentAuthorityFixture::new();
    let ids = fixture.seed_documents();
    assert!(matches!(
        fixture
            .read(SemanticOperation::Element {
                semantic_element_id: ids["secret"].clone()
            })
            .unwrap(),
        SemanticResult::Element(None)
    ));
    assert!(matches!(
        fixture
            .read(SemanticOperation::Element {
                semantic_element_id: ids["independent"].clone()
            })
            .unwrap(),
        SemanticResult::Element(Some(_))
    ));
    assert!(matches!(
        fixture
            .read(SemanticOperation::Artifact {
                artifact_id: ids["secret-body"].clone()
            })
            .unwrap(),
        SemanticResult::Artifact(None)
    ));
    for record in [
        SemanticRecordRef::element(&ids["secret"]),
        SemanticRecordRef::artifact(&ids["secret-body"]),
    ] {
        let operation = SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record,
        };
        let SemanticResult::RevisionResolution(Some(exact)) = fixture.raw(operation.clone()) else {
            panic!("stored exact body")
        };
        assert!(!matches!(exact.body, SemanticRevisionBody::Pruned));
        assert!(matches!(
            fixture.read(operation).unwrap(),
            SemanticResult::RevisionResolution(None)
        ));
    }
    assert!(
        fixture
            .read(SemanticOperation::RemoveElement {
                semantic_element_id: ids["secret"].clone()
            })
            .is_err()
    );
}

struct RankedDocuments {
    store: Arc<LocalPersistence>,
    ids: BTreeMap<String, String>,
}

impl SemanticPersistence for RankedDocuments {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::Result<SemanticResult> {
        match operation {
            SemanticOperation::SearchElementNameVectors { .. } => {
                Ok(SemanticResult::ElementVectorSearch(vec![
                    VectorSearchResult {
                        id: self.ids["secret"].clone(),
                        score: 1.0,
                    },
                    VectorSearchResult {
                        id: self.ids["independent"].clone(),
                        score: 0.8,
                    },
                ]))
            }
            SemanticOperation::SearchArtifactTextVectors { .. } => {
                Ok(SemanticResult::ArtifactVectorSearch(vec![
                    VectorSearchResult {
                        id: self.ids["secret-body"].clone(),
                        score: 1.0,
                    },
                ]))
            }
            operation => SemanticPersistence::execute(self.store.as_ref(), operation, control),
        }
    }
    fn readiness(&self) -> lumvise_db_core::Result<lumvise_db_core::SemanticReadiness> {
        SemanticPersistence::readiness(self.store.as_ref())
    }
}

#[test]
fn vector_ranking_cannot_disclose_revoked_document_ids_or_scores() {
    let fixture = AgentAuthorityFixture::new();
    let ids = fixture.seed_documents();
    let ranked = RankedDocuments {
        store: fixture.store.clone(),
        ids,
    };
    let control = InvocationControl::sixty_seconds();
    let access = KnowledgeSemanticAccess {
        semantic: &ranked,
        authority: &fixture.authority,
        context: &fixture.context,
        control: &control,
    };
    let elements = access
        .execute(SemanticOperation::SearchElementNameVectors {
            project_root: ROOT.into(),
            query: vec![],
            k: 10,
            engine_id: "fixture".into(),
            model: None,
        })
        .unwrap();
    assert!(
        matches!(elements, SemanticResult::ElementVectorSearch(hits) if hits.len() == 1 && hits[0].id == ranked.ids["independent"])
    );
    let artifacts = access
        .execute(SemanticOperation::SearchArtifactTextVectors {
            project_root: ROOT.into(),
            query: vec![],
            k: 10,
            engine_id: "fixture".into(),
            model: None,
        })
        .unwrap();
    assert!(matches!(artifacts, SemanticResult::ArtifactVectorSearch(hits) if hits.is_empty()));
}

#[test]
fn private_annotations_cannot_omit_newly_supplied_source_context() {
    let mut fixture = AgentAuthorityFixture::new();
    fixture.bind("scout", None).unwrap();
    let source = json!({"project_root":"/source","semantic_element_id":"source-file","semantic_source_id":"source",
        "path":"file.rs","element_kind":"file","name":"Source","lifecycle":"active","metadata":{}});
    fixture.raw(SemanticOperation::SyncStructure {
        project_root: "/source".into(),
        elements: vec![serde_json::from_value(source.clone()).unwrap()],
        relationships: vec![],
    });
    let mut independent = source.clone();
    independent["project_root"] = json!(ROOT);
    independent["semantic_element_id"] = json!("independent");
    fixture.raw(SemanticOperation::SyncStructure {
        project_root: ROOT.into(),
        elements: vec![serde_json::from_value(independent).unwrap()],
        relationships: vec![],
    });
    fixture.authority.invoke(AGENTS, json!({"operation":"bind_agent","binding":{"agent_id":"scout",
        "grants":[{"project_root":"/source","writable":false}], "observations":[{"table":"definitions","row_key":"scout","expected":null}]}}),
        fixture.store.as_ref(), &fixture.context).unwrap();
    fixture
        .read(SemanticOperation::Element {
            semantic_element_id: "source-file".into(),
        })
        .unwrap();
    let annotation = |owner: &str| {
        serde_json::from_value(json!({"artifact_id":"annotation","semantic_element_id":owner,
        "artifact_kind":"specification","title":"Derived title","content":"Derived body","metadata":{}})).unwrap()
    };
    assert!(
        fixture
            .read(SemanticOperation::UpsertArtifact {
                artifact: annotation("independent"),
                media_type: "text/plain".into()
            })
            .unwrap_err()
            .to_string()
            .contains("owning Knowledge document")
    );
    let SemanticResult::ProjectIdentity(identity) =
        fixture.raw(SemanticOperation::ProjectIdentity {
            project_root: ROOT.into(),
        })
    else {
        panic!("project identity")
    };
    let publication = SemanticRevisionPublication {
        operation_id: "annotation-escape".into(),
        project_root: ROOT.into(),
        project_id: identity.project_id,
        selection_id: "working".into(),
        source_context: "local".into(),
        producer_base: None,
        expected: Default::default(),
        structure: None,
        artifacts: vec![SemanticArtifactRevisionWrite {
            artifact: annotation("independent"),
            media_type: "text/plain".into(),
            checkpoint: false,
        }],
        removed_artifact_ids: vec![],
        source_anchor: None,
    };
    assert!(
        fixture
            .read(SemanticOperation::PublishRevisions(publication))
            .unwrap_err()
            .to_string()
            .contains("owning Knowledge document")
    );
    let scope = fixture.current().unwrap();
    let mut protected = source;
    protected["project_root"] = json!(ROOT);
    protected["semantic_element_id"] = json!("protected");
    protected["metadata"] = json!({"indexer_metadata":{"knowledge_document":{"access":{
        "space_id":scope.space.unwrap().space_id,"required_projects":scope.source_projects}}}});
    fixture.raw(SemanticOperation::SyncStructure {
        project_root: ROOT.into(),
        elements: vec![serde_json::from_value(protected).unwrap()],
        relationships: vec![],
    });
    fixture
        .read(SemanticOperation::UpsertArtifact {
            artifact: annotation("protected"),
            media_type: "text/plain".into(),
        })
        .unwrap();
    fixture.context = fixture.context.clone().enter_plugin(AGENTS, "agent.read");
    fixture.bind("scout", None).unwrap();
    assert!(matches!(
        fixture
            .read(SemanticOperation::Artifact {
                artifact_id: "annotation".into()
            })
            .unwrap(),
        SemanticResult::Artifact(None)
    ));
}
