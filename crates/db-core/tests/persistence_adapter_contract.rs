use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::{Uuid, Version};

use lumvise_db_core::{
    CentralizedPersistence, ChangeDisposition, ChangeHookScope, ChangedElement,
    ChangesSinceRevisionPage, LocalPersistence, RelationalOperation, RelationalPersistence,
    RelationalResult, SemanticElement, SemanticGraphGranularity, SemanticGraphProjection,
    SemanticGraphProjectionRequest, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_resource_routing::{
    InvocationControl, ResourceInvocationClient, TransportError,
    protocol::{
        CapabilityReadinessEntryV1, CapabilityReadinessV1, InvocationEnvelopeV1,
        InvocationTerminalStatusV1, InvocationTerminalV1, ReadinessRequestV1, ReadinessResponseV1,
        invocation_envelope_v1::Payload, invocation_start_v1::Operation,
        invocation_terminal_v1::Result as TerminalResult,
    },
};
use serde_json::json;

#[test]
fn project_identity_is_uuid_v4_and_stable_after_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("semantic.sqlite");
    let first_id = {
        let persistence = LocalPersistence::open(&path).unwrap();
        project_identity(&persistence, "/project")
    };

    let parsed = Uuid::parse_str(&first_id).unwrap();
    assert_eq!(parsed.get_version(), Some(Version::Random));

    let reopened_id = {
        let persistence = LocalPersistence::open(&path).unwrap();
        project_identity(&persistence, "/project")
    };
    assert_eq!(reopened_id, first_id);
}

#[test]
fn project_identity_normalizes_equivalent_root_spellings() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let semantic = &persistence;
    let canonical = project_identity(semantic, "/project");
    assert_eq!(project_identity(semantic, " /project/ "), canonical);
    let root = project_identity(semantic, "/");
    assert_eq!(project_identity(semantic, " //// "), root);
}

#[test]
fn project_identity_project_id_is_not_globally_unique() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("semantic.sqlite");
    let first_id = {
        let persistence = LocalPersistence::open(&path).unwrap();
        project_identity(&persistence, "/project-one")
    };

    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "INSERT INTO semantic_project_lineages
         (project_root, project_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3)",
        rusqlite::params!["/project-two", first_id, "test"],
    )
    .unwrap();
    drop(raw);

    let second_id = {
        let persistence = LocalPersistence::open(&path).unwrap();
        project_identity(&persistence, "/project-two")
    };
    assert_eq!(second_id, first_id);
}

