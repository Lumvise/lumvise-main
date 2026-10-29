use std::sync::Arc;

use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{
    LocalPersistence, RelationalPersistence, SemanticArtifact, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticResult,
};
use lumvise_plugin_knowledge::{
    APPLY_TRANSFER_EXPORT_ID, CREATE_EXPORT_ID, KnowledgePlugin, PLUGIN_ID,
    PREVIEW_TRANSFER_EXPORT_ID, UPDATE_EXPORT_ID,
};
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

struct TransferDependencyFakeHost {
    persistence: Arc<LocalPersistence>,
    broker: AppCoreHostCapabilityBroker,
    fail_final_artifact: Option<String>,
    pause_publication_once: bool,
    lose_acknowledgement_once: bool,
    candidate_queries: usize,
    legacy_writes: usize,
    writes: Vec<String>,
}

impl TransferDependencyFakeHost {
    fn new() -> Self {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let host = Self::sharing(persistence);
        for (root, id) in [("/source", "original"), ("/target", "target")] {
            host.execute(SemanticOperation::SyncStructure {
                project_root: root.into(),
                elements: vec![matching_element(root, id)],
                relationships: Vec::new(),
            });
        }
        host
    }

    fn sharing(persistence: Arc<LocalPersistence>) -> Self {
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        Self {
            persistence,
            broker: AppCoreHostCapabilityBroker::new(semantic, relational),
            fail_final_artifact: None,
            pause_publication_once: false,
            lose_acknowledgement_once: false,
            candidate_queries: 0,
            legacy_writes: 0,
            writes: Vec::new(),
        }
    }

