use super::*;

fn background_context(export: &str) -> PluginInvocationContext {
    PluginInvocationContext::new(
        "recover",
        "builtin.agents",
        PluginInvocationClass::Background,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.agents", export)
}

fn preparation_call(
    authority: &KnowledgeAuthority,
    fixture: &AuthorityFixture,
    context: &PluginInvocationContext,
    request: Value,
) -> Result<Value, HostCapabilityError> {
    authority.invoke(
        "builtin.agents",
        json!({"operation":"executor_preparation","request":request}),
        fixture.persistence.as_ref(),
        context,
    )
}

fn future_observation(key: &str, value: Value) -> Value {
    json!({"table":"delegation_test_owner","row_key":key,"expected":value})
}

fn put_fact(fixture: &AuthorityFixture, key: &str, value: Value) {
    RelationalPersistence::execute(
        fixture.persistence.as_ref(),
        RelationalOperation::ApplyMutations {
            plugin_id: "builtin.agents".into(),
            mutations: vec![PluginDataMutation::Put {
                table_name: "delegation_test_owner".into(),
                row_key: key.into(),
                value,
            }],
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
}

fn prepare(fixture: &AuthorityFixture) -> (Value, PluginInvocationContext, Value) {
    fixture.bind("owner", 1, true).unwrap();
    let owner = owner_context("owner");
    let grant = permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner,
        permission_intent(),
    )
    .unwrap()["delegation"]
        .clone();
    let agent = grant["executor_agent_id"].as_str().unwrap();
    executor_context(fixture, &fixture.authority, agent);
    permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner,
        json!({"operation":"prepare_executor",
        "delegation_id":agent,"revision":grant["revision"]}),
    )
    .unwrap();
    let setup = owner.enter_plugin("builtin.agents", "configure_memory_executor");
    bind_executor(fixture, &fixture.authority, &setup, agent, "owner_control");
    let request = json!({"operation":"save", "agent_id":agent,"revision":grant["revision"],"preparation":{
        "digest":"original-intent","pending":[future_observation("claim", json!({"intent":"original"}))],
        "accepted":[future_observation("accepted", json!({"configuration":"original"}))]}});
    (grant, setup, request)
}

#[test]
fn frozen_executor_setup_survives_the_gap_before_agent_journal_creation() {
    let fixture = AuthorityFixture::new();
    let (grant, setup, request) = prepare(&fixture);
    let saved = preparation_call(&fixture.authority, &fixture, &setup, request.clone()).unwrap();
    let read = preparation_call(
        &fixture.authority,
        &fixture,
        &setup,
        json!({"operation":"get",
        "agent_id":grant["executor_agent_id"],"revision":grant["revision"]}),
    )
    .unwrap();
    assert_eq!(read, saved);
    let mut retry = request;
    retry["preparation"]["pending"][0]["expected"]["new_clock"] = json!(123);
    assert_eq!(
        preparation_call(&fixture.authority, &fixture, &setup, retry).unwrap(),
        saved
    );
    fixture.bind("owner", 1, false).unwrap();
    let reopened = KnowledgeAuthority::new(fixture.persistence.clone(), Arc::default());
    let background = background_context("agent_tick");
    let pending = preparation_call(
        &reopened,
        &fixture,
        &background,
        json!({"operation":"pending"}),
    )
    .unwrap();
    assert_eq!(pending["pending"][0]["preparation"], saved["preparation"]);
    let agent = grant["executor_agent_id"].as_str().unwrap();
    bind_executor(&fixture, &reopened, &background, agent, "owner_control");
    assert!(
        reopened
            .current(
                fixture.persistence.as_ref(),
                &background,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    put_fact(&fixture, "claim", json!({"intent":"original"}));
    reopened
        .current(
            fixture.persistence.as_ref(),
            &background,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let executor = executor_context(&fixture, &reopened, agent);
    assert!(
        reopened
            .current(
                fixture.persistence.as_ref(),
                &executor,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    put_fact(&fixture, "accepted", json!({"configuration":"original"}));
    let scope = reopened
        .current(
            fixture.persistence.as_ref(),
            &executor,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    assert!(scope.project_grants.is_empty());
    let finished = preparation_call(
        &reopened,
        &fixture,
        &background_context("agent_tick"),
        json!({"operation":"pending"}),
    )
    .unwrap();
    assert_eq!(finished["pending"], json!([]));
}

#[test]
fn executor_preparation_cannot_be_replaced_or_disclosed_by_foreground_agents() {
    let fixture = AuthorityFixture::new();
    let (grant, setup, request) = prepare(&fixture);
    preparation_call(&fixture.authority, &fixture, &setup, request.clone()).unwrap();
    let mut changed = request.clone();
    changed["preparation"]["digest"] = json!("different-intent");
    assert!(preparation_call(&fixture.authority, &fixture, &setup, changed).is_err());
    let foreground = PluginInvocationContext::new(
        "foreign",
        "native",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.agents", "get_agent");
    bind_executor(
        &fixture,
        &fixture.authority,
        &foreground,
        grant["executor_agent_id"].as_str().unwrap(),
        "owner_control",
    );
    assert!(preparation_call(&fixture.authority, &fixture, &foreground, request).is_err());
    assert!(
        preparation_call(
            &fixture.authority,
            &fixture,
            &foreground,
            json!({"operation":"get",
        "agent_id":grant["executor_agent_id"],"revision":grant["revision"]})
        )
        .is_err()
    );
    assert!(
        preparation_call(
            &fixture.authority,
            &fixture,
            &foreground,
            json!({"operation":"pending"})
        )
        .is_err()
    );
    let view = permission_call(
        &fixture.authority,
        fixture.persistence.as_ref(),
        &owner_context("owner"),
        json!({"operation":"get","document_id":"decision"}),
    )
    .unwrap();
    assert!(view["delegation"].get("executor_preparation").is_none());
}
