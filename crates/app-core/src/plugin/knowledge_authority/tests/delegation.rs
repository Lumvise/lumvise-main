use super::*;
use lumvise_db_core::{PluginDataMutation, RelationalOperation, SemanticProjectAccess};
mod executor;
mod preparation;

fn permission_call(
    authority: &KnowledgeAuthority,
    source: &dyn SemanticPersistence,
    context: &PluginInvocationContext,
    request: Value,
) -> Result<Value, HostCapabilityError> {
    authority.invoke(
        "builtin.knowledge",
        json!({"operation":"delegation","request":request}),
        source,
        context,
    )
}

fn owner_context(session: &str) -> PluginInvocationContext {
    invocation(session, 1).enter_plugin("builtin.knowledge", "configure_document_hook")
}

fn permission_intent() -> Value {
    json!({"operation":"save", "intent":{"operation_id":"grant", "document_id":"decision", "expected_revision":null,
        "enabled":true,"inputs":[{"target_kind":"document","document_id":"source-note"}],"required_projects":[]}})
}

fn executor_context(
    fixture: &AuthorityFixture,
    authority: &KnowledgeAuthority,
    agent: &str,
) -> PluginInvocationContext {
    let store = fixture.persistence.as_ref();
    let control = InvocationControl::sixty_seconds();
    RelationalPersistence::execute(
        store,
        RelationalOperation::EnsurePluginDataTable {
            plugin_id: "builtin.agents".into(),
            table_name: "delegation_test_owner".into(),
            schema: json!({}),
        },
        &control,
    )
    .unwrap();
    RelationalPersistence::execute(
        store,
        RelationalOperation::ApplyMutations {
            plugin_id: "builtin.agents".into(),
            mutations: vec![PluginDataMutation::Put {
                table_name: "delegation_test_owner".into(),
                row_key: agent.into(),
                value: json!({"enabled":true}),
            }],
        },
        &control,
    )
    .unwrap();
    let context = PluginInvocationContext::new(
        "maintenance",
        "builtin.agents",
        PluginInvocationClass::Background,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.agents", "agent.perform_action");
    bind_executor(fixture, authority, &context, agent, "autonomous");
    context
}

fn bind_executor(
    fixture: &AuthorityFixture,
    authority: &KnowledgeAuthority,
    context: &PluginInvocationContext,
    agent: &str,
    purpose: &str,
) {
    authority.invoke("builtin.agents", json!({"operation":"bind_agent","binding":{
        "agent_id":agent, "grants":[], "purpose":purpose, "run_id":null,
        "observations":[{"table":"delegation_test_owner","row_key":agent,"expected":{"enabled":true}}]
    }}), fixture.persistence.as_ref(), context).unwrap();
}

fn bind_request(grant: &Value, action: Value) -> Value {
    json!({"operation":"bind","delegation_id":grant["delegation_id"],"revision":grant["revision"],"action":action})
}

fn activate(fixture: &AuthorityFixture, source: &dyn SemanticPersistence, grant: &Value) {
    permission_call(&fixture.authority, source, &owner_context("owner"), json!({"operation":"activate",
        "delegation_id":grant["delegation_id"],"revision":grant["revision"],"observations":[{
            "table":"delegation_test_owner","row_key":grant["executor_agent_id"],"expected":{"enabled":true}
        }]})).unwrap();
}

#[test]
fn delegation_survives_conversation_end_and_host_restart_but_never_grants_the_whole_space() {
    let fixture = AuthorityFixture::new();
    fixture.bind("owner", 1, true).unwrap();
    let saved = permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner_context("owner"),
        permission_intent(),
    )
    .unwrap();
    let grant = &saved["delegation"];
    let initial = executor_context(
        &fixture,
        &fixture.authority,
        grant["executor_agent_id"].as_str().unwrap(),
    )
    .enter_plugin("builtin.knowledge", "execute_document_delegation");
    assert!(
        permission_call(
            &fixture.authority,
            fixture.persistence.as_ref(),
            &initial,
            bind_request(grant, json!({"action":"read_destination"}))
        )
        .is_err()
    );
    activate(&fixture, fixture.persistence.as_ref(), grant);
    fixture.bind("owner", 1, false).unwrap();
    let reopened = KnowledgeAuthority::new(fixture.persistence.clone(), Arc::default());
    let executor = executor_context(
        &fixture,
        &reopened,
        grant["executor_agent_id"].as_str().unwrap(),
    );
    let normal = reopened
        .current(
            fixture.persistence.as_ref(),
            &executor,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    assert!(normal.project_grants.is_empty());
    assert_ne!(
        normal.space.as_ref().unwrap().project_root,
        grant["owner"]["space"]["project_root"]
    );
    let execution = executor
        .clone()
        .enter_plugin("builtin.knowledge", "execute_document_delegation");
    let selected = permission_call(
        &reopened,
        fixture.persistence.as_ref(),
        &execution,
        bind_request(grant, json!({"action":"read_destination"})),
    )
    .unwrap();
    assert_eq!(selected["document_id"], "decision");
    assert_eq!(
        selected["project_root"],
        grant["owner"]["space"]["project_root"]
    );
    assert!(permission_call(&reopened, fixture.persistence.as_ref(), &execution,
        bind_request(grant, json!({"action":"read_input", "reference":{"target_kind":"document","document_id":"other-secret"}}))).is_err());
    assert!(
        permission_call(
            &reopened,
            fixture.persistence.as_ref(),
            &executor,
            bind_request(grant, json!({"action":"read_destination"}))
        )
        .is_err()
    );
    let stranger = executor_context(&fixture, &reopened, "stranger")
        .enter_plugin("builtin.knowledge", "execute_document_delegation");
    assert!(
        permission_call(
            &reopened,
            fixture.persistence.as_ref(),
            &stranger,
            bind_request(grant, json!({"action":"read_destination"}))
        )
        .is_err()
    );
}

#[test]
fn disabling_a_delegation_revokes_an_already_bound_frame_and_old_retry_never_reenables_it() {
    let fixture = AuthorityFixture::new();
    fixture.bind("owner", 1, true).unwrap();
    let owner = owner_context("owner");
    let request = permission_intent();
    let saved = permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner,
        request.clone(),
    )
    .unwrap();
    let grant = &saved["delegation"];
    let execution = executor_context(
        &fixture,
        &fixture.authority,
        grant["executor_agent_id"].as_str().unwrap(),
    )
    .enter_plugin("builtin.knowledge", "execute_document_delegation");
    activate(&fixture, fixture.persistence.as_ref(), grant);
    permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &execution,
        bind_request(grant, json!({"action":"read_destination"})),
    )
    .unwrap();
    let mut disabled = request.clone();
    disabled["intent"]["operation_id"] = json!("disable");
    disabled["intent"]["expected_revision"] = grant["revision"].clone();
    disabled["intent"]["enabled"] = json!(false);
    permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner,
        disabled,
    )
    .unwrap();
    assert!(
        fixture
            .authority
            .current(
                fixture.persistence.as_ref(),
                &execution,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    assert_eq!(
        permission_call(
            &fixture.authority,
            fixture.persistence.as_ref(),
            &owner,
            request.clone()
        )
        .unwrap(),
        saved
    );
    let latest = permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner,
        json!({"operation":"get","document_id":"decision"}),
    )
    .unwrap();
    assert_eq!(latest["delegation"]["enabled"], false);
    let mut changed = request;
    changed["intent"]["document_id"] = json!("other");
    changed["intent"]["expected_revision"] = json!(1);
    assert!(
        permission_call(
            &fixture.authority,
            fixture.persistence.as_ref(),
            &owner,
            changed
        )
        .is_err()
    );
}

