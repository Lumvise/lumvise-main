//! Knowledge edits through the real host broker and native persistence.
use std::sync::Arc;

use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{
    LocalPersistence, RelationalPersistence, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticResult,
};
use lumvise_plugin_knowledge::KnowledgePlugin;
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

#[path = "artifact_revision_publication/archives.rs"]
mod archives;
#[path = "artifact_revision_publication/cooperation.rs"]
mod cooperation;
#[path = "artifact_revision_publication/write_receipts.rs"]
mod write_receipts;

struct ArtifactBoundaryHost {
    persistence: Arc<LocalPersistence>,
    broker: AppCoreHostCapabilityBroker,
    publication_inputs: Vec<Value>,
    drop_publication_reply: bool,
    interleave_publication: bool,
    interleave_observation: bool,
    reported_project_identity: Option<String>,
}

impl ArtifactBoundaryHost {
    fn new() -> Self {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let host = Self::with_persistence(persistence);
        host.seed_owner();
        host
    }

    fn with_persistence(persistence: Arc<LocalPersistence>) -> Self {
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        Self {
            persistence,
            broker: AppCoreHostCapabilityBroker::new(semantic, relational),
            publication_inputs: vec![],
            drop_publication_reply: false,
            interleave_publication: false,
            interleave_observation: false,
            reported_project_identity: None,
        }
    }

    fn seed_owner(&self) {
        self.execute(SemanticOperation::SyncStructure {
            project_root: "/artifact-project".into(),
            relationships: vec![],
            elements: vec![SemanticElement {
                revision_dependencies: Vec::new(),
                project_root: "/artifact-project".into(),
                semantic_element_id: "owner".into(),
                semantic_source_id: "source".into(),
                path: "source.rs".into(),
                element_kind: "file".into(),
                name: "Source".into(),
                parent_element_id: None,
                content_fingerprint: None,
                start_line: None,
                end_line: None,
                lifecycle: "active".into(),
                match_evidence: None,
                metadata: json!({}),
            }],
        });
    }

    fn execute(&self, operation: SemanticOperation) -> SemanticResult {
        SemanticPersistence::execute(
            self.persistence.as_ref(),
            operation,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap()
    }

    fn knowledge(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        KnowledgePlugin::default().dispatch(export, input, &mut PluginContext::for_test(self))
    }

    fn invoke_host(&self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.broker
            .invoke(HostCapabilityRequest {
                plugin_id: "builtin.knowledge".into(),
                invocation_id: "artifact-boundary".into(),
                call_id: "artifact-host-call".into(),
                capability_id: capability.into(),
                required_version: "^1.0".into(),
                input,
            })
            .map_err(|error| PluginError::new("artifact_test_host_error", error.to_string(), false))
    }

    fn exact_artifact(&self, id: &str) -> Value {
        let SemanticResult::Artifact(Some(artifact)) = self.execute(SemanticOperation::Artifact {
            artifact_id: id.into(),
        }) else {
            panic!("expected exact artifact `{id}`")
        };
        json!(artifact)
    }
}

impl HostCallTransport for ArtifactBoundaryHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        if capability == "storage.semantic"
            && input["operation"] == "revision_selection_members"
            && std::mem::take(&mut self.interleave_observation)
        {
            self.knowledge("update_knowledge", json!({"artifact_id":"spec", "project_root":"/artifact-project",
                "expected_revision":1, "content":"other writer before observation", "operation_id":"interleaved-observation"}))?;
        }
        let publication =
            capability == "storage.semantic" && input["operation"] == "publish_revisions";
        if !publication {
            let identity_read =
                capability == "storage.semantic" && input["operation"] == "project_identity";
            let mut output = self.invoke_host(capability, input)?;
            if identity_read && let Some(identity) = &self.reported_project_identity {
                output["identity"]["project_id"] = json!(identity);
            }
            return Ok(output);
        }
        self.publication_inputs.push(input.clone());
        if std::mem::take(&mut self.interleave_publication) {
            let mut other = input.clone();
            other["publication"]["operation_id"] = json!("other-writer");
            other["publication"]["artifacts"][0]["artifact"]["content"] = json!("other writer");
            self.invoke_host(capability, other)?;
        }
        let output = self.invoke_host(capability, input)?;
        if std::mem::take(&mut self.drop_publication_reply) {
            return Err(PluginError::new(
                "lost_publication_reply",
                "accepted reply was lost",
                true,
            ));
        }
        Ok(output)
    }
}

