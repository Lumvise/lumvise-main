//! Knowledge notes flag an anchor rewritten or removed since their last write, through the real persistence broker.
use std::sync::Arc;

use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{
    LocalPersistence, RelationalPersistence, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticRecordRef, SemanticResult, SemanticRevisionPublication,
    SemanticRevisionPublicationOutcome, SemanticRevisionStructure,
};
use lumvise_plugin_knowledge::KnowledgePlugin;
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

const PROJECT: &str = "/outdated-project";
const BASELINE: &str = "fp1:0000000000000000:a";
// Eight simhash bits flipped: at the identity threshold, which is not outdated.
const DRIFT: &str = "fp1:00000000000000ff:b";
// Complement of DRIFT's simhash: 64 bits from the drifted baseline, well past the identity threshold.
const REWRITTEN: &str = "fp1:ffffffffffffff00:c";

struct OutdatedBoundaryHost {
    persistence: Arc<LocalPersistence>,
    broker: AppCoreHostCapabilityBroker,
}

impl OutdatedBoundaryHost {
    fn new() -> Self {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        Self {
            persistence,
            broker: AppCoreHostCapabilityBroker::new(semantic, relational),
        }
    }

    /// Seeds the project before any note exists; afterwards only revision publication is valid.
    fn sync_structure(&self, elements: Vec<SemanticElement>) {
        SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::SyncStructure {
                project_root: PROJECT.into(),
                relationships: vec![],
                elements,
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    }

    /// Re-indexes a revision-managed project. An element left out of `elements` leaves the selection.
    /// Returns the accepted record ID that the `owner` element was published as.
    fn publish_structure(&self, elements: Vec<SemanticElement>) -> Option<String> {
        let control = InvocationControl::sixty_seconds();
        let execute = |operation| {
            SemanticPersistence::execute(self.persistence.as_ref(), operation, &control).unwrap()
        };
        let SemanticResult::RevisionSelection(Some(head)) =
            execute(SemanticOperation::RevisionSelection {
                project_root: PROJECT.into(),
                selection_id: "working".into(),
            })
        else {
            panic!("expected revision-managed selection")
        };
        let SemanticResult::ProjectIdentity(identity) =
            execute(SemanticOperation::ProjectIdentity {
                project_root: PROJECT.into(),
            })
        else {
            panic!("expected project lineage")
        };
        let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) = execute(SemanticOperation::PublishRevisions(
            SemanticRevisionPublication {
                operation_id: format!("outdated-reindex:{}", head.selection.generation),
                project_root: PROJECT.into(),
                project_id: identity.project_id,
                selection_id: "working".into(),
                source_context: "local".into(),
                producer_base: None,
                expected: head.expectation,
                structure: Some(SemanticRevisionStructure {
                    partition: None,
                    elements,
                    relationships: vec![],
                }),
                artifacts: vec![],
                removed_artifact_ids: vec![],
                source_anchor: None,
            },
        ))
        else {
            panic!("expected accepted revision publication")
        };
        receipt
            .remaps
            .iter()
            .find(|remap| remap.incoming == SemanticRecordRef::element("owner"))
            .map(|remap| remap.accepted.record_id.clone())
    }

    fn knowledge(&mut self, export: &str, input: Value) -> Value {
        KnowledgePlugin::default()
            .dispatch(export, input, &mut PluginContext::for_test(self))
            .unwrap()
    }
}

impl HostCallTransport for OutdatedBoundaryHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.broker
            .invoke(HostCapabilityRequest {
                plugin_id: "builtin.knowledge".into(),
                invocation_id: "outdated-anchor".into(),
                call_id: "outdated-anchor-host-call".into(),
                capability_id: capability.into(),
                required_version: "^1.0".into(),
                input,
            })
            .map_err(|error| PluginError::new("outdated_test_host_error", error.to_string(), false))
    }
}

fn owner(fingerprint: &str) -> SemanticElement {
    named_owner(fingerprint, "owner")
}