struct FakeDelegationMembership {
    local: Arc<LocalPersistence>,
    access: Mutex<SemanticProjectAccess>,
}

impl SemanticPersistence for FakeDelegationMembership {
    fn readiness(&self) -> lumvise_db_core::PersistenceResult<lumvise_db_core::SemanticReadiness> {
        SemanticPersistence::readiness(self.local.as_ref())
    }
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::PersistenceResult<SemanticResult> {
        match operation {
            SemanticOperation::ProjectRoots => {
                Ok(SemanticResult::ProjectRoots(vec!["/source".into()]))
            }
            SemanticOperation::ProjectAccess { project_root } if project_root == "/source" => Ok(
                SemanticResult::ProjectAccess(self.access.lock().unwrap().clone()),
            ),
            other => SemanticPersistence::execute(self.local.as_ref(), other, control),
        }
    }
}

#[test]
fn source_membership_is_rechecked_and_equal_lineage_in_a_different_authority_is_denied() {
    let fixture = AuthorityFixture::new();
    fixture.bind("owner", 1, true).unwrap();
    let source = FakeDelegationMembership {
        local: fixture.persistence.clone(),
        access: Mutex::new(SemanticProjectAccess::Member {
            project_id: "lineage".into(),
            storage_authority: "original-store".into(),
            writable: false,
        }),
    };
    let mut request = permission_intent();
    request["intent"]["required_projects"] = json!([{"project_id":"lineage","project_root":"/source","storage_authority":"original-store"}]);
    let saved = permission_call(
        &fixture.authority,
        &source,
        &owner_context("owner"),
        request,
    )
    .unwrap();
    let grant = &saved["delegation"];
    let execution = executor_context(
        &fixture,
        &fixture.authority,
        grant["executor_agent_id"].as_str().unwrap(),
    )
    .enter_plugin("builtin.knowledge", "execute_document_delegation");
    activate(&fixture, &source, grant);
    permission_call(
        &fixture.authority,
        &source,
        &execution,
        bind_request(grant, json!({"action":"read_destination"})),
    )
    .unwrap();
    for access in [
        SemanticProjectAccess::Denied,
        SemanticProjectAccess::Member {
            project_id: "lineage".into(),
            storage_authority: "copied-store".into(),
            writable: true,
        },
    ] {
        *source.access.lock().unwrap() = access;
        assert!(
            fixture
                .authority
                .current(&source, &execution, &InvocationControl::sixty_seconds())
                .is_err()
        );
    }
}