fn create_request() -> Value {
    json!({"artifact_id":"spec", "semantic_element_id":"owner", "project_root":"/artifact-project",
        "knowledge_type":"specification", "title":"A specification", "content":"first body"})
}

#[test]
fn accepted_replay_cannot_read_a_body_after_the_project_route_changes_lineage() {
    let mut host = ArtifactBoundaryHost::new();
    let request = create_request();
    host.knowledge("create_knowledge", request.clone()).unwrap();
    host.reported_project_identity = Some("new-project-lineage".into());
    let error = host.knowledge("create_knowledge", request).unwrap_err();
    assert_eq!(error.code, "knowledge_project_mismatch");
    assert!(error.message.contains("new-project-lineage"));
    assert_eq!(host.publication_inputs.len(), 1);
}

fn update_request(artifact: &Value, content: &str) -> Value {
    json!({"artifact_id":artifact["artifact_id"], "project_root":"/artifact-project",
        "expected_revision":artifact["revision"], "content":content})
}

#[test]
fn knowledge_saves_coalesce_in_one_working_body_under_the_same_element_revision() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let original = host.exact_artifact(first["artifact_id"].as_str().unwrap());
    let next = host
        .knowledge("update_knowledge", update_request(&first, "second body"))
        .unwrap();
    assert_eq!(next["artifact"]["artifact_id"], first["artifact_id"]);
    assert_eq!(next["artifact"]["content"], "second body");
    assert_ne!(
        host.exact_artifact(first["artifact_id"].as_str().unwrap()),
        original
    );
    assert_eq!(next["artifact"]["revision"], 2);
    assert_eq!(next["artifact"]["edit_version"], 2);
}

#[test]
fn lost_knowledge_reply_replays_frozen_input_even_after_a_later_update() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let mut request = update_request(&first, "second body");
    request["operation_id"] = json!("lost-reply-edit");
    host.drop_publication_reply = true;
    assert!(host.knowledge("update_knowledge", request.clone()).is_err());
    let frozen = host.publication_inputs.last().unwrap().clone();
    let accepted = host.knowledge("update_knowledge", request.clone()).unwrap();
    assert_eq!(host.publication_inputs.last().unwrap(), &frozen);
    let later = host
        .knowledge(
            "update_knowledge",
            update_request(&accepted["artifact"], "third body"),
        )
        .unwrap();
    assert_eq!(
        host.knowledge("update_knowledge", request)
            .unwrap_err()
            .code,
        "knowledge_artifact_edit_unavailable"
    );
    let acknowledgement = host
        .knowledge(
            "get_artifact_write_receipt",
            json!({"project_root":"/artifact-project",
        "operation_id":"lost-reply-edit", "action":"update"}),
        )
        .unwrap();
    assert_eq!(acknowledgement["lifecycle_revision"], 2);
    assert_eq!(
        host.exact_artifact(later["artifact"]["artifact_id"].as_str().unwrap())["content"],
        "third body"
    );
}

#[test]
fn an_interleaved_knowledge_writer_conflicts_instead_of_overwriting() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    host.interleave_publication = true;
    let error = host
        .knowledge("update_knowledge", update_request(&first, "my stale body"))
        .unwrap_err();
    assert_eq!(error.code, "knowledge_revision_conflict");
    assert_eq!(
        host.exact_artifact(first["artifact_id"].as_str().unwrap())["content"],
        "other writer"
    );
}

#[test]
fn a_different_request_using_an_old_artifact_id_cannot_replace_its_successor() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    host.knowledge("update_knowledge", update_request(&first, "second body"))
        .unwrap();
    let mut stale = update_request(&first, "stale overwrite");
    stale.as_object_mut().unwrap().remove("expected_revision");
    let error = host.knowledge("update_knowledge", stale).unwrap_err();
    assert_eq!(error.code, "knowledge_revision_conflict");
}

