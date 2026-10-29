use super::*;
use lumvise_db_core::{PersistenceResult, SemanticProjectAccess};

struct FakeProjectMembership {
    persistence: Arc<LocalPersistence>,
    remote: Mutex<SemanticProjectAccess>,
    unavailable: std::sync::atomic::AtomicBool,
}

impl SemanticPersistence for FakeProjectMembership {
    fn readiness(&self) -> PersistenceResult<lumvise_db_core::SemanticReadiness> {
        SemanticPersistence::readiness(self.persistence.as_ref())
    }

    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        match operation {
            SemanticOperation::ProjectAccess { project_root } if project_root == "/remote" => {
                if self.unavailable.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(lumvise_db_core::DbError::invalid_value(
                        "/remote",
                        "reachable project membership",
                    ));
                }
                Ok(SemanticResult::ProjectAccess(
                    self.remote.lock().unwrap().clone(),
                ))
            }
            other => SemanticPersistence::execute(self.persistence.as_ref(), other, control),
        }
    }
}

fn remote(fixture: &AuthorityFixture, writable: bool) -> FakeProjectMembership {
    fixture.seed_project("/remote");
    FakeProjectMembership {
        persistence: fixture.persistence.clone(),
        remote: Mutex::new(SemanticProjectAccess::Member {
            project_id: "remote-lineage".into(),
            storage_authority: "https://app.lumvise.com".into(),
            writable,
        }),
        unavailable: std::sync::atomic::AtomicBool::new(false),
    }
}

#[test]
fn unavailable_unrelated_project_does_not_block_independent_assistant_memory() {
    let fixture = AuthorityFixture::new();
    fixture.bind("independent", 1, true).unwrap();
    let source = remote(&fixture, false);
    source
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let scope = fixture
        .authority
        .current(
            &source,
            &invocation("independent", 1),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    assert!(scope.project_grants.is_empty());
    assert!(scope.space.is_some());
}

#[test]
fn membership_discovery_preserves_invocation_cancellation() {
    let fixture = AuthorityFixture::new();
    let source = remote(&fixture, false);
    source
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let control = InvocationControl::sixty_seconds();
    control.cancel();
    assert!(project_grants(&source, "local-profile", &control).is_err());
}

#[test]
fn unavailable_supplied_project_still_invalidates_the_conversation() {
    let fixture = AuthorityFixture::new();
    fixture.bind("dependent", 1, true).unwrap();
    let source = remote(&fixture, false);
    let context = invocation("dependent", 1);
    let control = InvocationControl::sixty_seconds();
    fixture
        .authority
        .authorize_source(&context, &source, "/remote", false, &control)
        .unwrap();
    source
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        fixture
            .authority
            .current(&source, &context, &control)
            .is_err()
    );
}