    fn execute(&self, operation: SemanticOperation) -> SemanticResult {
        SemanticPersistence::execute(
            self.persistence.as_ref(),
            operation,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap()
    }

    fn invoke(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        KnowledgePlugin::default().dispatch(export, input, &mut PluginContext::for_test(self))
    }

    fn seed_dependencies(&mut self, cycle: bool) {
        self.publish_fixture_artifacts(
            "/source",
            vec![
                source_artifact("source-A", Some("source-B")),
                source_artifact("source-B", cycle.then_some("source-A")),
            ],
            "seed-source-dependencies",
        );
        self.writes.clear();
    }

    fn publish_fixture_artifacts(
        &self,
        root: &str,
        artifacts: Vec<SemanticArtifact>,
        operation: &str,
    ) {
        let root = root.to_owned();
        let SemanticResult::ProjectIdentity(identity) =
            self.execute(SemanticOperation::ProjectIdentity {
                project_root: root.clone(),
            })
        else {
            panic!("expected durable fixture lineage");
        };
        let SemanticResult::RevisionObservation(membership) =
            self.execute(SemanticOperation::RevisionSelectionMembers(
                lumvise_db_core::SemanticRevisionSelectionQuery {
                    project_root: root.clone(),
                    selection_id: "working".into(),
                    structure_coverage: lumvise_db_core::SemanticRevisionStructureCoverage::None,
                    artifact_ids: artifacts
                        .iter()
                        .map(|artifact| artifact.artifact_id.clone())
                        .collect(),
                },
            ))
        else {
            panic!("expected scoped fixture membership");
        };
        let result = self.execute(SemanticOperation::PublishRevisions(
            lumvise_db_core::SemanticRevisionPublication {
                operation_id: operation.into(),
                project_root: root,
                project_id: identity.project_id,
                selection_id: "working".into(),
                source_context: "local".into(),
                producer_base: None,
                expected: membership.map_or_else(Default::default, |head| head.expected),
                structure: None,
                artifacts: artifacts
                    .into_iter()
                    .map(|artifact| lumvise_db_core::SemanticArtifactRevisionWrite {
                        checkpoint: false,
                        artifact,
                        media_type: "text/markdown".into(),
                    })
                    .collect(),
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

    fn preview(&mut self) -> Value {
        self.invoke(
            PREVIEW_TRANSFER_EXPORT_ID,
            json!({"project_root": "/target"}),
        )
        .unwrap()
    }

    fn apply(&mut self, ids: &[String]) -> Result<Value, PluginError> {
        self.invoke(
            APPLY_TRANSFER_EXPORT_ID,
            json!({"project_root": "/target", "transfer_ids": ids}),
        )
    }

    fn apply_intent(&mut self, ids: &[String], operation: &str) -> Result<Value, PluginError> {
        self.invoke(
            APPLY_TRANSFER_EXPORT_ID,
            json!({"project_root":"/target", "transfer_ids":ids, "operation_id":operation}),
        )
    }

    fn artifact(&self, artifact_id: &str) -> SemanticArtifact {
        let result = self.execute(SemanticOperation::Artifact {
            artifact_id: artifact_id.into(),
        });
        let SemanticResult::Artifact(Some(artifact)) = result else {
            panic!("expected existing artifact `{artifact_id}`, got {result:?}");
        };
        artifact
    }

    fn history(&self) -> lumvise_db_core::SemanticRevisionState {
        let SemanticResult::RevisionHistory(history) =
            self.execute(SemanticOperation::RevisionHistory {
                project_root: "/target".into(),
            })
        else {
            panic!("expected durable destination revision state")
        };
        history
    }

    fn interrupt_publication(&mut self, input: &Value) -> Result<(), PluginError> {
        let selected = self
            .fail_final_artifact
            .as_deref()
            .is_some_and(|artifact_id| {
                input["publication"]["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|write| {
                write["artifact"]["metadata"]["knowledge"]["metadata"]["inheritance"]["transfer_id"]
                    .as_str()
                    == Some(artifact_id)
            })
            });
        if !self.pause_publication_once && !selected {
            return Ok(());
        }
        self.pause_publication_once = false;
        self.fail_final_artifact = None;
        Err(PluginError::new(
            "injected_final_write_failure",
            "selected publication interrupted once; expected identical frozen retry",
            true,
        ))
    }

    fn call_semantic_broker(&self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.broker
            .invoke(HostCapabilityRequest {
                plugin_id: PLUGIN_ID.into(),
                invocation_id: "transfer-regression".into(),
                call_id: "transfer-host-call".into(),
                capability_id: capability.into(),
                required_version: "^1.4.0".into(),
                input,
            })
            .map_err(|error| PluginError::new("host_call_failed", error.to_string(), false))
    }
}

impl HostCallTransport for TransferDependencyFakeHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        if input["operation"] == "candidate_source_elements" {
            self.candidate_queries += 1;
        }
        if input["operation"] == "upsert_artifact" {
            self.legacy_writes += 1;
        }
        let publication = input["operation"] == "publish_revisions";
        let ids = input["publication"]["artifacts"].as_array().map(|writes| {
            writes
                .iter()
                .map(|write| {
                    write["artifact"]["artifact_id"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect::<Vec<_>>()
        });
        if publication {
            self.interrupt_publication(&input)?;
        }
        let result = self.call_semantic_broker(capability, input)?;
        if publication && result["publication"]["outcome"] == "accepted" {
            self.writes.extend(ids.unwrap());
            if std::mem::take(&mut self.lose_acknowledgement_once) {
                return Err(PluginError::new(
                    "injected_lost_acknowledgement",
                    "accepted publication acknowledgement lost once; expected identical retry",
                    true,
                ));
            }
        }
        Ok(result)
    }
}

fn source_artifact(id: &str, dependency: Option<&str>) -> SemanticArtifact {
    serde_json::from_value(
        json!({"project_root":"/source", "artifact_id":id, "semantic_element_id":"original",
        "artifact_kind":"specification", "title":id, "content":"Source knowledge",
        "dependencies":dependency.map(artifact_dependency).into_iter().collect::<Vec<_>>(),
        "metadata":{"knowledge":{"project_root":"/source", "tags":[], "metadata":{},
            "lifecycle":{"status":"active", "revision":1}}}}),
    )
    .unwrap()
}

fn matching_element(project_root: &str, semantic_element_id: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: project_root.into(),
        semantic_element_id: semantic_element_id.into(),
        semantic_source_id: "transfer-fixture".into(),
        path: "src/lib.rs".into(),
        element_kind: "function".into(),
        name: "shared".into(),
        parent_element_id: None,
        content_fingerprint: Some("fp1:0000000000000001:same".into()),
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn artifact_dependency(artifact_id: &str) -> Value {
    json!({"target": {"target_kind": "artifact", "artifact_id": artifact_id}})
}

fn selected_ids(preview: &Value) -> Vec<String> {
    preview["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["transfer_id"].as_str().unwrap().to_owned())
        .collect()
}

fn copy_id(preview: &Value, source: &str) -> String {
    preview["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["source_artifact_id"] == source)
        .unwrap()["transfer_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn assert_dependency(host: &TransferDependencyFakeHost, artifact: &str, target: &str) {
    let stored = host.artifact(artifact);
    assert_eq!(
        serde_json::to_value(stored.dependencies).unwrap(),
        json!([artifact_dependency(target)])
    );
    assert_ne!(
        stored.metadata["knowledge"]["metadata"]["inheritance"]["transfer_pending"],
        true
    );
}

#[test]
fn selected_dependencies_publish_even_when_dependent_copy_id_sorts_first() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(false);
    let preview = host.preview();
    let copy_a = copy_id(&preview, "source-A");
    let copy_b = copy_id(&preview, "source-B");
    assert!(
        copy_a < copy_b,
        "fixture must exercise dependent-first order"
    );
    let result = host.apply(&selected_ids(&preview)).unwrap();
    assert_eq!(result["copied_artifact_ids"], json!([copy_a, copy_b]));
    assert_dependency(&host, &copy_a, &copy_b);
    assert_dependency(&host, "source-A", "source-B");
}

#[test]
fn reviewed_transfer_preserves_authored_content_in_a_revision_managed_destination() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(true);
    let authored = host
        .invoke(
            CREATE_EXPORT_ID,
            json!({"artifact_id":"authored-target-note",
        "project_root":"/target", "semantic_element_id":"target", "knowledge_type":"specification",
        "title":"Existing note", "content":"Preserve this authored destination content."}),
        )
        .unwrap();
    let before = host.artifact(authored["artifact"]["artifact_id"].as_str().unwrap());
    let preview = host.preview();
    let selected = selected_ids(&preview);
    let applied = host
        .apply(&selected)
        .expect("reviewed copies publish into the existing revision-managed project");
    assert_eq!(applied["copied_artifact_ids"].as_array().unwrap().len(), 2);
    assert_eq!(host.artifact(&before.artifact_id), before);
}

#[test]
fn selected_dependency_cycles_publish_with_destination_edges_and_intact_sources() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(true);
    let preview = host.preview();
    let copy_a = copy_id(&preview, "source-A");
    let copy_b = copy_id(&preview, "source-B");
    host.apply(&selected_ids(&preview)).unwrap();
    assert_dependency(&host, &copy_a, &copy_b);
    assert_dependency(&host, &copy_b, &copy_a);
    assert_dependency(&host, "source-A", "source-B");
    assert_dependency(&host, "source-B", "source-A");
}

#[test]
fn interrupted_dependency_publication_is_atomic_and_retries_the_frozen_selection() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(true);
    let preview = host.preview();
    let selected = selected_ids(&preview);
    let copy_a = copy_id(&preview, "source-A");
    let copy_b = copy_id(&preview, "source-B");
    host.fail_final_artifact = Some(copy_b.clone());
    assert_eq!(
        host.apply(&selected).unwrap_err().code,
        "injected_final_write_failure"
    );
    for id in [&copy_a, &copy_b] {
        assert!(matches!(
            host.execute(SemanticOperation::Artifact {
                artifact_id: id.clone()
            }),
            SemanticResult::Artifact(None)
        ));
    }
    let after_failure = host.preview();
    let candidates = after_failure["candidates"].as_array().unwrap();
    assert_eq!(
        candidates
            .iter()
            .find(|candidate| candidate["transfer_id"] == copy_a)
            .unwrap()["already_copied"],
        false
    );
    assert_eq!(
        candidates
            .iter()
            .find(|candidate| candidate["transfer_id"] == copy_b)
            .unwrap()["already_copied"],
        false
    );
    host.writes.clear();
    let retried = host.apply(&selected).unwrap();
    assert_eq!(retried["copied_artifact_ids"], json!([copy_a, copy_b]));
    assert_eq!(retried["already_copied_artifact_ids"], json!([]));
    assert_eq!(host.writes, selected);
    assert_eq!(host.legacy_writes, 0);
    assert_dependency(&host, &copy_a, &copy_b);
    assert_dependency(&host, &copy_b, &copy_a);
}

#[test]
fn concurrent_instances_cannot_duplicate_the_same_reviewed_copies() {
    let mut first = TransferDependencyFakeHost::new();
    first.seed_dependencies(true);
    let selected = selected_ids(&first.preview());
    first.pause_publication_once = true;
    assert_eq!(
        first
            .apply_intent(&selected, "first-transfer")
            .unwrap_err()
            .code,
        "injected_final_write_failure"
    );
    let mut second = TransferDependencyFakeHost::sharing(first.persistence.clone());
    let accepted = second.apply_intent(&selected, "second-transfer").unwrap();
    assert_eq!(accepted["copied_artifact_ids"].as_array().unwrap().len(), 2);
    let stale = first.apply_intent(&selected, "first-transfer").unwrap_err();
    assert_eq!(stale.code, "knowledge_revision_conflict");
    assert!(first.writes.is_empty());
    let reviewed = first.apply_intent(&selected, "fresh-review").unwrap();
    assert_eq!(reviewed["copied_artifact_ids"], json!([]));
    assert_eq!(
        reviewed["already_copied_artifact_ids"],
        accepted["copied_artifact_ids"]
    );
    assert_eq!(first.legacy_writes + second.legacy_writes, 0);
}

#[test]
fn lost_transfer_reply_replays_original_result_after_eligibility_changes_and_reordered_input() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(true);
    let selected = selected_ids(&host.preview());
    host.lose_acknowledgement_once = true;
    assert_eq!(
        host.apply_intent(&selected, "lost-transfer")
            .unwrap_err()
            .code,
        "injected_lost_acknowledgement"
    );
    host.invoke(
        UPDATE_EXPORT_ID,
        json!({"artifact_id":"source-A", "expected_revision":1,
        "content":"Source changed after transfer acceptance", "operation_id":"edit-source-A"}),
    )
    .unwrap();
    assert!(!selected_ids(&host.preview()).contains(&selected[0]));
    let accepted_state = host.history();
    let lookups = host.candidate_queries;
    let reordered = selected.iter().rev().cloned().collect::<Vec<_>>();
    let retried = host.apply_intent(&reordered, "lost-transfer").unwrap();
    assert_eq!(retried["copied_artifact_ids"], json!(selected));
    assert_eq!(
        host.apply_intent(&selected, "lost-transfer").unwrap(),
        retried
    );
    assert_eq!(
        host.history(),
        accepted_state,
        "storage receipt replay never republishes copied bodies"
    );
    assert_eq!(
        host.candidate_queries, lookups,
        "frozen retry never revalidates mutable eligibility"
    );
}

#[test]
fn later_dependency_copy_keeps_original_evidence_after_a_human_edits_its_copy() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(false);
    let preview = host.preview();
    let copy_a = copy_id(&preview, "source-A");
    let copy_b = copy_id(&preview, "source-B");
    host.apply_intent(std::slice::from_ref(&copy_b), "copy-evidence-first")
        .unwrap();
    let original = host.artifact(&copy_b);
    let edited = host
        .invoke(
            UPDATE_EXPORT_ID,
            json!({"artifact_id":copy_b,
        "expected_revision":1, "content":"Human interpretation, not the original evidence",
        "operation_id":"edit-copied-evidence"}),
        )
        .unwrap();
    let human_id = edited["artifact"]["artifact_id"].as_str().unwrap();
    assert_ne!(human_id, copy_b);
    host.apply_intent(std::slice::from_ref(&copy_a), "copy-dependent-later")
        .unwrap();
    assert_dependency(&host, &copy_a, &copy_b);
    assert_eq!(host.artifact(&copy_b), original);
    assert_eq!(
        host.artifact(human_id).content.as_deref(),
        Some("Human interpretation, not the original evidence")
    );
    assert_eq!(host.legacy_writes, 0);
}

#[test]
fn replacing_a_pending_copy_links_its_successor_and_owns_attachments_at_the_accepted_id() {
    let mut host = TransferDependencyFakeHost::new();
    let mut source = source_artifact("source-A", Some("source-B"));
    source.metadata["knowledge"]["metadata"]["files"] =
        json!({"image":{"contentRef":"canvas-file:source-A:image"}});
    host.publish_fixture_artifacts(
        "/source",
        vec![source, source_artifact("source-B", None)],
        "seed-attached-source",
    );
    host.execute(SemanticOperation::ArtifactBlobPut {
        content_ref: "canvas-file:source-A:image".into(),
        artifact_id: "source-A".into(),
        media_type: "image/png".into(),
        content: vec![7, 8, 9],
    });
    let preview = host.preview();
    let copy_a = copy_id(&preview, "source-A");
    let copy_b = copy_id(&preview, "source-B");
    let mut pending = source_artifact(&copy_a, None);
    pending.semantic_element_id = "target".into();
    pending.content = Some("Incomplete old copy".into());
    pending.metadata["knowledge"]["project_root"] = json!("/target");
    pending.metadata["knowledge"]["metadata"]["inheritance"] = json!({"transfer_id":copy_a,
        "source_artifact_id":"source-A", "transfer_pending":true});
    host.publish_fixture_artifacts("/target", vec![pending.clone()], "seed-pending-target");
    let accepted = host
        .apply_intent(&selected_ids(&preview), "complete-pending-target")
        .unwrap();
    let actual_a = accepted["copied_artifact_ids"][0].as_str().unwrap();
    assert_ne!(actual_a, copy_a);
    assert_dependency(&host, actual_a, &copy_b);
    assert_eq!(host.artifact(&copy_a).content, pending.content);
    let actual = host.artifact(actual_a);
    let reference = actual.metadata["knowledge"]["metadata"]["files"]["image"]["contentRef"]
        .as_str()
        .unwrap();
    let SemanticResult::ArtifactBlob(Some(blob)) =
        host.execute(SemanticOperation::ArtifactBlobGet {
            content_ref: reference.into(),
        })
    else {
        panic!("expected copied attachment")
    };
    assert_eq!(blob.artifact_id, actual_a);
    assert_eq!(blob.content, vec![7, 8, 9]);
    let SemanticResult::RevisionResolution(Some(previous)) =
        host.execute(SemanticOperation::ResolveRevision {
            project_root: "/target".into(),
            record: lumvise_db_core::SemanticRecordRef::artifact(&copy_a),
        })
    else {
        panic!("expected exact predecessor")
    };
    assert!(
        previous
            .successors
            .iter()
            .any(|link| link.newer.record_id == actual_a)
    );
}

#[test]
fn accepted_transfer_replays_after_collection_and_a_new_review_creates_successors() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(true);
    let selected = selected_ids(&host.preview());
    let accepted = host.apply_intent(&selected, "original-transfer").unwrap();
    for id in &selected {
        host.invoke(
            UPDATE_EXPORT_ID,
            json!({"artifact_id":id, "expected_revision":1,
            "content":"Revised copy before explicit withdrawal", "dependencies":[],
            "operation_id":format!("revise-before-withdrawal:{id}")}),
        )
        .unwrap();
    }
    let history = host.history();
    let current = history.selection("working").unwrap();
    host.execute(SemanticOperation::SelectRevisions(
        lumvise_db_core::SemanticRevisionSelectionUpdate {
            artifact_edits: Default::default(),
            operation_id: "withdraw-reviewed-copies".into(),
            project_root: "/target".into(),
            project_id: history.project_id.clone(),
            selection_id: "working".into(),
            source_context: "local".into(),
            expected_generation: current.selection.generation,
            mode: lumvise_db_core::SemanticSelectionMode::FollowAccepted,
            records: current
                .records
                .iter()
                .filter(|record| record.kind != lumvise_db_core::SemanticRecordKind::Artifact)
                .cloned()
                .collect(),
            connection_ids: current.connection_ids.clone(),
            source_anchor: None,
        },
    ));
    let policy = lumvise_db_core::SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) =
        host.execute(SemanticOperation::PlanRevisionRetention {
            project_root: "/target".into(),
            operation_id: "collect-withdrawn-copies".into(),
            policy: policy.clone(),
        })
    else {
        panic!("expected current-time retention plan")
    };
    assert!(selected.iter().all(|id| {
        plan.records
            .contains(&lumvise_db_core::SemanticRecordRef::artifact(id))
    }));
    host.execute(SemanticOperation::CollectRevisionRetention { plan, policy });
    for id in &selected {
        let SemanticResult::RevisionResolution(Some(revision)) =
            host.execute(SemanticOperation::ResolveRevision {
                project_root: "/target".into(),
                record: lumvise_db_core::SemanticRecordRef::artifact(id),
            })
        else {
            panic!("expected retained header")
        };
        assert_eq!(revision.body, lumvise_db_core::SemanticRevisionBody::Pruned);
    }
    let collected = host.history();
    assert_eq!(
        host.apply_intent(&selected, "original-transfer").unwrap(),
        accepted
    );
    assert_eq!(
        host.history(),
        collected,
        "replay cannot resurrect collected bodies"
    );
    let copied_again = host
        .apply_intent(&selected, "review-after-collection")
        .unwrap();
    for (index, old) in selected.iter().enumerate() {
        let actual = copied_again["copied_artifact_ids"][index].as_str().unwrap();
        assert_ne!(actual, old);
        assert_eq!(
            host.artifact(actual).metadata["knowledge"]["metadata"]["inheritance"]["copied_revision_id"],
            actual
        );
    }
}

#[test]
fn generic_destination_collision_fails_before_any_copy_write_and_preserves_existing_artifact() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(false);
    let preview = host.preview();
    let copy_a = copy_id(&preview, "source-A");
    let collision = SemanticArtifact {
        artifact_id: copy_a.clone(),
        semantic_element_id: "original".into(),
        artifact_kind: "generic".into(),
        title: "Must survive".into(),
        content_ref: None,
        content: Some("Original content".into()),
        searchable_text: None,
        content_size_bytes: None,
        dependencies: Vec::new(),
        metadata: json!({"original": true}),
    };
    host.publish_fixture_artifacts("/source", vec![collision], "seed-destination-collision");
    let before = host.artifact(&copy_a);
    assert_eq!(
        host.apply(&selected_ids(&preview)).unwrap_err().code,
        "transfer_destination_conflict"
    );
    assert!(host.writes.is_empty());
    assert_eq!(host.artifact(&copy_a), before);
}
