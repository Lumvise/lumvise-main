//! Routing extracts declared scopes without inferring ownership from opaque record IDs.
use lumvise_db_core::SemanticOperation;
use serde_json::{Value, json};

const ROOT: &str = "lumvise://projects/22222222-2222-4222-8222-222222222222";

fn operation(kind: &str, fields: Value) -> SemanticOperation {
    serde_json::from_value(Value::Object([(kind.into(), fields)].into_iter().collect())).unwrap()
}

fn revision_cases() -> Vec<SemanticOperation> {
    let observed = json!({"generation":0,"records":[],"connection_ids":[]});
    let changes = json!({"operation_id":"edit","project_root":ROOT,"project_id":"lineage","selection_id":"working","source_context":"local","expected":observed,"structure":null,"artifacts":[],"removed_artifact_ids":[],"source_anchor":null});
    let pin = json!({"operation_id":"pin","project_root":ROOT,"project_id":"lineage","selection_id":"public","source_context":"local","expected_generation":0,"mode":"pinned","records":[],"connection_ids":[],"source_anchor":null});
    let sweep = json!({"operation_id":"gc","project_root":ROOT,"project_id":"lineage","expected_commit_version":1,"evaluated_at_ms":1,"records":[],"expired_association_ids":[]});
    [
        ("RevisionFamilies", json!({"project_root":ROOT,"selection_id":"working","records":[]})),
        ("RevisionConnections", json!({"project_root":ROOT,"project_id":"lineage","connection_ids":[]})),
        ("RevisionSelection", json!({"project_root":ROOT,"selection_id":"working"})),
        ("RevisionSelectionMembers", json!({"project_root":ROOT,"selection_id":"working","structure_coverage":{"kind":"none"},"artifact_ids":[]})),
        ("RevisionSelectionAt", json!({"project_root":ROOT,"selection_id":"working","generation":1})),
        ("RevisionSourceAnchor", json!({"project_root":ROOT,"selection_id":"working","anchor":{"revision":"git","canonical_remote":null,"dirty":false}})),
        ("PublishRevisions", changes), ("SelectRevisions", pin),
        ("AcquireCitationRetention", json!({"project_root":ROOT,"project_id":"lineage","source_context":"local","intent":{"space_id":"space","document_id":"document","publication_id":"write"},"records":[{"kind":"element","record_id":"original"}],"artifact_edits":{}})),
        ("ReleaseCitationRetention", json!({"project_root":ROOT,"project_id":"lineage","intent":{"space_id":"space","document_id":"document","publication_id":"write"},"expected_generation":1})),
        ("ResolveRevision", json!({"project_root":ROOT,"record":{"kind":"element","record_id":"e17"}})),
        ("RevisionHistory", json!({"project_root":ROOT})),
        ("PlanRevisionRetention", json!({"project_root":ROOT,"operation_id":"gc","policy":{"mode":"revision_count","revisions":10}})),
        ("CollectRevisionRetention", json!({"plan":sweep,"policy":{"mode":"revision_count","revisions":10}})),
    ].into_iter().map(|(kind, fields)| operation(kind, fields)).collect()
}

#[test]
fn every_revision_operation_keeps_its_explicit_project_route() {
    for operation in revision_cases() {
        assert_eq!(operation.project_root(), Some(ROOT), "{operation:?}");
    }
}

#[test]
fn source_and_view_operations_keep_roots_while_id_only_selectors_need_lookup() {
    for operation in [
        operation(
            "SyncStructure",
            json!({"project_root":ROOT,"elements":[],"relationships":[]}),
        ),
        operation("ProjectArchive", json!({"project_root":ROOT})),
        operation(
            "ProjectSnapshot",
            json!({"scope":{"project_root":ROOT},"artifact_namespace":null}),
        ),
        operation(
            "ScopedRead",
            json!({"Location":{"project_root":ROOT,"path":null,"line":1,"include_inactive":false}}),
        ),
    ] {
        assert_eq!(operation.project_root(), Some(ROOT));
    }
    assert_eq!(
        operation("Element", json!({"semantic_element_id":"opaque"})).project_root(),
        None
    );
    assert_eq!(
        operation(
            "ProjectSnapshot",
            json!({"scope":{"semantic_element":"opaque"},"artifact_namespace":null})
        )
        .project_root(),
        None
    );
}