fn named_owner(fingerprint: &str, name: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: PROJECT.into(),
        semantic_element_id: "owner".into(),
        semantic_source_id: "source".into(),
        path: "source.rs".into(),
        element_kind: "function".into(),
        name: name.into(),
        parent_element_id: None,
        content_fingerprint: Some(fingerprint.into()),
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn create_note(host: &mut OutdatedBoundaryHost) {
    let created = host.knowledge(
        "create_knowledge",
        json!({"artifact_id":"spec", "semantic_element_id":"owner", "project_root":PROJECT,
            "knowledge_type":"specification", "title":"Owner specification",
            "content":"The owner behaves as written."}),
    );
    assert_eq!(created["artifact"]["anchor_fingerprint"], BASELINE);
    assert!(created["artifact"].get("outdated").is_none());
}

/// The note's current record. Structure publication and working saves can move it off its original ID.
fn current_note(host: &mut OutdatedBoundaryHost) -> Value {
    host.knowledge("list_knowledge", json!({"project_root":PROJECT}))["artifacts"][0].clone()
}

/// Confirms the note against the current code with only its ID and revision.
fn confirm_unchanged(host: &mut OutdatedBoundaryHost) -> Value {
    let note = current_note(host);
    host.knowledge(
        "update_knowledge",
        json!({"artifact_id":note["artifact_id"], "expected_revision":note["revision"]}),
    )
}

#[test]
fn notes_flag_an_anchor_rewritten_or_removed_since_their_last_write() {
    let mut host = OutdatedBoundaryHost::new();
    host.sync_structure(vec![owner(BASELINE)]);
    create_note(&mut host);
    assert!(current_note(&mut host).get("outdated").is_none());

    // Eight bits of drift stay at the identity threshold and are not outdated.
    host.publish_structure(vec![owner(DRIFT)]);
    let drifted = current_note(&mut host);
    assert_eq!(drifted["anchor_fingerprint"], BASELINE);
    assert!(drifted.get("outdated").is_none());
    let confirmed = confirm_unchanged(&mut host);
    assert_eq!(confirmed["artifact"]["anchor_fingerprint"], DRIFT);
    assert!(confirmed["artifact"].get("outdated").is_none());
    assert!(current_note(&mut host).get("outdated").is_none());

    // Rewriting the anchor 64 bits away is reported on every read path.
    let head_id = host
        .publish_structure(vec![owner(REWRITTEN)])
        .expect("owner keeps a current revision");
    let rewritten = json!({"reason":"rewritten", "distance":64});
    let note = current_note(&mut host);
    assert_eq!(note["outdated"], rewritten);
    assert_eq!(
        host.knowledge("get_knowledge", json!({"artifact_id":note["artifact_id"]}))["artifact"]["outdated"],
        rewritten
    );
    assert_eq!(
        host.knowledge(
            "list_knowledge_for_element",
            json!({"semantic_element_id":head_id})
        )["artifacts"][0]["outdated"],
        rewritten
    );
    assert_eq!(
        host.knowledge(
            "search_knowledge",
            json!({"query":"Owner specification", "project_root":PROJECT})
        )["results"][0]["outdated"],
        rewritten
    );

    // Confirming the rewritten anchor moves the baseline to it and clears the signal.
    let confirmed = confirm_unchanged(&mut host);
    assert_eq!(confirmed["artifact"]["anchor_fingerprint"], REWRITTEN);
    assert!(confirmed["artifact"].get("outdated").is_none());
    assert!(current_note(&mut host).get("outdated").is_none());

    // Leaving the owner out of the structure removes its anchor from the selection.
    host.publish_structure(vec![]);
    assert_eq!(
        current_note(&mut host)["outdated"],
        json!({"reason":"removed"})
    );
}

#[test]
fn a_rename_with_unchanged_content_follows_the_successor_and_is_not_outdated() {
    let mut host = OutdatedBoundaryHost::new();
    host.sync_structure(vec![owner(BASELINE)]);
    create_note(&mut host);
    let successor = host
        .publish_structure(vec![named_owner(BASELINE, "renamed")])
        .expect("renamed owner keeps a current revision");
    assert_ne!(
        successor, "owner",
        "rename must publish a successor revision"
    );
    let note = current_note(&mut host);
    assert!(note.get("outdated").is_none());
    assert_eq!(note["anchor_fingerprint"], BASELINE);
}
