use super::*;
use crate::{AppCore, ConnectorGrantMaintainer};
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityRequest, PluginInvocationClass, PluginInvocationContext,
    PluginMcpOrigin,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct FakeNativeMaintainer;
impl ConnectorGrantMaintainer for FakeNativeMaintainer {
    fn require_maintainer(&self, _: &KnowledgeProjectIdentity) -> Result<(), String> {
        Ok(())
    }
}

fn seed_scope_project(app: &AppCore, root: &str) {
    let element = serde_json::from_value(json!({"project_root":root,"semantic_element_id":format!("file:{root}"),
        "semantic_source_id":format!("source:{root}"),"path":"file.rs","element_kind":"file","name":"scope fixture",
        "lifecycle":"active","metadata":{}})).unwrap();
    app.semantic
        .execute(
            lumvise_db_core::SemanticOperation::SyncStructure {
                project_root: root.into(),
                elements: vec![element],
                relationships: vec![],
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
}

fn bind_scope(app: &AppCore, session: &str, origin: Option<PluginMcpOrigin>) {
    let context = PluginInvocationContext::new(
        "bind",
        "builtin.assistant",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.assistant", "start_assistant_session");
    let context = if let Some(origin) = origin {
        context.with_mcp_origin(origin)
    } else {
        context
    };
    app.default_plugin_host_capability_broker().unwrap().invoke_controlled(HostCapabilityRequest {
        plugin_id: "builtin.assistant".into(), invocation_id: "bind".into(), call_id: "scope".into(),
        capability_id: "runtime.knowledge_scope".into(), required_version: "1".into(),
        input: json!({"operation":"bind_assistant","session_id":session,"session_epoch":1,"active":true,"project_root":"/repo"}),
    }, &context).unwrap();
}

#[test]
fn scoped_connector_can_continue_its_own_session_but_cannot_adopt_a_native_session_or_another_project()
 {
    let app = AppCore::in_memory().unwrap();
    seed_scope_project(&app, "/repo");
    seed_scope_project(&app, "/other");
    let (id, initial) = connected(&app);
    let control = InvocationControl::sixty_seconds();
    app.connector_authority()
        .authorize_connector_call(app.semantic.as_ref(), &initial, &control)
        .unwrap_err();
    app.decide_connector_knowledge_grant(
        "/repo",
        &id,
        1,
        ConnectorGrantState::Granted,
        &FakeNativeMaintainer,
    )
    .unwrap();
    bind_scope(&app, "connector-session", initial.mcp_origin().cloned());
    bind_scope(&app, "native-session", None);
    let scoped = |session: &str| {
        PluginInvocationContext::new(
            "scoped-write",
            "builtin.assistant",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        )
        .with_route_identity(session, Some("assistant_session".into()))
        .with_scope_epoch(Some(1))
        .with_mcp_origin(initial.mcp_origin().unwrap().clone())
    };
    let authorize = |session: &str, root: &str| {
        app.connector_authority().authorize_connector_request(
            app.semantic.as_ref(),
            &scoped(session),
            "builtin.knowledge",
            "document.create",
            &json!({"project_root":root}),
            &control,
        )
    };
    authorize("connector-session", "/repo").unwrap();
    assert!(authorize("native-session", "/repo").is_err());
    assert!(authorize("connector-session", "/other").is_err());
    authorize("connector-session", "/repo").unwrap();
}

#[test]
fn read_aliases_do_not_request_write_access_and_scoped_write_aliases_use_the_same_grant() {
    let app = AppCore::in_memory().unwrap();
    let (id, context) = connected(&app);
    let authority = app.connector_authority();
    let control = InvocationControl::sixty_seconds();
    for export in [
        "knowledge.get",
        "knowledge.search",
        "document.get",
        "get_artifact_write_receipt",
    ] {
        authority
            .authorize_connector_request(
                app.semantic.as_ref(),
                &context,
                "builtin.knowledge",
                export,
                &json!({}),
                &control,
            )
            .unwrap();
    }
    assert!(
        app.connector_knowledge_grants("/repo", &FakeNativeMaintainer)
            .unwrap()
            .is_empty()
    );
    authority
        .authorize_connector_call(app.semantic.as_ref(), &context, &control)
        .unwrap_err();
    app.decide_connector_knowledge_grant(
        "/repo",
        &id,
        1,
        ConnectorGrantState::Granted,
        &FakeNativeMaintainer,
    )
    .unwrap();
    for export in [
        "knowledge.create",
        "knowledge.update",
        "document.create",
        "document.update",
    ] {
        authority
            .authorize_connector_request(
                app.semantic.as_ref(),
                &context,
                "builtin.knowledge",
                export,
                &json!({"project_root":"/repo"}),
                &control,
            )
            .unwrap();
    }
    assert!(
        authority
            .authorize_connector_request(
                app.semantic.as_ref(),
                &context,
                "builtin.knowledge",
                "delete_document",
                &json!({"project_root":"/repo"}),
                &control
            )
            .is_err()
    );
}

fn connected(app: &AppCore) -> (String, PluginInvocationContext) {
    let control = InvocationControl::sixty_seconds();
    let authority = app.connector_authority();
    let id = authority
        .register_connector(
            &ConnectorCredential("ab".repeat(32)),
            "Agent installation",
            &control,
        )
        .unwrap()
        .connector_id;
    app.mcp_session_membership
        .register_connector("connection", "/repo", Some(&id))
        .unwrap();
    let project = super::super::project_access::resolve_project(
        app.semantic.as_ref(),
        "/repo",
        &authority.identity(&control).unwrap(),
        &control,
    )
    .unwrap();
    let epoch = app
        .mcp_session_membership
        .capture_project("connection", "/repo", &project.project_id, None)
        .unwrap();
    let origin = PluginMcpOrigin {
        connector_id: Some(id.clone()),
        connection_id: "connection".into(),
        project_epoch: Some(epoch),
    };
    let context = PluginInvocationContext::new(
        "write",
        "agent",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_route_identity(
        "connection",
        Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE.into()),
    )
    .with_scope_epoch(Some(epoch))
    .with_mcp_origin(origin)
    .enter_plugin("builtin.knowledge", "create_document");
    (id, context)
}

fn storage_call(
    app: &AppCore,
    context: &PluginInvocationContext,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    app.default_plugin_host_capability_broker()
        .unwrap()
        .invoke_controlled(
            HostCapabilityRequest {
                plugin_id: "builtin.knowledge".into(),
                capability_id: "storage.plugin".into(),
                required_version: "1".into(),
                invocation_id: "probe".into(),
                call_id: "write".into(),
                input,
            },
            context,
        )
}

fn row(app: &AppCore) -> Value {
    let output = app
        .relational
        .execute(
            lumvise_db_core::RelationalOperation::GetPluginData {
                plugin_id: "builtin.knowledge".into(),
                table_name: "write_probe".into(),
                row_key: "body".into(),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let lumvise_db_core::RelationalResult::PluginDataRow(Some(row)) = output else {
        panic!("expected persisted probe row")
    };
    row.value
}

#[test]
fn real_host_boundary_requires_approval_and_rechecks_revocation_on_the_same_connection() {
    let app = AppCore::in_memory().unwrap();
    let (id, external) = connected(&app);
    let native = PluginInvocationContext::new(
        "native",
        "native",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    );
    storage_call(
        &app,
        &native,
        json!({"operation":"ensure_table","table_name":"write_probe","schema":{}}),
    )
    .unwrap();
    let put = |value: &str| json!({"operation":"put_row","table_name":"write_probe","row_key":"body","value":value});
    storage_call(&app, &native, put("initial")).unwrap();
    app.connector_authority()
        .authorize_connector_request(
            app.semantic.as_ref(),
            &external,
            "builtin.knowledge",
            "create_document",
            &json!({"project_root":"/repo"}),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err();
    assert!(storage_call(&app, &external, put("unapproved")).is_err());
    assert_eq!(
        app.drain_frontend_actions().unwrap(),
        vec![json!({"action":"connector.request_review","payload":{"project_root":"/repo"}})]
    );
    assert_eq!(row(&app), json!("initial"));
    app.decide_connector_knowledge_grant(
        "/repo",
        &id,
        1,
        ConnectorGrantState::Granted,
        &FakeNativeMaintainer,
    )
    .unwrap();
    storage_call(&app, &external, put("approved")).unwrap();
    assert_eq!(row(&app), json!("approved"));
    app.decide_connector_knowledge_grant(
        "/repo",
        &id,
        2,
        ConnectorGrantState::Revoked,
        &FakeNativeMaintainer,
    )
    .unwrap();
    assert!(storage_call(&app, &external, put("revoked")).is_err());
    assert_eq!(row(&app), json!("approved"));
}

#[test]
fn raw_semantic_writes_and_retired_project_epochs_do_not_inherit_a_knowledge_grant() {
    let app = AppCore::in_memory().unwrap();
    let (id, context) = connected(&app);
    let authority = app.connector_authority();
    authority
        .authorize_connector_call(
            app.semantic.as_ref(),
            &context,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err();
    app.decide_connector_knowledge_grant(
        "/repo",
        &id,
        1,
        ConnectorGrantState::Granted,
        &FakeNativeMaintainer,
    )
    .unwrap();
    let raw = PluginInvocationContext::new(
        "raw",
        "agent",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_mcp_origin(context.mcp_origin().unwrap().clone())
    .enter_plugin("builtin.semantic", "ingest_index_batch");
    assert!(
        authority
            .authorize_connector_semantic(
                &raw,
                &lumvise_db_core::SemanticOperation::RemoveElement {
                    semantic_element_id: "document".into()
                }
            )
            .is_err()
    );
    assert!(authority.authorize_connector_index(&raw).is_err());
    app.mcp_session_membership
        .register_connector("connection", "/other", Some(&id))
        .unwrap();
    assert!(
        authority
            .authorize_connector_call(
                app.semantic.as_ref(),
                &context,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
}

#[test]
fn concurrent_pending_requests_create_only_one_native_review_prompt() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let (_id, context) = connected(&app);
    let authority = app.connector_authority();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                assert!(
                    authority
                        .authorize_connector_call(
                            app.semantic.as_ref(),
                            &context,
                            &InvocationControl::sixty_seconds()
                        )
                        .is_err()
                );
            });
        }
    });
    assert_eq!(
        app.drain_frontend_actions().unwrap(),
        vec![json!({"action":"connector.request_review","payload":{"project_root":"/repo"}})]
    );
    assert_eq!(
        app.connector_knowledge_grants("/repo", &FakeNativeMaintainer)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn accepted_grant_survives_real_store_reopen_but_another_credential_does_not_inherit_it() {
    let temporary = tempfile::tempdir().unwrap();
    let database = temporary.path().join("profile.db");
    let open = || {
        let store = Arc::new(lumvise_db_core::LocalPersistence::open(&database).unwrap());
        AppCore::new(
            store.clone(),
            store,
            lumvise_frontend_core::FrontendCore::default(),
            lumvise_neural_core::LlmProviderRegistry::empty(),
        )
    };
    let id = {
        let app = open();
        let (id, context) = connected(&app);
        app.connector_authority()
            .authorize_connector_call(
                app.semantic.as_ref(),
                &context,
                &InvocationControl::sixty_seconds(),
            )
            .unwrap_err();
        app.decide_connector_knowledge_grant(
            "/repo",
            &id,
            1,
            ConnectorGrantState::Granted,
            &FakeNativeMaintainer,
        )
        .unwrap();
        id
    };
    let app = open();
    let (reconnected, context) = connected(&app);
    assert_eq!(reconnected, id);
    app.connector_authority()
        .authorize_connector_call(
            app.semantic.as_ref(),
            &context,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let other = app
        .connector_authority()
        .register_connector(
            &ConnectorCredential("cd".repeat(32)),
            "Agent installation",
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let project = app
        .connector_knowledge_grants("/repo", &FakeNativeMaintainer)
        .unwrap()[0]
        .project
        .clone();
    assert!(
        app.connector_authority()
            .require_connector_write(
                &other.connector_id,
                &project,
                ConnectorKnowledgeOperation::Update,
                &InvocationControl::sixty_seconds()
            )
            .is_err()
    );
}

struct FakePermissionStorage {
    delegate: Arc<dyn lumvise_db_core::RelationalPersistence>,
    unavailable: std::sync::atomic::AtomicBool,
}
impl lumvise_db_core::RelationalPersistence for FakePermissionStorage {
    fn execute(
        &self,
        operation: lumvise_db_core::RelationalOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::PersistenceResult<lumvise_db_core::RelationalResult> {
        if self.unavailable.load(std::sync::atomic::Ordering::Acquire)
            && matches!(&operation, lumvise_db_core::RelationalOperation::GetPluginData { plugin_id, row_key, .. }
                if plugin_id == "app.knowledge-authority" && row_key.starts_with("connector-grant:"))
        {
            return Err(lumvise_db_core::DbError::invalid_value(
                "permission store unavailable",
                "readable permission record",
            ));
        }
        self.delegate.execute(operation, control)
    }
    fn readiness(
        &self,
    ) -> lumvise_db_core::PersistenceResult<lumvise_db_core::RelationalReadiness> {
        self.delegate.readiness()
    }
}

#[test]
fn unavailable_permission_store_blocks_a_previously_approved_write_without_changing_its_body() {
    let store = Arc::new(lumvise_db_core::LocalPersistence::in_memory().unwrap());
    let permissions = Arc::new(FakePermissionStorage {
        delegate: store.clone(),
        unavailable: false.into(),
    });
    let app = AppCore::new(
        store,
        permissions.clone(),
        lumvise_frontend_core::FrontendCore::default(),
        lumvise_neural_core::LlmProviderRegistry::empty(),
    );
    let (id, context) = connected(&app);
    let native = PluginInvocationContext::new(
        "native",
        "native",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    );
    storage_call(
        &app,
        &native,
        json!({"operation":"ensure_table","table_name":"write_probe","schema":{}}),
    )
    .unwrap();
    storage_call(&app, &native, json!({"operation":"put_row","table_name":"write_probe","row_key":"body","value":"original"})).unwrap();
    app.connector_authority()
        .authorize_connector_call(
            app.semantic.as_ref(),
            &context,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err();
    app.decide_connector_knowledge_grant(
        "/repo",
        &id,
        1,
        ConnectorGrantState::Granted,
        &FakeNativeMaintainer,
    )
    .unwrap();
    permissions
        .unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    let result = storage_call(
        &app,
        &context,
        json!({"operation":"put_row","table_name":"write_probe","row_key":"body","value":"must not persist"}),
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("permission store unavailable")
    );
    assert_eq!(row(&app), json!("original"));
}
