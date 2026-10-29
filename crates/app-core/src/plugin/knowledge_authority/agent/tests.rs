use super::*;
use lumvise_db_core::{LocalPersistence, RelationalOperation};
use lumvise_plugin_runtime::PluginInvocationClass;
use std::time::Duration;
mod disclosure;
mod run_restart;

struct AgentAuthorityFixture {
    store: Arc<LocalPersistence>,
    authority: KnowledgeAuthority,
    context: PluginInvocationContext,
}

impl AgentAuthorityFixture {
    fn new() -> Self {
        let store = Arc::new(LocalPersistence::in_memory().unwrap());
        let context = PluginInvocationContext::new(
            "agent-proof",
            AGENTS,
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        )
        .enter_plugin(AGENTS, "get_agent");
        let authority = KnowledgeAuthority::new(store.clone(), Arc::default());
        let fixture = Self {
            store,
            authority,
            context,
        };
        RelationalPersistence::execute(
            fixture.store.as_ref(),
            RelationalOperation::EnsurePluginDataTable {
                plugin_id: AGENTS.into(),
                table_name: "definitions".into(),
                schema: json!({}),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
        fixture
    }

    fn bind(&self, agent: &str, expected: Option<Value>) -> Result<(), HostCapabilityError> {
        let evidence = serde_json::from_value(json!({"agent_id":agent,"grants":[],"run_id":null,
            "observations":[{"table":"definitions","row_key":agent,"expected":expected}]}))
        .unwrap();
        self.authority.bind_agent(
            AGENTS,
            evidence,
            self.store.as_ref(),
            &self.context,
            &InvocationControl::sixty_seconds(),
        )
    }

    fn current(&self) -> Result<KnowledgeAccessContext, HostCapabilityError> {
        self.authority.current(
            self.store.as_ref(),
            &self.context,
            &InvocationControl::sixty_seconds(),
        )
    }

    fn replace(&self, agent: &str, value: Value) {
        RelationalPersistence::execute(
            self.store.as_ref(),
            RelationalOperation::ApplyMutations {
                plugin_id: AGENTS.into(),
                mutations: vec![lumvise_db_core::PluginDataMutation::Put {
                    table_name: "definitions".into(),
                    row_key: agent.into(),
                    value,
                }],
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    }
}

#[test]
fn owner_binding_is_frame_local_and_exact_observations_revoke_old_access() {
    let fixture = AgentAuthorityFixture::new();
    assert!(fixture.current().is_err());
    fixture.bind("scout", None).unwrap();
    let original = fixture.current().unwrap();
    assert_eq!(original.space.unwrap().project_root, "agent-space://scout");
    fixture.replace("scout", json!({"enabled":false}));
    assert!(fixture.current().is_err());
    assert!(fixture.bind("scout", None).is_err());
    fixture
        .bind("scout", Some(json!({"enabled":false})))
        .unwrap();
    let other_frame = fixture.context.clone().enter_plugin(AGENTS, "get_agent");
    assert!(
        fixture
            .authority
            .current(
                fixture.store.as_ref(),
                &other_frame,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
}

#[test]
fn nested_calls_inherit_one_agent_but_cannot_bind_another_owner() {
    let fixture = AgentAuthorityFixture::new();
    fixture.bind("scout", None).unwrap();
    let nested = fixture
        .context
        .clone()
        .enter_plugin("builtin.knowledge", "get_document");
    let scope = fixture
        .authority
        .current(
            fixture.store.as_ref(),
            &nested,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    assert_eq!(scope.principal_id, "scout");
    let evidence = serde_json::from_value(json!({"agent_id":"other","grants":[],"run_id":null,
        "observations":[{"table":"definitions","row_key":"other","expected":null}]}))
    .unwrap();
    assert!(
        fixture
            .authority
            .bind_agent(
                "builtin.knowledge",
                evidence,
                fixture.store.as_ref(),
                &nested,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    assert_eq!(fixture.current().unwrap().principal_id, "scout");
}

#[test]
fn a_scoped_run_requires_its_host_session_and_keeps_its_original_run_observation() {
    let mut fixture = AgentAuthorityFixture::new();
    fixture.context = fixture
        .context
        .with_route_identity("scout:run", Some("agent_run".into()));
    assert!(fixture.bind("scout", None).is_err());
    let evidence =
        serde_json::from_value(json!({"agent_id":"scout","grants":[],"run_id":"scout:run",
        "observations":[{"table":"definitions","row_key":"scout","expected":null},
        {"table":"definitions","row_key":"run","expected":null}]}))
        .unwrap();
    fixture
        .authority
        .bind_agent(
            AGENTS,
            evidence,
            fixture.store.as_ref(),
            &fixture.context,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    fixture.bind("scout", None).unwrap();
    fixture.replace("run", json!({"status":"completed"}));
    assert!(fixture.current().is_err());
    assert!(fixture.bind("scout", None).is_err());
}

#[test]
fn an_autonomous_run_cannot_bind_owner_control_purpose() {
    let mut fixture = AgentAuthorityFixture::new();
    fixture.context = fixture
        .context
        .with_route_identity("scout:run", Some("agent_run".into()));
    let evidence = serde_json::from_value(json!({"agent_id":"scout","grants":[],"run_id":"scout:run",
        "purpose":"owner_control", "observations":[{"table":"definitions","row_key":"scout","expected":null},
        {"table":"definitions","row_key":"run","expected":null}]})).unwrap();
    assert!(
        fixture
            .authority
            .bind_agent(
                AGENTS,
                evidence,
                fixture.store.as_ref(),
                &fixture.context,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
    assert!(fixture.current().is_err());
}
