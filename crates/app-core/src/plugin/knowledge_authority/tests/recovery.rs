//! Exercise the controlled storage entry point with real project and MCP membership authority.
use super::*;
use crate::plugin::{
    semantic_snapshot_writes::SemanticSnapshotWrites, semantic_storage_capability,
};
use lumvise_db_core::{SemanticResult, SemanticRevisionPublication};

fn seed_managed_project(fixture: &AuthorityFixture, root: &str) {
    fixture.seed_project(root);
    let SemanticResult::RevisionHistory(state) = SemanticPersistence::execute(
        fixture.persistence.as_ref(),
        SemanticOperation::RevisionHistory {
            project_root: root.into(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap() else {
        panic!("expected adopted revision history")
    };
    let head = state.selection("working").unwrap();
    let publication = SemanticRevisionPublication {
        operation_id: format!("manage-{root}"),
        producer_base: None,
        project_root: root.into(),
        project_id: state.project_id.clone(),
        selection_id: "working".into(),
        source_context: head.selection.source_context.clone(),
        expected: head.expectation(&state.records),
        structure: None,
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    };
    SemanticPersistence::execute(
        fixture.persistence.as_ref(),
        SemanticOperation::PublishRevisions(publication),
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
}

fn inspect(
    fixture: &AuthorityFixture,
    root: &str,
    context: &PluginInvocationContext,
) -> Result<Value, HostCapabilityError> {
    semantic_storage_capability::invoke_controlled(
        fixture.persistence.as_ref(),
        &SemanticSnapshotWrites::new(),
        "builtin.semantic",
        json!({"operation":"inspect_revision_integrity", "project_root":root}),
        context,
        &fixture.authority,
    )
}

#[test]
fn direct_native_owner_and_bound_mcp_can_inspect_only_their_authorized_project() {
    let fixture = AuthorityFixture::new();
    seed_managed_project(&fixture, "/p");
    seed_managed_project(&fixture, "/q");
    let native = PluginInvocationContext::new(
        "native-inspection",
        "desktop-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_human_interaction()
    .enter_plugin("builtin.semantic", "inspect_revision_integrity");
    assert_eq!(
        inspect(&fixture, "/p", &native).unwrap()["missing_records"],
        json!([])
    );
    let mounted = fixture
        .mcp_invocation("repair-owner", "/p")
        .enter_plugin("builtin.semantic", "inspect_revision_integrity");
    let result = inspect(&fixture, "/p", &mounted).unwrap();
    assert_eq!(result["project_root"], "/p");
    assert_eq!(result["missing_records"], json!([]));
    assert!(inspect(&fixture, "/q", &mounted).is_err());
}

#[test]
fn delegated_unscoped_and_wrong_export_calls_cannot_promote_themselves_to_repair() {
    let fixture = AuthorityFixture::new();
    seed_managed_project(&fixture, "/p");
    let base = fixture.mcp_invocation("repair-owner", "/p");
    let delegated = base
        .clone()
        .enter_plugin("builtin.knowledge", "get_document")
        .enter_plugin("builtin.semantic", "inspect_revision_integrity");
    let wrong_export = base
        .clone()
        .enter_plugin("builtin.semantic", "read_revision_selection");
    let unscoped = PluginInvocationContext::new(
        "unscoped",
        "desktop-owner",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.semantic", "inspect_revision_integrity");
    for context in [delegated, wrong_export, unscoped] {
        let error = inspect(&fixture, "/p", &context).unwrap_err().to_string();
        assert!(error.contains("direct host-validated"), "{error}");
    }
    let stale = base
        .with_scope_epoch(None)
        .enter_plugin("builtin.semantic", "inspect_revision_integrity");
    assert!(inspect(&fixture, "/p", &stale).is_err());
}

#[test]
fn legacy_unscoped_broker_cannot_inspect_or_recover_integrity() {
    use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
    let app = crate::AppCore::in_memory().unwrap();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    for input in [
        json!({"operation":"inspect_revision_integrity","project_root":"/p"}),
        json!({"operation":"recover_revision_integrity","request":{
            "operation_id":"recover", "project_root":"/p", "project_id":"project", "expected_commit_version":1,
            "provider_instance_id":"parser", "missing_records":[{"kind":"element","record_id":"missing"}]
        }}),
    ] {
        let error = broker
            .invoke(HostCapabilityRequest {
                plugin_id: "builtin.semantic".into(),
                invocation_id: "unscoped-repair".into(),
                call_id: "guarded-recovery".into(),
                capability_id: "storage.semantic".into(),
                required_version: "^1.10.0".into(),
                input,
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("`recovery_context_required`"),
            "{error}"
        );
    }
}