#[test]
fn mcp_delegation_rechecks_an_authorized_project_absent_from_the_index_catalog() {
    let fixture = AuthorityFixture::new();
    let owner = fixture
        .mcp_invocation("unindexed-mcp", "/unindexed-project")
        .enter_plugin("builtin.knowledge", "configure_document_hook");
    let saved = permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner,
        permission_intent(),
    )
    .unwrap();
    let grant = &saved["delegation"];
    let execution = executor_context(
        &fixture,
        &fixture.authority,
        grant["executor_agent_id"].as_str().unwrap(),
    )
    .enter_plugin("builtin.knowledge", "execute_document_delegation");
    permission_call(&fixture.authority, fixture.persistence.as_ref(), &owner, json!({"operation":"activate",
        "delegation_id":grant["delegation_id"],"revision":grant["revision"],"observations":[{
            "table":"delegation_test_owner","row_key":grant["executor_agent_id"],"expected":{"enabled":true}
        }]})).unwrap();
    let bound = permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &execution,
        bind_request(grant, json!({"action":"read_destination"})),
    )
    .unwrap();
    assert_eq!(
        bound["project_root"],
        grant["owner"]["space"]["project_root"]
    );
}

#[test]
fn accepted_executor_configuration_change_revokes_an_already_bound_delegation() {
    let fixture = AuthorityFixture::new();
    fixture.bind("owner", 1, true).unwrap();
    let saved = permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner_context("owner"),
        permission_intent(),
    )
    .unwrap();
    let grant = &saved["delegation"];
    let execution = executor_context(
        &fixture,
        &fixture.authority,
        grant["executor_agent_id"].as_str().unwrap(),
    )
    .enter_plugin("builtin.knowledge", "execute_document_delegation");
    activate(&fixture, fixture.persistence.as_ref(), grant);
    permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &execution,
        bind_request(grant, json!({"action":"read_destination"})),
    )
    .unwrap();
    RelationalPersistence::execute(
        fixture.persistence.as_ref(),
        RelationalOperation::ApplyMutations {
            plugin_id: "builtin.agents".into(),
            mutations: vec![PluginDataMutation::Put {
                table_name: "delegation_test_owner".into(),
                row_key: grant["executor_agent_id"].as_str().unwrap().into(),
                value: json!({"enabled":true,"objective":"different"}),
            }],
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    assert!(
        fixture
            .authority
            .current(
                fixture.persistence.as_ref(),
                &execution,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
}
