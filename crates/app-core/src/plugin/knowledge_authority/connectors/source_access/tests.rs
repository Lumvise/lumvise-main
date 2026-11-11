use super::*;
use crate::AppCore;
use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticResult};
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityRequest, HostPluginInvocation, HostPluginInvoker,
    PluginInvocationClass, PluginMcpOrigin,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct SourceAccessFixture {
    app: AppCore,
    context: PluginInvocationContext,
    id: String,
    folder: tempfile::TempDir,
}

struct FakeUnreachedSourcePublisher;

impl HostPluginInvoker for FakeUnreachedSourcePublisher {
    fn invoke_plugin(&self, _: HostPluginInvocation) -> Result<Value, HostCapabilityError> {
        panic!("source permission denial must precede publication");
    }
}

impl SourceAccessFixture {
    fn new(app: AppCore) -> Self {
        let (id, context) = connect_source(&app);
        Self {
            app,
            id,
            context,
            folder: tempfile::tempdir().unwrap(),
        }
    }

    fn request(&self, path: &Path, approval: bool) -> Result<PathBuf, HostCapabilityError> {
        self.app.connector_authority().require_source_folder(
            &self.context,
            &SourceImportAuthorization::configured(None),
            path.to_str().unwrap(),
            &InvocationControl::sixty_seconds(),
            approval,
        )
    }

    fn access(&self) -> ConnectorSourceAccess {
        self.app
            .connector_source_access()
            .unwrap()
            .into_iter()
            .find(|row| row.connector.connector_id == self.id)
            .unwrap()
    }

    fn decide(&self, state: ConnectorGrantState) -> ConnectorSourceAccess {
        let access = self.access();
        self.app
            .change_connector_source_access(ConnectorSourceAccessChange::Folder {
                connector_id: self.id.clone(),
                expected_revision: access.revision,
                folder: access.grants[0].folder.clone(),
                state,
            })
            .unwrap()
    }

    fn policy(&self, mode: ConnectorSourceMode, roots: Vec<String>) -> ConnectorSourceAccess {
        self.app
            .change_connector_source_access(ConnectorSourceAccessChange::Policy {
                connector_id: self.id.clone(),
                expected_revision: self.access().revision,
                mode,
                roots,
            })
            .unwrap()
    }
}

fn connect_source(app: &AppCore) -> (String, PluginInvocationContext) {
    let control = InvocationControl::sixty_seconds();
    let id = app
        .connector_authority()
        .register_connector(
            &ConnectorCredential("cd".repeat(32)),
            "Source test agent",
            &control,
        )
        .unwrap()
        .connector_id;
    app.mcp_session_membership
        .register_connector("source-connection", "/repo", Some(&id))
        .unwrap();
    let SemanticResult::ProjectIdentity(project) = app
        .semantic
        .execute(
            SemanticOperation::ProjectIdentity {
                project_root: "/repo".into(),
            },
            &control,
        )
        .unwrap()
    else {
        panic!("expected project identity");
    };
    let epoch = app
        .mcp_session_membership
        .capture_project("source-connection", "/repo", &project.project_id, None)
        .unwrap();
    let context = PluginInvocationContext::new(
        "source-test",
        "mcp",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_route_identity(
        "source-connection",
        Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE.into()),
    )
    .with_scope_epoch(Some(epoch))
    .with_mcp_origin(PluginMcpOrigin {
        connector_id: Some(id.clone()),
        connection_id: "source-connection".into(),
        project_epoch: Some(epoch),
    })
    .enter_plugin("builtin.semantic", "index_project");
    (id, context)
}

#[test]
fn real_broker_queues_one_popup_before_any_source_publication() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    assert_eq!(f.access().mode, ConnectorSourceMode::Restricted);
    let broker = f.app.default_plugin_host_capability_broker().unwrap();
    for _ in 0..3 {
        let call = HostCapabilityRequest {
            plugin_id: "builtin.semantic".into(),
            invocation_id: "source-test".into(),
            call_id: "source".into(),
            capability_id: "project.import".into(),
            required_version: "1".into(),
            input: json!({"operation":"import_folder","request":{"folderPath":f.folder.path(),"projectRoot":"/repo"}}),
        };
        let error = broker
            .invoke_with_nested(call, &f.context, &FakeUnreachedSourcePublisher)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("source_import_approval_required"),
            "{error}"
        );
    }
    assert_eq!(f.access().revision, 1);
    assert_eq!(f.access().grants.len(), 1);
    let queued = f.app.frontend_actions.lock().unwrap();
    assert_eq!(
        queued
            .iter()
            .filter(|v| v["action"] == "connector.source_review")
            .count(),
        1
    );
}