#[test]
fn a_document_body_identity_refuses_knowledge_overwrite_at_the_real_boundary() {
    let mut host = ArtifactBoundaryHost::new();
    let document_body = lumvise_db_core::SemanticArtifact {
        artifact_id: "spec".into(),
        semantic_element_id: "owner".into(),
        artifact_kind: "knowledge_document".into(),
        title: "Document body".into(),
        content: Some("immutable document body".into()),
        content_ref: None,
        searchable_text: None,
        content_size_bytes: None,
        dependencies: vec![],
        metadata: json!({"knowledge_document":{"schema":2}}),
    };
    host.execute(SemanticOperation::UpsertArtifact {
        artifact: document_body,
        media_type: "text/plain".into(),
    });
    let original = host.exact_artifact("spec");
    let error = host
        .knowledge("create_knowledge", create_request())
        .unwrap_err();
    assert_eq!(error.code, "knowledge_id_conflict");
    assert_eq!(host.exact_artifact("spec"), original);
}

#[test]
fn an_explicit_operation_id_cannot_be_reused_for_a_different_request() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let mut request = update_request(&first, "second body");
    request["operation_id"] = json!("user-action-1");
    host.knowledge("update_knowledge", request.clone()).unwrap();
    request["content"] = json!("a different operation's body");
    let error = host.knowledge("update_knowledge", request).unwrap_err();
    assert_eq!(error.code, "invalid_knowledge_publication");
    assert_eq!(host.publication_inputs.len(), 2);
}

#[test]
fn a_new_explicit_action_can_edit_a_restored_revision_with_the_same_content() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let mut request = update_request(&first, "second body");
    checkpoint_artifact(&host, first["artifact_id"].as_str().unwrap());
    request["operation_id"] = json!("user-action-1");
    let prior = host.knowledge("update_knowledge", request.clone()).unwrap();
    restore_artifact(&host, first["artifact_id"].as_str().unwrap());
    request["operation_id"] = json!("user-action-2");
    let accepted = host.knowledge("update_knowledge", request).unwrap();
    assert_ne!(
        accepted["artifact"]["artifact_id"],
        prior["artifact"]["artifact_id"]
    );
    let SemanticResult::RevisionSelection(Some(selected)) =
        host.execute(SemanticOperation::RevisionSelection {
            project_root: "/artifact-project".into(),
            selection_id: "working".into(),
        })
    else {
        panic!("expected current selection")
    };
    assert_eq!(
        selected.artifacts[0].artifact_id,
        accepted["artifact"]["artifact_id"].as_str().unwrap()
    );
}

fn restore_artifact(host: &ArtifactBoundaryHost, artifact_id: &str) {
    let root = "/artifact-project";
    let SemanticResult::ProjectIdentity(identity) =
        host.execute(SemanticOperation::ProjectIdentity {
            project_root: root.into(),
        })
    else {
        panic!("expected lineage")
    };
    let SemanticResult::RevisionSelection(Some(current)) =
        host.execute(SemanticOperation::RevisionSelection {
            project_root: root.into(),
            selection_id: "working".into(),
        })
    else {
        panic!("expected selection")
    };
    host.execute(SemanticOperation::SelectRevisions(
        lumvise_db_core::SemanticRevisionSelectionUpdate {
            artifact_edits: Default::default(),
            operation_id: "restore-artifact".into(),
            project_root: root.into(),
            project_id: identity.project_id,
            selection_id: "working".into(),
            source_context: "local".into(),
            expected_generation: current.selection.generation,
            mode: lumvise_db_core::SemanticSelectionMode::FollowAccepted,
            records: vec![
                lumvise_db_core::SemanticRecordRef::element("owner"),
                lumvise_db_core::SemanticRecordRef::artifact(artifact_id),
            ],
            connection_ids: vec![],
            source_anchor: None,
        },
    ));
}

