//! The bounded command reaches the real host and document journal, never a fake grant policy.
use super::*;
use lumvise_db_core::{PluginDataMutation, RelationalOperation};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

struct DelegatedDocumentFixture {
    host: DocumentBoundaryHost,
    owner: PluginInvocationContext,
    grant: Value,
    original: Value,
}

impl DelegatedDocumentFixture {
    fn new() -> Self {
        let mut host = DocumentBoundaryHost::new();
        host.invocation_context = PluginInvocationContext::new(
            "memory-owner",
            "builtin.assistant",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        )
        .with_route_identity("owner", Some("assistant_session".into()))
        .with_scope_epoch(Some(1));
        let owner = host.invocation_context.clone();
        bind_owner(&mut host, true);
        let space = host
            .knowledge("resolve_document_space", json!({"purpose":"self"}))
            .unwrap()["space"]
            .clone();
        let grant = save_permission(&mut host, &owner);
        let original = create_hook_document(&mut host, &space, &grant);
        for id in ["source-note", "other-secret"] {
            host.knowledge(
                "create_document",
                json!({"project_root":space["project_root"],"document_id":id,
                "document_kind":"context","title":id,"content":id}),
            )
            .unwrap();
        }
        install_executor(&host, &grant);
        activate(&mut host, &owner, &grant);
        Self {
            host,
            owner,
            grant,
            original,
        }
    }

    fn execute(&mut self, action: Value) -> Result<Value, PluginError> {
        self.host.knowledge("execute_document_delegation", json!({
            "delegation_id":self.grant["delegation_id"],"revision":self.grant["revision"],"action":action
        })).map(|reply| reply["output"].clone())
    }

    fn run(&mut self) {
        let id = self.grant["executor_agent_id"].as_str().unwrap();
        self.host.invocation_context = PluginInvocationContext::new(
            "maintenance",
            "builtin.agents",
            PluginInvocationClass::Background,
            Instant::now() + Duration::from_secs(60),
        )
        .enter_plugin("builtin.agents", "agent.perform_action");
        self.host.plugin_id = "builtin.agents";
        self.host.host_call("runtime.knowledge_scope", json!({"operation":"bind_agent","binding":{
            "agent_id":id,"grants":[],"purpose":"autonomous","run_id":null,"observations":[executor_fact(&self.grant)]
        }})).unwrap();
        self.host.plugin_id = "builtin.knowledge";
    }

    fn human_edit(&mut self, original: &Value, content: &str) -> Value {
        self.host.invocation_context = self.owner.clone();
        self.host.knowledge("update_document", json!({"project_root":original["project_root"],"document_id":"decision",
            "expected_semantic_element_id":original["semantic_element_id"],"expected_revision":original["revision"],"content":content})).unwrap()["document"].clone()
    }
}

fn bind_owner(host: &mut DocumentBoundaryHost, active: bool) {
    host.plugin_id = "builtin.assistant";
    host.host_call("runtime.knowledge_scope", json!({"operation":"bind_assistant","session_id":"owner","session_epoch":1,"active":active})).unwrap();
    host.plugin_id = "builtin.knowledge";
}

fn save_permission(host: &mut DocumentBoundaryHost, owner: &PluginInvocationContext) -> Value {
    host.invocation_context = owner
        .clone()
        .enter_plugin("builtin.knowledge", "configure_document_hook");
    let saved = host.host_call("runtime.knowledge_scope", json!({"operation":"delegation","request":{"operation":"save","intent":{
        "operation_id":"configure","document_id":"decision","expected_revision":null,"enabled":true,
        "inputs":[{"target_kind":"document","document_id":"source-note"}],"required_projects":[]
    }}})).unwrap();
    host.invocation_context = owner.clone();
    saved["delegation"].clone()
}

fn create_hook_document(host: &mut DocumentBoundaryHost, space: &Value, grant: &Value) -> Value {
    let body = json!(["Decision", "Original", [], {}]).to_string();
    let signature = format!("{:x}", Sha256::digest(body.as_bytes()));
    host.knowledge("create_document", json!({"project_root":space["project_root"],"document_id":"decision","document_kind":"context",
        "title":"Decision","content":"Original","metadata":{"generation":{"owner":"knowledge.memory_hook",
            "source_element_id":grant["delegation_id"],"metadata_fields":[],"content_sha256":signature}}})).unwrap()["document"].clone()
}

fn executor_fact(grant: &Value) -> Value {
    json!({"table":"memory_executor_fixture","row_key":grant["executor_agent_id"],"expected":{"enabled":true}})
}