#[test]
fn native_allow_and_revoke_are_rechecked_on_the_existing_connection() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    assert!(f.request(f.folder.path(), true).is_err());
    f.decide(ConnectorGrantState::Granted);
    assert_eq!(
        f.request(f.folder.path(), false).unwrap(),
        f.folder.path().canonicalize().unwrap()
    );
    f.decide(ConnectorGrantState::Revoked);
    assert!(
        f.request(f.folder.path(), false)
            .unwrap_err()
            .to_string()
            .contains("source_import_revoked")
    );
    assert_eq!(f.access().revision, 3);
}

#[test]
fn autoapprove_is_scoped_and_switching_to_restricted_removes_automatic_access() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    let child = f.folder.path().join("child");
    std::fs::create_dir(&child).unwrap();
    f.policy(
        ConnectorSourceMode::Autoapprove,
        vec![f.folder.path().to_str().unwrap().into()],
    );
    assert_eq!(
        f.request(&child, true).unwrap(),
        child.canonicalize().unwrap()
    );
    assert!(f.access().grants.is_empty());
    let outside = tempfile::tempdir().unwrap();
    assert!(f.request(outside.path(), true).is_err());
    f.policy(ConnectorSourceMode::Restricted, vec![]);
    assert!(
        f.request(&child, true)
            .unwrap_err()
            .to_string()
            .contains("source_import_approval_required")
    );
}

#[test]
fn explicit_revocation_overrides_autoapprove_and_bootstrap_roots() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    assert!(f.request(f.folder.path(), true).is_err());
    f.decide(ConnectorGrantState::Revoked);
    f.policy(
        ConnectorSourceMode::Autoapprove,
        vec![f.folder.path().to_str().unwrap().into()],
    );
    let policy = SourceImportAuthorization::configured(Some(
        std::env::join_paths([f.folder.path()]).unwrap(),
    ));
    let denied = f
        .app
        .connector_authority()
        .require_source_folder(
            &f.context,
            &policy,
            f.folder.path().to_str().unwrap(),
            &InvocationControl::sixty_seconds(),
            false,
        )
        .unwrap_err();
    assert!(denied.to_string().contains("source_import_revoked"));
}

#[test]
fn stale_decision_and_unbounded_autoapprove_are_rejected_without_mutation() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    assert!(f.request(f.folder.path(), true).is_err());
    let before = f.access();
    for change in [
        ConnectorSourceAccessChange::Policy {
            connector_id: f.id.clone(),
            expected_revision: 0,
            mode: ConnectorSourceMode::Restricted,
            roots: vec![],
        },
        ConnectorSourceAccessChange::Policy {
            connector_id: f.id.clone(),
            expected_revision: 1,
            mode: ConnectorSourceMode::Autoapprove,
            roots: vec![],
        },
    ] {
        assert!(f.app.change_connector_source_access(change).is_err());
    }
    assert_eq!(f.access(), before);
}

#[cfg(unix)]
#[test]
fn autoapprove_rejects_symlink_escape_and_retargeted_approved_alias() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    let outside = tempfile::tempdir().unwrap();
    let alias = f.folder.path().join("alias");
    std::os::unix::fs::symlink(outside.path(), &alias).unwrap();
    f.policy(
        ConnectorSourceMode::Autoapprove,
        vec![f.folder.path().to_str().unwrap().into()],
    );
    assert!(f.request(&alias, true).is_err());
    f.policy(ConnectorSourceMode::Restricted, vec![]);
    f.decide(ConnectorGrantState::Granted);
    let different = tempfile::tempdir().unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(different.path(), &alias).unwrap();
    assert!(f.request(&alias, true).is_err());
}