fn checkpoint_artifact(host: &ArtifactBoundaryHost, id: &str) {
    let root = "/artifact-project";
    let SemanticResult::ProjectIdentity(identity) =
        host.execute(SemanticOperation::ProjectIdentity {
            project_root: root.into(),
        })
    else {
        panic!("expected lineage");
    };
    let SemanticResult::RevisionSelection(Some(current)) =
        host.execute(SemanticOperation::RevisionSelection {
            project_root: root.into(),
            selection_id: "working".into(),
        })
    else {
        panic!("expected working selection");
    };
    let mut pin: lumvise_db_core::SemanticRevisionSelectionUpdate = serde_json::from_value(json!({
        "operation_id":format!("freeze-{id}-{}", current.selection.generation), "project_root":root,
        "project_id":identity.project_id, "selection_id":format!("checkpoint:{id}:{}", current.selection.generation),
        "source_context":"local", "expected_generation":0, "mode":"pinned",
        "records":[{"kind":"artifact", "record_id":id}], "connection_ids":[], "source_anchor":null,
        "artifact_edits":current.expectation.artifact_edits})).unwrap();
    assert!(matches!(
        host.execute(SemanticOperation::SelectRevisions(pin.clone())),
        SemanticResult::RevisionPublication(
            lumvise_db_core::SemanticRevisionPublicationOutcome::Accepted(_)
        )
    ));
    pin.operation_id.push_str(":release");
    pin.expected_generation = 1;
    pin.records.clear();
    host.execute(SemanticOperation::SelectRevisions(pin));
}

#[test]
fn an_edit_between_the_original_body_read_and_observation_cannot_authorize_a_stale_save() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    host.interleave_observation = true;
    assert_eq!(
        host.knowledge("update_knowledge", update_request(&first, "Stale save"))
            .unwrap_err()
            .code,
        "knowledge_revision_conflict"
    );
    assert_eq!(
        host.knowledge("get_knowledge", json!({"artifact_id":"spec"}))
            .unwrap()["artifact"]["content"],
        "other writer before observation"
    );
}

#[test]
fn citing_a_working_artifact_requires_its_original_edit_version() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let mut citation = create_request();
    citation["artifact_id"] = json!("citation");
    citation["dependencies"] = json!([{"target":{"target_kind":"artifact", "artifact_id":"spec"}}]);
    citation["operation_id"] = json!("missing-citation-token");
    assert_eq!(
        host.knowledge("create_knowledge", citation.clone())
            .unwrap_err()
            .code,
        "knowledge_revision_conflict"
    );
    citation["operation_id"] = json!("original-citation-token");
    citation["dependencies"][0]["target"]["edit_version"] = first["edit_version"].clone();
    host.knowledge("create_knowledge", citation).unwrap();
    let next = host
        .knowledge("update_knowledge", update_request(&first, "After citation"))
        .unwrap();
    assert_ne!(next["artifact"]["artifact_id"], first["artifact_id"]);
}

#[test]
fn a_lost_artifact_reply_replays_after_closing_and_reopening_the_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cooperation.db");
    let (request, frozen) = {
        let mut host = ArtifactBoundaryHost::with_persistence(Arc::new(
            LocalPersistence::open(&path).unwrap(),
        ));
        host.seed_owner();
        let first = host
            .knowledge("create_knowledge", create_request())
            .unwrap()["artifact"]
            .clone();
        let mut request = update_request(&first, "second body");
        request["operation_id"] = json!("crash-safe-edit");
        host.drop_publication_reply = true;
        host.knowledge("update_knowledge", request.clone())
            .unwrap_err();
        (request, host.publication_inputs.last().unwrap().clone())
    };
    let mut host =
        ArtifactBoundaryHost::with_persistence(Arc::new(LocalPersistence::open(&path).unwrap()));
    let accepted = host.knowledge("update_knowledge", request).unwrap();
    assert_eq!(host.publication_inputs, vec![frozen]);
    assert_eq!(accepted["artifact"]["revision"], 2);
}

