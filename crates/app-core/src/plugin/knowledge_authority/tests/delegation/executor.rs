use super::*;

fn current_executor(
    fixture: &AuthorityFixture,
    source: &dyn SemanticPersistence,
    context: &PluginInvocationContext,
) -> Result<KnowledgeAccessContext, HostCapabilityError> {
    fixture
        .authority
        .current(source, context, &InvocationControl::sixty_seconds())
}

fn saved_permission(fixture: &AuthorityFixture) -> Value {
    fixture.bind("owner", 1, true).unwrap();
    permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner_context("owner"),
        permission_intent(),
    )
    .unwrap()["delegation"]
        .clone()
}

fn prepare_executor(fixture: &AuthorityFixture, owner: &PluginInvocationContext, grant: &Value) {
    permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        owner,
        json!({"operation":"prepare_executor", "delegation_id":grant["delegation_id"],
        "revision":grant["revision"]}),
    )
    .unwrap();
}

fn setup_context(
    fixture: &AuthorityFixture,
    owner: &PluginInvocationContext,
    agent: &str,
) -> PluginInvocationContext {
    let context = owner.clone().enter_plugin("builtin.agents", "define_agent");
    bind_executor(
        fixture,
        &fixture.authority,
        &context,
        agent,
        "owner_control",
    );
    context
}

#[test]
fn foreground_calls_cannot_read_or_execute_a_private_memory_agent() {
    let fixture = AuthorityFixture::new();
    let grant = saved_permission(&fixture);
    let agent = grant["executor_agent_id"].as_str().unwrap();
    executor_context(&fixture, &fixture.authority, agent);
    activate(&fixture, fixture.persistence.as_ref(), &grant);
    let context = PluginInvocationContext::new(
        "inspect",
        "native",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.agents", "get_agent");
    for purpose in ["owner_control", "autonomous"] {
        bind_executor(&fixture, &fixture.authority, &context, agent, purpose);
        assert!(current_executor(&fixture, fixture.persistence.as_ref(), &context).is_err());
        let execution = context
            .clone()
            .enter_plugin("builtin.knowledge", "execute_document_delegation");
        assert!(
            permission_call(
                &fixture.authority,
                fixture.persistence.as_ref(),
                &execution,
                bind_request(&grant, json!({"action":"read_destination"}))
            )
            .is_err()
        );
    }
}

#[test]
fn executor_setup_is_bound_to_the_owning_frame_and_live_origin() {
    let fixture = AuthorityFixture::new();
    let grant = saved_permission(&fixture);
    let agent = grant["executor_agent_id"].as_str().unwrap();
    executor_context(&fixture, &fixture.authority, agent);
    let owner = owner_context("owner");
    prepare_executor(&fixture, &owner, &grant);
    let setup = setup_context(&fixture, &owner, agent);
    let scope = current_executor(&fixture, fixture.persistence.as_ref(), &setup).unwrap();
    assert_eq!(scope.principal_id, agent);
    assert!(scope.project_grants.is_empty() && scope.source_projects.is_empty());
    let detached = setup_context(&fixture, &owner_context("owner"), agent);
    assert!(current_executor(&fixture, fixture.persistence.as_ref(), &detached).is_err());
    fixture.bind("owner", 1, false).unwrap();
    assert!(current_executor(&fixture, fixture.persistence.as_ref(), &setup).is_err());
}

#[test]
fn a_new_permission_revision_invalidates_prepared_executor_setup() {
    let fixture = AuthorityFixture::new();
    let grant = saved_permission(&fixture);
    let agent = grant["executor_agent_id"].as_str().unwrap();
    executor_context(&fixture, &fixture.authority, agent);
    let owner = owner_context("owner");
    prepare_executor(&fixture, &owner, &grant);
    let setup = setup_context(&fixture, &owner, agent);
    let mut request = permission_intent();
    request["intent"]["operation_id"] = json!("disable");
    request["intent"]["expected_revision"] = grant["revision"].clone();
    request["intent"]["enabled"] = json!(false);
    permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner,
        request,
    )
    .unwrap();
    assert!(current_executor(&fixture, fixture.persistence.as_ref(), &setup).is_err());
}

#[test]
fn preparation_can_capture_the_bound_destination_but_cannot_execute_a_write() {
    let fixture = AuthorityFixture::new();
    let grant = saved_permission(&fixture);
    let agent = grant["executor_agent_id"].as_str().unwrap();
    executor_context(&fixture, &fixture.authority, agent);
    let owner = owner_context("owner");
    prepare_executor(&fixture, &owner, &grant);
    let setup = setup_context(&fixture, &owner, agent)
        .enter_plugin("builtin.knowledge", "execute_document_delegation");
    permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &setup,
        bind_request(&grant, json!({"action":"read_destination"})),
    )
    .unwrap();
    assert!(permission_call(&fixture.authority, fixture.persistence.as_ref(), &setup,
        bind_request(&grant, json!({"action":"update_destination","operation_id":"write", "expected_semantic_element_id":"original",
            "expected_revision":1,"content":"unauthorized write"}))).is_err());
    fixture.bind("owner", 1, false).unwrap();
    assert!(current_executor(&fixture, fixture.persistence.as_ref(), &setup).is_err());
}

#[test]
fn dropping_an_input_does_not_declassify_private_executor_history() {
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
    let owner = owner_context("owner");
    let mut request = permission_intent();
    request["intent"]["required_projects"] = json!([{"project_id":"lineage","project_root":"/source","storage_authority":"original-store"}]);
    let saved = permission_call(&fixture.authority, &source, &owner, request).unwrap();
    let agent = saved["delegation"]["executor_agent_id"].as_str().unwrap();
    let execution = executor_context(&fixture, &fixture.authority, agent);
    let mut reduced = permission_intent();
    reduced["intent"]["operation_id"] = json!("remove-source");
    reduced["intent"]["expected_revision"] = saved["delegation"]["revision"].clone();
    let next = permission_call(&fixture.authority, &source, &owner, reduced).unwrap();
    activate(&fixture, &source, &next["delegation"]);
    *source.access.lock().unwrap() = SemanticProjectAccess::Denied;
    assert!(current_executor(&fixture, &source, &execution).is_err());
}
