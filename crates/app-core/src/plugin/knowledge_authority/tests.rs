use super::*;
use lumvise_db_core::LocalPersistence;
use lumvise_plugin_runtime::PluginInvocationClass;
use std::time::{Duration, Instant};
mod citations;
mod delegation;
mod inspection;
mod project_access;

struct AuthorityFixture {
    persistence: Arc<LocalPersistence>,
    authority: KnowledgeAuthority,
}

impl AuthorityFixture {
    fn bind_agent_owner(
        &self,
        context: &PluginInvocationContext,
        purpose: &str,
    ) -> Result<Value, HostCapabilityError> {
        RelationalPersistence::execute(
            self.persistence.as_ref(),
            lumvise_db_core::RelationalOperation::EnsurePluginDataTable {
                plugin_id: "builtin.agents".into(),
                table_name: "observed_owner".into(),
                schema: json!({}),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
        self.authority.invoke(
            "builtin.agents",
            json!({"operation":"bind_agent","binding":{
                "agent_id":"scout","grants":[],"purpose":purpose,"run_id":null,
                "observations":[{"table":"observed_owner","row_key":"scout","expected":null}]
            }}),
            self.persistence.as_ref(),
            context,
        )
    }

    fn new() -> Self {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let authority = KnowledgeAuthority::new(persistence.clone(), Arc::default());
        Self {
            persistence,
            authority,
        }
    }

    fn bind(&self, session: &str, epoch: u64, active: bool) -> Result<Value, HostCapabilityError> {
        self.authority.invoke(
            ASSISTANT,
            json!({"operation":"bind_assistant",
            "session_id":session, "session_epoch":epoch, "active":active}),
            self.persistence.as_ref(),
            &invocation(session, epoch),
        )
    }

    fn scope(&self, session: &str, epoch: u64) -> Result<Value, HostCapabilityError> {
        self.authority.invoke(
            "builtin.knowledge",
            json!({"operation":"current"}),
            self.persistence.as_ref(),
            &invocation(session, epoch),
        )
    }

    fn seed_project(&self, root: &str) -> String {
        let id = format!("file:{root}");
        let element = serde_json::from_value(json!({"project_root":root, "semantic_element_id":id,
            "semantic_source_id":format!("source:{root}"), "path":"file.rs", "element_kind":"file",
            "name":"file", "content_fingerprint":"fp1:0000000000000001:file", "lifecycle":"active", "metadata":{}})).unwrap();
        SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::SyncStructure {
                project_root: root.into(),
                elements: vec![element],
                relationships: vec![],
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
        id
    }

    fn mcp_invocation(&self, connection: &str, root: &str) -> PluginInvocationContext {
        self.authority
            .mcp_membership
            .register(connection, root)
            .unwrap();
        let identity = resolve_project(
            self.persistence.as_ref(),
            root,
            "fixture-authority",
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
        let epoch = self
            .authority
            .mcp_membership
            .capture_project(connection, root, &identity.project_id, None)
            .unwrap();
        PluginInvocationContext::new(
            "mcp-proof",
            "local-mcp",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        )
        .with_route_identity(
            connection,
            Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE.into()),
        )
        .with_scope_epoch(Some(epoch))
    }

    fn current_mcp(&self, context: &PluginInvocationContext) -> Result<Value, HostCapabilityError> {
        self.authority.invoke(
            "builtin.knowledge",
            json!({"operation":"current"}),
            self.persistence.as_ref(),
            context,
        )
    }
}

#[test]
fn agent_management_keeps_origin_rights_and_rejects_foreign_grant_assignment() {
    let fixture = AuthorityFixture::new();
    fixture.seed_project("/p");
    fixture.seed_project("/q");
    let context = fixture
        .mcp_invocation("connection", "/p")
        .enter_plugin("builtin.agents", "define_agent");
    fixture.bind_agent_owner(&context, "owner_control").unwrap();
    let scope = fixture.current_mcp(&context).unwrap();
    assert_eq!(scope["principal_kind"], "agent");
    assert_eq!(scope["project_grants"].as_array().unwrap().len(), 1);
    let assign = |root| {
        fixture.authority.invoke(
            "builtin.agents",
            json!({"operation":"authorize_agent_grants",
        "grants":[{"project_root":root,"writable":false}]}),
            fixture.persistence.as_ref(),
            &context,
        )
    };
    assert!(assign("/p").is_ok());
    assert!(assign("/q").is_err());
    assert!(assign("agent-space://other").is_err());
    fixture.bind_agent_owner(&context, "autonomous").unwrap();
    assert!(fixture.current_mcp(&context).is_err());
    assert!(assign("/p").is_err());
}

fn invocation(session: &str, epoch: u64) -> PluginInvocationContext {
    PluginInvocationContext::new(
        "request",
        ASSISTANT,
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_route_identity(session, Some("assistant_session".into()))
    .with_scope_epoch(Some(epoch))
}

#[test]
fn different_conversations_share_durable_identity_but_need_individual_live_bindings() {
    let fixture = AuthorityFixture::new();
    assert!(fixture.scope("a", 1).is_err());
    fixture.bind("a", 1, true).unwrap();
    fixture.bind("b", 2, true).unwrap();
    let a = fixture.scope("a", 1).unwrap();
    assert_eq!(a, fixture.scope("b", 2).unwrap());
    fixture.bind("a", 1, false).unwrap();
    assert!(fixture.scope("a", 1).is_err());
    assert_eq!(a, fixture.scope("b", 2).unwrap());
}

#[test]
fn restart_keeps_profile_and_rejects_previous_process_session_until_rebound() {
    let mut fixture = AuthorityFixture::new();
    fixture.bind("a", 1, true).unwrap();
    let original = fixture.scope("a", 1).unwrap();
    fixture.authority = KnowledgeAuthority::new(fixture.persistence.clone(), Arc::default());
    assert!(fixture.scope("a", 1).is_err());
    fixture.bind("b", 2, true).unwrap();
    assert_eq!(original, fixture.scope("b", 2).unwrap());
}

#[test]
fn epoch_and_signed_owner_are_required_and_terminal_binding_cannot_be_revived() {
    let fixture = AuthorityFixture::new();
    fixture.bind("a", 3, true).unwrap();
    assert!(fixture.scope("a", 2).is_err());
    assert!(fixture.bind("a", 2, true).is_err());
    fixture.bind("a", 3, false).unwrap();
    assert!(fixture.bind("a", 3, true).is_err());
    fixture.bind("a", 4, true).unwrap();
    let input =
        json!({"operation":"bind_assistant", "session_id":"a", "session_epoch":4, "active":true});
    assert!(
        fixture
            .authority
            .invoke(
                "builtin.knowledge",
                input,
                fixture.persistence.as_ref(),
                &invocation("a", 4)
            )
            .is_err()
    );
}

#[test]
fn model_owner_fields_cannot_amend_authority_and_nested_context_keeps_epoch() {
    let fixture = AuthorityFixture::new();
    fixture.bind("a", 2, true).unwrap();
    let context = invocation("a", 2).clone();
    assert_eq!(context.scope_epoch(), Some(2));
    let input = json!({"operation":"current", "principal_id":"other"});
    assert!(
        fixture
            .authority
            .invoke(
                "builtin.knowledge",
                input,
                fixture.persistence.as_ref(),
                &context
            )
            .is_err()
    );
}

#[test]
fn concurrent_hosts_resolve_one_profile_without_overwriting_it() {
    let fixture = AuthorityFixture::new();
    let other = KnowledgeAuthority::new(fixture.persistence.clone(), Arc::default());
    let control = InvocationControl::sixty_seconds();
    let identities = std::thread::scope(|scope| {
        let left = scope.spawn(|| fixture.authority.identity(&control).unwrap());
        let right = scope.spawn(|| other.identity(&control).unwrap());
        (left.join().unwrap(), right.join().unwrap())
    });
    assert_eq!(identities.0, identities.1);
    assert!(!identities.0.is_empty());
}

#[test]
fn source_reads_restrict_the_generation_and_revocation_requires_a_fresh_conversation() {
    let fixture = AuthorityFixture::new();
    let control = InvocationControl::sixty_seconds();
    let element_id = fixture.seed_project("/repo");
    fixture.bind("reader", 1, true).unwrap();
    fixture
        .authority
        .authorize_source(
            &invocation("reader", 1),
            fixture.persistence.as_ref(),
            "/repo",
            false,
            &control,
        )
        .unwrap();
    let scope = fixture.scope("reader", 1).unwrap();
    assert_eq!(scope["source_projects"][0]["project_root"], "/repo");
    SemanticPersistence::execute(
        fixture.persistence.as_ref(),
        SemanticOperation::RemoveElement {
            semantic_element_id: element_id,
        },
        &control,
    )
    .unwrap();
    assert!(fixture.scope("reader", 1).is_err());
    fixture.bind("fresh", 2, true).unwrap();
    let fresh = fixture.scope("fresh", 2).unwrap();
    assert_eq!(scope["space"], fresh["space"]);
    assert_eq!(fresh["source_projects"], json!([]));
}

#[test]
fn mcp_connections_share_the_canonical_project_space_and_reconnect_keeps_it() {
    let fixture = AuthorityFixture::new();
    fixture.seed_project("/p");
    fixture.seed_project("/q");
    let first = fixture.mcp_invocation("one", "/p");
    let second = fixture.mcp_invocation("two", "/p");
    let unrelated = fixture.mcp_invocation("three", "/q");
    let scope = fixture.current_mcp(&first).unwrap();
    assert_eq!(scope, fixture.current_mcp(&second).unwrap());
    assert_ne!(
        scope["space"],
        fixture.current_mcp(&unrelated).unwrap()["space"]
    );
    assert_eq!(scope["principal_kind"], "mcp");
    assert_eq!(scope["project_grants"].as_array().unwrap().len(), 1);
    assert_eq!(scope["source_projects"][0]["project_root"], "/p");
    fixture
        .authority
        .mcp_membership
        .heartbeat("one", "/p", "unavailable")
        .unwrap();
    assert!(fixture.current_mcp(&first).is_err());
    let reconnected = fixture.mcp_invocation("reconnected", "/p");
    assert_eq!(scope, fixture.current_mcp(&reconnected).unwrap());
}

#[test]
fn mcp_authority_rejects_rebound_or_retired_membership_without_using_graph_content_as_a_grant() {
    let fixture = AuthorityFixture::new();
    let element = fixture.seed_project("/p");
    fixture.seed_project("/q");
    let original = fixture.mcp_invocation("a", "/p");
    let switched = fixture.mcp_invocation("a", "/q");
    assert!(fixture.current_mcp(&original).is_err());
    assert!(fixture.current_mcp(&switched).is_ok());
    let rebound_epoch = fixture
        .authority
        .mcp_membership
        .capture_project("a", "/q", "another-lineage", None)
        .unwrap();
    assert!(
        fixture
            .current_mcp(&switched.with_scope_epoch(Some(rebound_epoch)))
            .is_err()
    );
    let removed = fixture.mcp_invocation("b", "/p");
    SemanticPersistence::execute(
        fixture.persistence.as_ref(),
        SemanticOperation::RemoveElement {
            semantic_element_id: element,
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    assert!(fixture.current_mcp(&removed).is_ok());
    fixture
        .authority
        .mcp_membership
        .heartbeat("b", "/p", "unavailable")
        .unwrap();
    assert!(fixture.current_mcp(&removed).is_err());
}