#[test]
fn a_terminal_conflict_drops_its_frozen_body_but_replays_the_same_conflict() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let request = update_request(&first, "unneeded conflicted body");
    host.interleave_publication = true;
    let error = host
        .knowledge("update_knowledge", request.clone())
        .unwrap_err();
    assert_eq!(
        host.knowledge("update_knowledge", request).unwrap_err(),
        error
    );
    assert_eq!(host.publication_inputs.len(), 2);
    let rows = RelationalPersistence::execute(
        host.persistence.as_ref(),
        lumvise_db_core::RelationalOperation::ListPluginData {
            plugin_id: "builtin.knowledge".into(),
            table_name: "knowledge_publications".into(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    let lumvise_db_core::RelationalResult::PluginDataRows(rows) = rows else {
        panic!("expected journal rows")
    };
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains("unneeded conflicted body")
    );
}

#[test]
fn removing_a_current_artifact_keeps_its_exact_body_and_replays_the_same_effect() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let original = host.exact_artifact(first["artifact_id"].as_str().unwrap());
    let request = json!({"artifact_id":first["artifact_id"], "expected_revision":first["revision"], "project_root":"/artifact-project", "operation_id":"remove-spec"});
    let deleted = host.knowledge("delete_knowledge", request.clone()).unwrap();
    assert_eq!(deleted["deleted"], true);
    assert_eq!(
        host.exact_artifact(first["artifact_id"].as_str().unwrap()),
        original
    );
    assert_eq!(
        host.knowledge("delete_knowledge", request).unwrap(),
        deleted
    );
    let SemanticResult::RevisionSelection(Some(selected)) =
        host.execute(SemanticOperation::RevisionSelection {
            project_root: "/artifact-project".into(),
            selection_id: "working".into(),
        })
    else {
        panic!("expected current selection")
    };
    assert!(selected.artifacts.is_empty());
    let absent = json!({"artifact_id":first["artifact_id"],"project_root":"/artifact-project",
        "expected_revision":0,"operation_id":"remove-observed-absence"});
    let noop = host.knowledge("delete_knowledge", absent.clone()).unwrap();
    assert_eq!(noop["deleted"], false);
    assert_eq!(host.knowledge("delete_knowledge", absent).unwrap(), noop);
    let stale = json!({"artifact_id":first["artifact_id"],"project_root":"/artifact-project",
        "expected_revision":first["revision"],"operation_id":"another-stale-removal"});
    assert_eq!(
        host.knowledge("delete_knowledge", stale).unwrap_err().code,
        "knowledge_revision_conflict"
    );
    assert_eq!(
        host.exact_artifact(first["artifact_id"].as_str().unwrap()),
        original
    );
}

#[test]
fn a_missing_artifact_removal_is_a_durable_noop() {
    let mut host = ArtifactBoundaryHost::new();
    let request = json!({"artifact_id":"missing", "expected_revision":0, "project_root":"/artifact-project", "operation_id":"remove-missing"});
    let result = host.knowledge("delete_knowledge", request.clone()).unwrap();
    assert_eq!(result["deleted"], false);
    assert_eq!(
        host.knowledge("delete_knowledge", request.clone()).unwrap(),
        result
    );
    assert!(host.publication_inputs.is_empty());
    let mut create = create_request();
    create["artifact_id"] = json!("missing");
    let created = host.knowledge("create_knowledge", create).unwrap();
    assert_eq!(
        host.knowledge("delete_knowledge", request.clone()).unwrap(),
        result
    );
    let mut stale_absence = request;
    stale_absence["operation_id"] = json!("different-observed-absence");
    assert_eq!(
        host.knowledge("delete_knowledge", stale_absence)
            .unwrap_err()
            .code,
        "knowledge_revision_conflict"
    );
    assert_eq!(
        host.knowledge("get_knowledge", json!({"artifact_id":"missing"}))
            .unwrap(),
        created
    );
}

#[test]
fn a_stale_artifact_removal_cannot_deselect_its_current_successor() {
    let mut host = ArtifactBoundaryHost::new();
    let first = host
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let next = host
        .knowledge("update_knowledge", update_request(&first, "second body"))
        .unwrap();
    let request = json!({"artifact_id":first["artifact_id"], "expected_revision":first["revision"], "project_root":"/artifact-project"});
    assert_eq!(
        host.knowledge("delete_knowledge", request)
            .unwrap_err()
            .code,
        "knowledge_revision_conflict"
    );
    assert_eq!(
        host.exact_artifact(next["artifact"]["artifact_id"].as_str().unwrap())["content"],
        "second body"
    );
}