fn persisted_app(path: &Path) -> AppCore {
    let store = Arc::new(LocalPersistence::open(path).unwrap());
    AppCore::new(
        store.clone(),
        store,
        lumvise_frontend_core::FrontendCore::default(),
        lumvise_neural_core::LlmProviderRegistry::empty(),
    )
}

#[test]
fn mode_and_native_decisions_survive_database_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.db");
    let folder = tempfile::tempdir().unwrap();
    let f = SourceAccessFixture::new(persisted_app(&path));
    assert!(f.request(folder.path(), true).is_err());
    f.decide(ConnectorGrantState::Granted);
    f.policy(
        ConnectorSourceMode::Autoapprove,
        vec![directory.path().to_str().unwrap().into()],
    );
    let before = f.access();
    drop(f);
    let reopened = SourceAccessFixture::new(persisted_app(&path));
    assert_eq!(reopened.access(), before);
    assert!(reopened.request(folder.path(), false).is_ok());
}

#[test]
fn pending_request_reopens_one_popup_after_restart_without_changing_its_grant() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pending.db");
    let folder = tempfile::tempdir().unwrap();
    let first = SourceAccessFixture::new(persisted_app(&path));
    assert!(first.request(folder.path(), true).is_err());
    let before = first.access();
    drop(first);
    let reopened = SourceAccessFixture::new(persisted_app(&path));
    for _ in 0..3 {
        assert!(reopened.request(folder.path(), true).is_err());
    }
    assert_eq!(reopened.access(), before);
    assert_eq!(
        reopened
            .app
            .frontend_actions
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["action"] == "connector.source_review")
            .count(),
        1
    );
}

#[test]
fn unregistered_or_spoofed_connector_cannot_create_permission_requests() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    let context = f.context.clone().with_mcp_origin(PluginMcpOrigin {
        connector_id: Some("connector:forged".into()),
        connection_id: "source-connection".into(),
        project_epoch: Some(1),
    });
    assert!(
        f.app
            .connector_authority()
            .require_source_folder(
                &context,
                &SourceImportAuthorization::configured(None),
                f.folder.path().to_str().unwrap(),
                &InvocationControl::sixty_seconds(),
                true
            )
            .is_err()
    );
    assert_eq!(f.access().revision, 0);
}

#[test]
fn mcp_cannot_submit_approval_flags_or_invoke_native_decision_capabilities() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    let broker = f.app.default_plugin_host_capability_broker().unwrap();
    for (capability, input) in [
        (
            "project.import",
            json!({"operation":"import_folder","request":{"folderPath":f.folder.path(),"autoapprove":true}}),
        ),
        (
            "change_connector_source_access",
            json!({"connector_id":f.id,"mode":"autoapprove"}),
        ),
    ] {
        let call = HostCapabilityRequest {
            plugin_id: "builtin.semantic".into(),
            invocation_id: "source-test".into(),
            call_id: "forged-approval".into(),
            capability_id: capability.into(),
            required_version: "1".into(),
            input,
        };
        assert!(
            broker
                .invoke_with_nested(call, &f.context, &FakeUnreachedSourcePublisher)
                .is_err()
        );
    }
    assert_eq!(f.access().revision, 0);
}

#[test]
fn foreign_project_target_is_rejected_before_creating_a_folder_popup() {
    let f = SourceAccessFixture::new(AppCore::in_memory().unwrap());
    let broker = f.app.default_plugin_host_capability_broker().unwrap();
    let call = HostCapabilityRequest {
        plugin_id: "builtin.semantic".into(),
        invocation_id: "source-test".into(),
        call_id: "foreign-target".into(),
        capability_id: "project.import".into(),
        required_version: "1".into(),
        input: json!({"operation":"import_folder","request":{"folderPath":f.folder.path(),"projectRoot":"/foreign"}}),
    };
    assert!(
        broker
            .invoke_with_nested(call, &f.context, &FakeUnreachedSourcePublisher)
            .is_err()
    );
    assert_eq!(f.access().revision, 0);
    assert!(
        !f.app
            .frontend_actions
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["action"] == "connector.source_review")
    );
}
