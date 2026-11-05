use super::*;
use crate::{AppCore, ConnectorGrantMaintainer};
use std::sync::atomic::{AtomicBool, Ordering};

struct FakeProjectMaintainer(AtomicBool);

impl ConnectorGrantMaintainer for FakeProjectMaintainer {
    fn require_maintainer(&self, project: &KnowledgeProjectIdentity) -> Result<(), String> {
        if self.0.load(Ordering::Acquire) {
            return Ok(());
        }
        Err(format!(
            "project {:?}; expected owner rather than editor",
            project.project_id
        ))
    }
}

fn fixture() -> (AppCore, ConnectorIdentity, FakeProjectMaintainer) {
    let app = AppCore::in_memory().unwrap();
    let identity = app
        .connector_authority()
        .register_connector(
            &ConnectorCredential("a1".repeat(32)),
            "Test connector",
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    (app, identity, FakeProjectMaintainer(AtomicBool::new(true)))
}

fn project(app: &AppCore) -> KnowledgeProjectIdentity {
    let authority = app.connector_authority();
    let control = InvocationControl::sixty_seconds();
    super::super::project_access::resolve_project(
        app.semantic.as_ref(),
        "/repo",
        &authority.identity(&control).unwrap(),
        &control,
    )
    .unwrap()
}

fn request(app: &AppCore, id: &str) -> Result<(), HostCapabilityError> {
    app.connector_authority().require_connector_write(
        id,
        &project(app),
        ConnectorKnowledgeOperation::Create,
        &InvocationControl::sixty_seconds(),
    )
}

#[test]
fn authenticated_connector_identity_survives_reconnect_without_trusting_its_label() {
    let (app, identity, _) = fixture();
    let authority = app.connector_authority();
    let control = InvocationControl::sixty_seconds();
    let credential = ConnectorCredential("a1".repeat(32));
    assert_eq!(
        authority
            .register_connector(&credential, "Changed label", &control)
            .unwrap(),
        identity
    );
    assert_eq!(
        app.connector_authority()
            .authenticate_connector(&credential, &control)
            .unwrap(),
        identity
    );
    assert!(
        authority
            .authenticate_connector(&ConnectorCredential("b2".repeat(32)), &control)
            .is_err()
    );
    assert!(
        authority
            .register_connector(
                &ConnectorCredential("short secret".into()),
                "Other",
                &control
            )
            .is_err()
    );
    assert_eq!(format!("{credential:?}"), "ConnectorCredential([redacted])");
    assert!(!json!(identity).to_string().contains(&credential.0));
}

#[test]
fn duplicate_requests_keep_one_pending_permission_until_native_approval() {
    let (app, identity, maintainer) = fixture();
    for _ in 0..3 {
        assert!(request(&app, &identity.connector_id).is_err());
    }
    let pending = app
        .connector_knowledge_grants("/repo", &maintainer)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].revision, 1);
    assert_eq!(pending[0].state, ConnectorGrantState::Pending);
    assert!(pending[0].operations.is_empty());
    app.decide_connector_knowledge_grant(
        "/repo",
        &identity.connector_id,
        1,
        ConnectorGrantState::Granted,
        &maintainer,
    )
    .unwrap();
    assert!(request(&app, &identity.connector_id).is_ok());
}

#[test]
fn grant_is_bound_to_credential_and_project_lineage_and_is_rechecked_after_revocation() {
    let (app, identity, maintainer) = fixture();
    assert!(request(&app, &identity.connector_id).is_err());
    app.decide_connector_knowledge_grant(
        "/repo",
        &identity.connector_id,
        1,
        ConnectorGrantState::Granted,
        &maintainer,
    )
    .unwrap();
    let authority = app.connector_authority();
    let control = InvocationControl::sixty_seconds();
    assert!(
        authority
            .require_connector_write(
                &identity.connector_id,
                &project(&app),
                ConnectorKnowledgeOperation::Update,
                &control
            )
            .is_ok()
    );
    let mut replaced = project(&app);
    replaced.storage_authority = "different durable authority".into();
    assert!(
        authority
            .require_connector_write(
                &identity.connector_id,
                &replaced,
                ConnectorKnowledgeOperation::Create,
                &control
            )
            .is_err()
    );
    assert!(request(&app, "connector:forged").is_err());
    app.decide_connector_knowledge_grant(
        "/repo",
        &identity.connector_id,
        2,
        ConnectorGrantState::Revoked,
        &maintainer,
    )
    .unwrap();
    assert!(request(&app, &identity.connector_id).is_err());
    assert_eq!(
        app.connector_knowledge_grants("/repo", &maintainer)
            .unwrap()[0]
            .revision,
        3
    );
}

#[test]
fn editor_and_stale_decisions_cannot_approve_or_restore_a_revoked_permission() {
    let (app, identity, maintainer) = fixture();
    assert!(request(&app, &identity.connector_id).is_err());
    maintainer.0.store(false, Ordering::Release);
    assert!(
        app.decide_connector_knowledge_grant(
            "/repo",
            &identity.connector_id,
            1,
            ConnectorGrantState::Granted,
            &maintainer
        )
        .is_err()
    );
    maintainer.0.store(true, Ordering::Release);
    app.decide_connector_knowledge_grant(
        "/repo",
        &identity.connector_id,
        1,
        ConnectorGrantState::Revoked,
        &maintainer,
    )
    .unwrap();
    assert!(
        app.decide_connector_knowledge_grant(
            "/repo",
            &identity.connector_id,
            1,
            ConnectorGrantState::Granted,
            &maintainer
        )
        .is_err()
    );
    assert!(
        app.decide_connector_knowledge_grant(
            "/repo",
            &identity.connector_id,
            2,
            ConnectorGrantState::Pending,
            &maintainer
        )
        .is_err()
    );
    assert!(request(&app, &identity.connector_id).is_err());
}