#[test]
fn human_project_memory_inspection_does_not_elevate_read_only_or_revoked_membership() {
    let fixture = AuthorityFixture::new();
    let source = remote(&fixture, false);
    let context = PluginInvocationContext::new(
        "inspection",
        "desktop-renderer-command",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_human_interaction()
    .enter_plugin("builtin.knowledge", "inspect_document_space");
    let inspected = fixture
        .authority
        .invoke(
            "builtin.knowledge",
            json!({"operation":"inspect_space", "selection":{
        "space_kind":"mcp_project", "project_root":"/remote"}}),
            &source,
            &context,
        )
        .unwrap();
    assert_eq!(inspected["writable"], false);
    assert!(fixture.authority.invoke("builtin.knowledge", json!({"operation":"citation","request":{
        "operation":"intent","project_root":inspected["space"]["project_root"],"document_id":"d","publication_id":"p"}}), &source, &context).is_err());
    *source.remote.lock().unwrap() = SemanticProjectAccess::Denied;
    assert!(
        fixture
            .authority
            .current(&source, &context, &InvocationControl::sixty_seconds())
            .is_err()
    );
}

#[test]
fn remote_membership_is_fresh_and_keeps_its_authority_and_write_permission() {
    let fixture = AuthorityFixture::new();
    fixture.seed_project("/local");
    let source = remote(&fixture, false);
    let control = InvocationControl::sixty_seconds();
    let grants = project_grants(&source, "local-profile", &control).unwrap();
    let remote = grants
        .iter()
        .find(|grant| grant.project.project_root == "/remote")
        .unwrap();
    assert_eq!(remote.project.storage_authority, "https://app.lumvise.com");
    assert!(!remote.writable);
    let local = grants
        .iter()
        .find(|grant| grant.project.project_root == "/local")
        .unwrap();
    assert_eq!(local.project.storage_authority, "local-profile");
    assert!(local.writable);
    *source.remote.lock().unwrap() = SemanticProjectAccess::Denied;
    assert_eq!(
        project_grants(&source, "local-profile", &control).unwrap(),
        vec![local.clone()]
    );
    assert!(resolve_project(&source, "/remote", "local-profile", &control).is_err());
}

#[test]
fn revoking_a_supplied_project_invalidates_the_bound_conversation() {
    let fixture = AuthorityFixture::new();
    fixture.bind("conversation", 1, true).unwrap();
    let source = remote(&fixture, false);
    let context = invocation("conversation", 1);
    let control = InvocationControl::sixty_seconds();
    fixture
        .authority
        .authorize_source(&context, &source, "/remote", false, &control)
        .unwrap();
    assert!(
        fixture
            .authority
            .authorize_source(&context, &source, "/remote", true, &control)
            .is_err()
    );
    *source.remote.lock().unwrap() = SemanticProjectAccess::Denied;
    assert!(
        fixture
            .authority
            .current(&source, &context, &control)
            .is_err()
    );
}

#[test]
fn mcp_can_bind_an_unindexed_local_project_without_a_catalog_grant() {
    let fixture = AuthorityFixture::new();
    let context = fixture.mcp_invocation("empty-project", "/empty");
    let scope = fixture.current_mcp(&context).unwrap();
    assert_eq!(
        scope["project_grants"][0]["project"]["project_root"],
        "/empty"
    );
    assert_eq!(scope["project_grants"][0]["writable"], true);
}

#[test]
fn read_only_mcp_membership_cannot_publish_private_memory_or_create_intents() {
    let fixture = AuthorityFixture::new();
    let source = remote(&fixture, false);
    let mut context = fixture.mcp_invocation("member", "/remote");
    let binding = fixture
        .authority
        .mcp_membership
        .capture_project(
            "member",
            "/remote",
            "remote-lineage",
            Some("https://app.lumvise.com"),
        )
        .unwrap();
    context = context
        .with_scope_epoch(Some(binding))
        .enter_plugin("builtin.knowledge", "create_document");
    let control = InvocationControl::sixty_seconds();
    let scope = fixture
        .authority
        .current(&source, &context, &control)
        .unwrap();
    let root = scope.space.unwrap().project_root;
    let hook = fixture
        .authority
        .invoke(
            "builtin.knowledge",
            json!({"operation":"delegation","request":{
        "operation":"get","document_id":"decision"}}),
            &source,
            &context,
        )
        .unwrap();
    assert_eq!(hook["writable"], false);
    assert_eq!(hook["project_root"], root);
    assert_eq!(hook["delegation"], Value::Null);
    let call = fixture.authority.invoke(
        "builtin.knowledge",
        json!({"operation":"citation", "request":{
        "operation":"intent", "project_root":root, "document_id":"d", "publication_id":"write"}}),
        &source,
        &context,
    );
    assert!(call.is_err());
    let access = semantic_access::KnowledgeSemanticAccess {
        semantic: &source,
        authority: &fixture.authority,
        context: &context,
        control: &control,
    };
    // An owned space is not a write grant; the exact publication body is irrelevant to this gate.
    let publication = lumvise_db_core::SemanticRevisionPublication {
        operation_id: "write".into(),
        project_root: root,
        project_id: "lineage".into(),
        selection_id: "working".into(),
        source_context: "knowledge".into(),
        producer_base: None,
        expected: Default::default(),
        structure: None,
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    };
    assert!(
        access
            .execute(SemanticOperation::PublishRevisions(publication))
            .is_err()
    );
}

fn inbox_context(human: bool) -> PluginInvocationContext {
    let context = PluginInvocationContext::new(
        "inbox-access",
        "desktop-renderer-command",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    );
    let context = if human {
        context.with_human_interaction()
    } else {
        context
    };
    context.enter_plugin("builtin.assistant", "list_assistant_inbox")
}

#[test]
fn idle_native_inbox_resolves_exact_membership_without_creating_a_session() {
    let fixture = AuthorityFixture::new();
    let source = remote(&fixture, false);
    let request = json!({"operation":"project_access","project_root":"/remote"});
    let read = fixture
        .authority
        .invoke(ASSISTANT, request.clone(), &source, &inbox_context(true))
        .unwrap();
    assert_eq!(read["project"]["project_id"], "remote-lineage");
    assert_eq!(
        read["project"]["storage_authority"],
        "https://app.lumvise.com"
    );
    assert_eq!(read["writable"], false);
    *source.remote.lock().unwrap() = SemanticProjectAccess::Denied;
    assert!(
        fixture
            .authority
            .invoke(ASSISTANT, request, &source, &inbox_context(true))
            .is_err()
    );
}

#[test]
fn an_unbound_inbox_cannot_claim_human_project_authority() {
    let fixture = AuthorityFixture::new();
    let source = remote(&fixture, true);
    assert!(
        fixture
            .authority
            .invoke(
                ASSISTANT,
                json!({"operation":"project_access","project_root":"/remote"}),
                &source,
                &inbox_context(false)
            )
            .is_err()
    );
}

#[test]
fn inbox_mcp_access_stays_with_its_original_project_scope() {
    let fixture = AuthorityFixture::new();
    fixture.seed_project("/other");
    let context = fixture
        .mcp_invocation("inbox", "/selected")
        .enter_plugin(ASSISTANT, "receive_inbox_message");
    let own = json!({"operation":"project_access","project_root":"/selected"});
    assert_eq!(
        fixture
            .authority
            .invoke(ASSISTANT, own, fixture.persistence.as_ref(), &context)
            .unwrap()["project"]["project_root"],
        "/selected"
    );
    assert!(
        fixture
            .authority
            .invoke(
                ASSISTANT,
                json!({"operation":"project_access","project_root":"/other"}),
                fixture.persistence.as_ref(),
                &context
            )
            .is_err()
    );
}