fn install_executor(host: &DocumentBoundaryHost, grant: &Value) {
    let control = InvocationControl::sixty_seconds();
    RelationalPersistence::execute(
        host.persistence.as_ref(),
        RelationalOperation::EnsurePluginDataTable {
            plugin_id: "builtin.agents".into(),
            table_name: "memory_executor_fixture".into(),
            schema: json!({}),
        },
        &control,
    )
    .unwrap();
    RelationalPersistence::execute(
        host.persistence.as_ref(),
        RelationalOperation::ApplyMutations {
            plugin_id: "builtin.agents".into(),
            mutations: vec![PluginDataMutation::Put {
                table_name: "memory_executor_fixture".into(),
                row_key: grant["executor_agent_id"].as_str().unwrap().into(),
                value: json!({"enabled":true}),
            }],
        },
        &control,
    )
    .unwrap();
}

fn activate(host: &mut DocumentBoundaryHost, owner: &PluginInvocationContext, grant: &Value) {
    host.invocation_context = owner
        .clone()
        .enter_plugin("builtin.knowledge", "configure_document_hook");
    host.host_call("runtime.knowledge_scope", json!({"operation":"delegation","request":{"operation":"activate",
        "delegation_id":grant["delegation_id"],"revision":grant["revision"],"observations":[executor_fact(grant)]}})).unwrap();
    host.invocation_context = owner.clone();
}

fn proposal(original: &Value, operation: &str, content: &str) -> Value {
    json!({"action":"update_destination","operation_id":operation,"expected_revision":original["revision"],
        "expected_semantic_element_id":original["semantic_element_id"],"content":content})
}

#[test]
fn delegated_command_survives_owner_conversation_and_restricts_named_inputs_and_destination() {
    let mut fixture = DelegatedDocumentFixture::new();
    bind_owner(&mut fixture.host, false);
    fixture.run();
    assert_eq!(
        fixture
            .execute(json!({"action":"read_destination"}))
            .unwrap()["document"]["content"],
        "Original"
    );
    assert_eq!(fixture.execute(json!({"action":"read_input","reference":{"target_kind":"document","document_id":"source-note"}})).unwrap()["state"], "available");
    assert!(fixture.execute(json!({"action":"read_input","reference":{"target_kind":"document","document_id":"other-secret"}})).is_err());
    assert!(
        fixture
            .execute(json!({"action":"read_destination","document_id":"other-secret"}))
            .is_err()
    );
    assert!(
        fixture
            .host
            .knowledge(
                "get_document",
                json!({"project_root":fixture.original["project_root"],"document_id":"decision"})
            )
            .is_err()
    );
    assert!(
        fixture
            .execute(
                json!({"action":"update_destination","operation_id":"forge","expected_revision":1,
        "expected_semantic_element_id":"other","content":"No","references":[]})
            )
            .is_err()
    );
}

#[test]
fn delegated_content_write_replays_after_lost_reply_without_overwriting_a_later_human_edit() {
    let mut fixture = DelegatedDocumentFixture::new();
    fixture.run();
    let request = proposal(&fixture.original, "mission-step", "Maintained");
    fixture.host.drop_document_reply = true;
    assert!(fixture.execute(request.clone()).is_err());
    let receipt = fixture
        .execute(json!({"action":"read_receipt","operation_id":"mission-step"}))
        .unwrap();
    assert_eq!(receipt["state"], "pending");
    let accepted = fixture.execute(request.clone()).unwrap()["document"].clone();
    assert_eq!(accepted["content"], "Maintained");
    assert_eq!(
        fixture
            .execute(json!({"action":"read_receipt","operation_id":"mission-step"}))
            .unwrap()["state"],
        "accepted"
    );
    let human = fixture.human_edit(&accepted, "Keep my edit");
    fixture.run();
    assert_eq!(
        fixture.execute(request).unwrap()["document"]["semantic_element_id"],
        accepted["semantic_element_id"]
    );
    let held = fixture
        .execute(proposal(&human, "next-mission-step", "Overwrite human"))
        .unwrap_err();
    assert_eq!(held.code, "generated_document_authored");
    assert_eq!(
        fixture
            .execute(json!({"action":"read_destination"}))
            .unwrap()["document"]["content"],
        "Keep my edit"
    );
}

#[test]
fn stale_delegated_proposal_never_refreshes_its_editing_base() {
    let mut fixture = DelegatedDocumentFixture::new();
    fixture.run();
    let first = proposal(&fixture.original, "one", "First");
    fixture.execute(first).unwrap();
    let error = fixture
        .execute(proposal(&fixture.original, "two", "Stale"))
        .unwrap_err();
    assert_eq!(error.code, "document_revision_conflict");
    assert_eq!(
        fixture
            .execute(json!({"action":"read_destination"}))
            .unwrap()["document"]["content"],
        "First"
    );
}