fn project_identity(graph: &dyn SemanticPersistence, project_root: &str) -> String {
    match SemanticPersistence::execute(
        graph,
        SemanticOperation::ProjectIdentity {
            project_root: project_root.into(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap()
    {
        SemanticResult::ProjectIdentity(identity) => identity.project_id,
        other => panic!("unexpected semantic result: {other:?}"),
    }
}

#[test]
fn local_persistence_shares_fresh_composition_and_executes_both_contracts() {
    let temp = tempfile::tempdir().unwrap();
    let relational_path = temp.path().join("relational.sqlite");
    let relational = LocalPersistence::open(&relational_path).unwrap();
    let sql = &relational;
    let control = InvocationControl::sixty_seconds();

    let stored = RelationalPersistence::execute(
        sql,
        RelationalOperation::SetPersistentSetting {
            scope: "adapter".into(),
            key: "selection".into(),
            value: json!("relational-only"),
        },
        &control,
    )
    .unwrap();
    match stored {
        RelationalResult::PersistentSettingUpdated(record) => {
            assert_eq!(record.value, json!("relational-only"))
        }
        other => panic!("unexpected relational result: {other:?}"),
    }
    let loaded = RelationalPersistence::execute(
        sql,
        RelationalOperation::GetPersistentSetting {
            scope: "adapter".into(),
            key: "selection".into(),
        },
        &control,
    )
    .unwrap();
    match loaded {
        RelationalResult::PersistentSetting(Some(record)) => {
            assert_eq!(record.value, json!("relational-only"))
        }
        other => panic!("unexpected relational result: {other:?}"),
    }
    RelationalPersistence::execute(
        sql,
        RelationalOperation::EnsurePluginDataTable {
            plugin_id: "plugin.adapter".into(),
            table_name: "records".into(),
            schema: json!({"type": "object"}),
        },
        &control,
    )
    .unwrap();
    let tables = RelationalPersistence::execute(
        sql,
        RelationalOperation::ListPluginDataTables {
            plugin_id: "plugin.adapter".into(),
        },
        &control,
    )
    .unwrap();
    assert!(matches!(&tables, RelationalResult::PluginDataTables(tables) if tables.len() == 1));
    RelationalPersistence::execute(
        sql,
        RelationalOperation::PutPluginData {
            plugin_id: "plugin.adapter".into(),
            table_name: "records".into(),
            row_key: "row-1".into(),
            value: json!({"stored": true}),
        },
        &control,
    )
    .unwrap();
    let deleted = RelationalPersistence::execute(
        sql,
        RelationalOperation::DeletePluginData {
            plugin_id: "plugin.adapter".into(),
            table_name: "records".into(),
            row_key: "row-1".into(),
        },
        &control,
    )
    .unwrap();
    assert!(
        matches!(deleted, RelationalResult::PluginDataMutations(result) if result.rows_deleted == 1)
    );
    assert!(graph_sibling_path(&relational_path).exists());

    let semantic_path = temp.path().join("semantic.sqlite");
    let semantic = LocalPersistence::open(&semantic_path).unwrap();
    let graph = &semantic;
    let element = semantic_element("element:1");
    let synced = SemanticPersistence::execute(
        graph,
        SemanticOperation::SyncStructure {
            project_root: "/project".into(),
            elements: vec![element.clone()],
            relationships: vec![],
        },
        &control,
    )
    .unwrap();
    match synced {
        SemanticResult::SyncStructure(report) => assert_eq!(report.elements_upserted, 1),
        other => panic!("unexpected semantic result: {other:?}"),
    }
    let loaded = SemanticPersistence::execute(
        graph,
        SemanticOperation::Element {
            semantic_element_id: element.semantic_element_id.clone(),
        },
        &control,
    )
    .unwrap();
    match loaded {
        SemanticResult::Element(Some(found)) => assert_eq!(found, element),
        other => panic!("unexpected semantic result: {other:?}"),
    }
    let staged_element = semantic_element("element:staged");
    SemanticPersistence::execute(
        graph,
        SemanticOperation::BeginProjectSnapshot {
            snapshot_id: "snapshot-1".into(),
            project_root: "/project".into(),
            partition_paths: vec![],
            page_count: 1,
        },
        &control,
    )
    .unwrap();
    SemanticPersistence::execute(
        graph,
        SemanticOperation::StageProjectSnapshot {
            snapshot_id: "snapshot-1".into(),
            page_index: 0,
            elements: vec![staged_element.clone()],
            relationships: vec![],
        },
        &control,
    )
    .unwrap();
    let committed = SemanticPersistence::execute(
        graph,
        SemanticOperation::CommitProjectSnapshot {
            snapshot_id: "snapshot-1".into(),
        },
        &control,
    )
    .unwrap();
    assert!(matches!(committed, SemanticResult::SnapshotCommitted(_)));
    assert!(graph_sibling_path(&semantic_path).exists());
}

fn semantic_element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/project".into(),
        semantic_element_id: id.into(),
        semantic_source_id: id.into(),
        path: "src/lib.rs".into(),
        element_kind: "function".into(),
        name: "example".into(),
        parent_element_id: None,
        content_fingerprint: Some("fp1:0000000000000001:example".into()),
        start_line: Some(1),
        end_line: Some(1),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn graph_sibling_path(path: &Path) -> PathBuf {
    let mut graph_path = path.to_path_buf();
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap();
    graph_path.set_file_name(format!("{file_name}.semantic.grafeo"));
    graph_path
}

struct CentralClient;

impl ResourceInvocationClient for CentralClient {
    fn readiness(
        &self,
        request: &ReadinessRequestV1,
        control: &InvocationControl,
    ) -> std::result::Result<ReadinessResponseV1, TransportError> {
        assert!(!control.is_expired());
        Ok(ReadinessResponseV1 {
            tenant_id: "tenant".into(),
            server_instance_id: "server".into(),
            negotiated_minor: 0,
            capabilities: request
                .requested_capabilities
                .iter()
                .map(|capability| CapabilityReadinessEntryV1 {
                    capability: *capability,
                    status: CapabilityReadinessV1::Ready as i32,
                })
                .collect(),
            llm_providers: vec![],
            speech: vec![],
        })
    }

    fn invoke(
        &self,
        envelopes: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> std::result::Result<Vec<InvocationEnvelopeV1>, TransportError> {
        assert_eq!(envelopes.len(), 1);
        assert!(!control.is_cancelled());
        let start = &envelopes[0];
        let operation = match start.payload.as_ref() {
            Some(Payload::Start(start)) => {
                assert_eq!(start.deadline_unix_ms, control.deadline_unix_ms());
                start.operation.as_ref().unwrap()
            }
            _ => panic!("central persistence adapter must send one start envelope"),
        };
        let result = match operation {
            Operation::SemanticOperation(operation) => {
                assert!(matches!(
                    CentralizedPersistence::decode_semantic_operation(operation).unwrap(),
                    SemanticOperation::SemanticRevision
                ));
                TerminalResult::Semantic(
                    CentralizedPersistence::encode_semantic_result(
                        SemanticResult::SemanticRevision { commit_version: 42 },
                    )
                    .unwrap(),
                )
            }
            Operation::RelationalOperation(operation) => {
                assert!(matches!(
                    CentralizedPersistence::decode_relational_operation(operation).unwrap(),
                    RelationalOperation::GetPersistentSetting { .. }
                ));
                TerminalResult::Relational(
                    CentralizedPersistence::encode_relational_result(
                        RelationalResult::PersistentSetting(None),
                    )
                    .unwrap(),
                )
            }
            _ => panic!("central persistence adapter sent unexpected operation"),
        };
        Ok(vec![InvocationEnvelopeV1 {
            protocol_major: start.protocol_major,
            protocol_minor: start.protocol_minor,
            request_id: start.request_id.clone(),
            sequence: 0,
            payload: Some(Payload::Terminal(InvocationTerminalV1 {
                status: InvocationTerminalStatusV1::Completed as i32,
                retryable: false,
                outcome_unknown: false,
                error_code: None,
                message: None,
                result: Some(result),
            })),
        }])
    }

    fn cancel(
        &self,
        _: &InvocationEnvelopeV1,
        _: &InvocationControl,
    ) -> std::result::Result<InvocationTerminalV1, TransportError> {
        unreachable!("persistence execution does not cancel in this test")
    }
}

#[test]
fn centralized_adapters_preserve_control_and_use_typed_binary_persistence_records() {
    let client: Arc<dyn ResourceInvocationClient> = Arc::new(CentralClient);
    let persistence = CentralizedPersistence::new(client, "desktop-1");
    let control = InvocationControl::sixty_seconds();

    assert!(SemanticPersistence::readiness(&persistence).unwrap().ready);
    assert!(
        RelationalPersistence::readiness(&persistence)
            .unwrap()
            .ready
    );
    assert!(matches!(
        SemanticPersistence::execute(&persistence, SemanticOperation::SemanticRevision, &control,)
            .unwrap(),
        SemanticResult::SemanticRevision { commit_version: 42 }
    ));
    assert!(matches!(
        RelationalPersistence::execute(
            &persistence,
            RelationalOperation::GetPersistentSetting {
                scope: "app".into(),
                key: "theme".into(),
            },
            &control,
        )
        .unwrap(),
        RelationalResult::PersistentSetting(None)
    ));
}

#[test]
fn revision_delta_operations_and_pages_round_trip_through_centralized_transport() {
    let scope = ChangeHookScope {
        project_root: "/repo".into(),
        entity_kinds: ["file".to_string()].into_iter().collect(),
    };
    let operation = SemanticOperation::ChangesSinceRevision {
        scope: scope.clone(),
        after_revision: 4,
        limit: 100,
    };
    let encoded_operation =
        CentralizedPersistence::encode_semantic_operation(operation.clone()).unwrap();
    assert_eq!(encoded_operation.operation_name, "ChangesSinceRevision");
    assert!(matches!(
        CentralizedPersistence::decode_semantic_operation(&encoded_operation).unwrap(),
        SemanticOperation::ChangesSinceRevision {
            scope: decoded_scope,
            after_revision: 4,
            limit: 100,
        } if decoded_scope == scope
    ));

    let result = SemanticResult::ChangesSinceRevision(ChangesSinceRevisionPage {
        base_revision: 4,
        target_revision: 7,
        head_revision: 9,
        has_more: false,
        changed: vec![ChangedElement {
            element_id: "element".into(),
            entity_kind: "file".into(),
            revision: 7,
            disposition: ChangeDisposition::Upserted,
        }],
    });
    let encoded_result = CentralizedPersistence::encode_semantic_result(result.clone()).unwrap();
    assert_eq!(encoded_result.operation_name, "ChangesSinceRevision");
    assert!(matches!(
        CentralizedPersistence::decode_semantic_result(&encoded_result).unwrap(),
        SemanticResult::ChangesSinceRevision(page)
            if page.base_revision == 4
                && page.target_revision == 7
                && page.head_revision == 9
                && !page.has_more
                && matches!(
                    page.changed.as_slice(),
                    [ChangedElement {
                        element_id,
                        entity_kind,
                        revision: 7,
                        disposition: ChangeDisposition::Upserted,
                    }] if element_id == "element" && entity_kind == "file"
                )
    ));
}

#[test]
fn semantic_adapter_persists_binary_blobs() {
    let temp = tempfile::tempdir().unwrap();
    let semantic = LocalPersistence::open(temp.path().join("semantic.sqlite")).unwrap();
    let control = InvocationControl::sixty_seconds();

    let stored = SemanticPersistence::execute(
        &semantic,
        SemanticOperation::ArtifactBlobPut {
            content_ref: "blob://contract".into(),
            artifact_id: "artifact".into(),
            media_type: "application/octet-stream".into(),
            content: vec![0, 255, 1],
        },
        &control,
    )
    .unwrap();
    assert!(
        matches!(stored, SemanticResult::ArtifactBlob(Some(blob)) if blob.content == vec![0, 255, 1])
    );
    assert!(matches!(
        SemanticPersistence::execute(
            &semantic,
            SemanticOperation::ArtifactBlobGet {
                content_ref: "blob://contract".into(),
            },
            &control,
        )
            .unwrap(),
        SemanticResult::ArtifactBlob(Some(blob)) if blob.media_type == "application/octet-stream"
    ));
}

#[test]
fn renderer_graph_operation_and_result_round_trip_through_v2_codec() {
    let operation = SemanticOperation::ProjectRendererGraph(SemanticGraphProjectionRequest {
        project_root: "/repo".into(),
        target_path: None,
        granularity: SemanticGraphGranularity::File,
        recursive: true,
        include_external: false,
        include_first_neighbors: false,
    });
    let encoded_operation = CentralizedPersistence::encode_semantic_operation(operation).unwrap();
    assert_eq!(encoded_operation.operation_name, "ProjectRendererGraph");
    assert!(matches!(
        CentralizedPersistence::decode_semantic_operation(&encoded_operation).unwrap(),
        SemanticOperation::ProjectRendererGraph(request)
            if request.project_root == "/repo"
                && request.granularity == SemanticGraphGranularity::File
                && request.recursive
    ));

    let result = SemanticResult::RendererGraphProjection(SemanticGraphProjection {
        commit_version: 7,
        published_at: "2026-07-22T00:00:00Z".into(),
        project_root: "/repo".into(),
        nodes: Vec::new(),
        edges: Vec::new(),
    });
    let encoded_result = CentralizedPersistence::encode_semantic_result(result).unwrap();
    assert_eq!(encoded_result.operation_name, "RendererGraphProjection");
    assert!(matches!(
        CentralizedPersistence::decode_semantic_result(&encoded_result).unwrap(),
        SemanticResult::RendererGraphProjection(projection)
            if projection.commit_version == 7
                && projection.project_root == "/repo"
                && projection.nodes.is_empty()
                && projection.edges.is_empty()
    ));
}
