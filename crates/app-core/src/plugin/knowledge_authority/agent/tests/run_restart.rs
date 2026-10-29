use super::*;

fn bind_run(
    fixture: &AgentAuthorityFixture,
    run: &str,
    grants: Value,
) -> Result<(), HostCapabilityError> {
    let context = fixture
        .context
        .clone()
        .with_route_identity(run, Some("agent_run".into()));
    let evidence = serde_json::from_value(json!({"agent_id":"scout","grants":grants,"run_id":run,
        "observations":[{"table":"definitions","row_key":"scout","expected":null},
        {"table":"definitions","row_key":run,"expected":null}]}))
    .unwrap();
    fixture.authority.bind_agent(
        AGENTS,
        evidence,
        fixture.store.as_ref(),
        &context,
        &InvocationControl::sixty_seconds(),
    )
}

#[test]
fn host_restart_cannot_erase_revoked_context_from_a_running_agent() {
    let mut fixture = AgentAuthorityFixture::new();
    let element = serde_json::from_value(
        json!({"project_root":"/source","semantic_element_id":"source",
        "semantic_source_id":"fixture","path":"source.rs","element_kind":"file",
        "name":"source","lifecycle":"active","metadata":{}}),
    )
    .unwrap();
    SemanticPersistence::execute(
        fixture.store.as_ref(),
        SemanticOperation::SyncStructure {
            project_root: "/source".into(),
            elements: vec![element],
            relationships: vec![],
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    let grants = json!([{"project_root":"/source","writable":false}]);
    bind_run(&fixture, "scout:run", grants.clone()).unwrap();
    fixture.authority = KnowledgeAuthority::new(fixture.store.clone(), Arc::default());
    bind_run(&fixture, "scout:run", grants).unwrap();
    assert!(bind_run(&fixture, "scout:run", json!([])).is_err());
    bind_run(&fixture, "scout:new-run", json!([])).unwrap();
}
