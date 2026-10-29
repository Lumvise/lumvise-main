use super::*;

fn native_inspection() -> PluginInvocationContext {
    PluginInvocationContext::new(
        "inspection",
        "desktop-renderer-command",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_human_interaction()
    .enter_plugin("builtin.knowledge", "inspect_document_space")
}

fn bind(
    fixture: &AuthorityFixture,
    context: &PluginInvocationContext,
    selection: Value,
) -> Result<Value, HostCapabilityError> {
    fixture.authority.invoke(
        "builtin.knowledge",
        json!({"operation":"inspect_space", "selection":selection}),
        fixture.persistence.as_ref(),
        context,
    )
}

#[test]
fn only_native_human_inspection_can_select_an_owned_space() {
    let fixture = AuthorityFixture::new();
    let native = native_inspection();
    let selected = bind(&fixture, &native, json!({"space_kind":"assistant"})).unwrap();
    fixture.bind("conversation", 1, true).unwrap();
    assert_eq!(
        selected["space"],
        fixture.scope("conversation", 1).unwrap()["space"]
    );
    assert_eq!(selected["writable"], true);
    let spoofed = PluginInvocationContext::new(
        "inspection",
        "desktop-renderer-command",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.knowledge", "inspect_document_space");
    for denied in [
        spoofed,
        native
            .clone()
            .with_route_identity("model", Some("assistant_session".into())),
        native.clone().enter_plugin("builtin.agents", "get_agent"),
    ] {
        assert!(bind(&fixture, &denied, json!({"space_kind":"assistant"})).is_err());
    }
}

#[test]
fn inspection_is_bound_to_one_runtime_frame_and_nested_calls_keep_it() {
    let fixture = AuthorityFixture::new();
    let native = native_inspection();
    let selected = bind(
        &fixture,
        &native,
        json!({"space_kind":"agent", "agent_id":"scout"}),
    )
    .unwrap();
    let nested = native
        .clone()
        .enter_plugin("builtin.semantic", "ingest_index_batch");
    let current = fixture
        .authority
        .current(
            fixture.persistence.as_ref(),
            &nested,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    assert_eq!(current.principal_kind, KnowledgePrincipalKind::Human);
    assert_eq!(json!(current.space), selected["space"]);
    assert!(
        fixture
            .authority
            .current(
                fixture.persistence.as_ref(),
                &native_inspection(),
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    assert!(
        bind(
            &fixture,
            &native_inspection(),
            json!({"space_kind":"agent", "agent_id":"../other"})
        )
        .is_err()
    );
}

#[test]
fn project_memory_inspection_resolves_the_same_project_shared_space() {
    let fixture = AuthorityFixture::new();
    fixture.seed_project("/project");
    let mcp = fixture.mcp_invocation("connection", "/project");
    let original = fixture.current_mcp(&mcp).unwrap();
    let human = bind(
        &fixture,
        &native_inspection(),
        json!({"space_kind":"mcp_project","project_root":"/project"}),
    )
    .unwrap();
    assert_eq!(human["space"], original["space"]);
}

#[test]
fn copied_lineages_in_different_storage_domains_have_distinct_shared_spaces() {
    let project = KnowledgeProjectIdentity {
        project_root: "/project".into(),
        project_id: "copied-lineage".into(),
        storage_authority: "hub-a".into(),
    };
    let first = mcp::mcp_space("profile", &project);
    let mut moved = project.clone();
    moved.project_root = "/new-checkout".into();
    assert_eq!(
        first.project_root,
        mcp::mcp_space("profile", &moved).project_root
    );
    let mut foreign = project;
    foreign.storage_authority = "hub-b".into();
    let second = mcp::mcp_space("profile", &foreign);
    assert_ne!(first.space_id, second.space_id);
    assert_ne!(first.project_root, second.project_root);
}
