//! Versioned graph-storage protocol for compiled semantic plugins.
//!
//! Plugins own semantic rules. This adapter only authorizes coarse operations,
//! converts protocol values, and delegates to the selected semantic persistence
//! boundary through owned requests and results.

mod controlled;
pub(super) use controlled::invoke_controlled;

use std::collections::HashSet;

use lumvise_db_core::{
    ProjectSnapshotScope, SemanticArtifact, SemanticElement, SemanticGraphProjectionRequest,
    SemanticOperation, SemanticPartition, SemanticPersistence, SemanticRelationship,
    SemanticResult, StoredSemanticElementNameVector,
};
use lumvise_plugin_runtime::HostCapabilityError;
use lumvise_resource_routing::InvocationControl;
use serde::Deserialize;
use serde_json::{Value, json};

use super::host_capability_catalog::SEMANTIC_STORAGE;
use super::semantic_snapshot_writes::SemanticSnapshotWrites;

const SEMANTIC_PLUGIN_ID: &str = "builtin.semantic";
const KNOWLEDGE_PLUGIN_ID: &str = "builtin.knowledge";
const NUCLEUS_PLUGIN_ID: &str = "builtin.nucleus";

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum SemanticStorageRequest {
    SyncStructure {
        project_root: String,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    },
    BeginProjectSnapshot {
        snapshot_id: String,
        project_root: String,
        partition_paths: Vec<String>,
        page_count: usize,
    },
    StageProjectSnapshot {
        snapshot_id: String,
        page_index: usize,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    },
    CommitProjectSnapshot {
        snapshot_id: String,
    },
    AbortProjectSnapshot {
        snapshot_id: String,
    },
    SyncPartition {
        partition: SemanticPartition,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    },
    Element {
        semantic_element_id: String,
    },
    ElementsByIds {
        project_root: String,
        semantic_element_ids: HashSet<String>,
    },
    ElementsByIdsIncludingInactive {
        project_root: String,
        semantic_element_ids: HashSet<String>,
    },
    ArtifactsByIds {
        artifact_ids: HashSet<String>,
    },
    SearchElementCandidates {
        project_root: Option<String>,
        query: String,
        limit: usize,
    },
    SearchElementNameVectors {
        project_root: String,
        query: Vec<f32>,
        k: usize,
        engine_id: String,
        #[serde(default)]
        model: Option<String>,
    },
    StoreElementNameVectors {
        project_root: String,
        vectors: Vec<StoredSemanticElementNameVector>,
    },
    SearchArtifactTextVectors {
        project_root: String,
        query: Vec<f32>,
        k: usize,
        engine_id: String,
        #[serde(default)]
        model: Option<String>,
    },
    StoreArtifactTextVectors {
        project_root: String,
        vectors: Vec<lumvise_db_core::StoredArtifactTextVector>,
    },
    RelationshipsFrom {
        semantic_element_id: String,
    },
    RelationshipsTouchingElements {
        semantic_element_ids: HashSet<String>,
    },
    Artifact {
        artifact_id: String,
    },
    ArtifactsForElements {
        semantic_element_ids: HashSet<String>,
    },
    ArtifactsForElementWithInheritance {
        semantic_element_id: String,
    },
    ArtifactDependents {
        target_kind: String,
        target_id: String,
    },
    UpsertArtifact {
        artifact: SemanticArtifact,
        #[serde(default = "default_media_type")]
        media_type: String,
    },
    RemoveArtifact {
        artifact_id: String,
    },
    RemoveElement {
        semantic_element_id: String,
    },
    ProjectArtifacts {
        project_root: String,
        artifact_namespace: Option<String>,
    },
    ProjectRoots,
    SemanticRevision,
    ProjectElementCounts {
        project_root: String,
    },
    ScopedRead {
        request: lumvise_db_core::ScopedSemanticRead,
    },
    ProjectSnapshot {
        scope: ProjectSnapshotScope,
        artifact_namespace: Option<String>,
    },
    SelectiveSubgraph {
        project_root: String,
        root_element_id: String,
        artifact_namespace: Option<String>,
    },
    ProjectRendererGraph {
        project_root: String,
        target_path: Option<String>,
        granularity: lumvise_db_core::SemanticGraphGranularity,
        #[serde(default = "default_true")]
        recursive: bool,
        #[serde(default)]
        include_external: bool,
        #[serde(default)]
        include_first_neighbors: bool,
    },
    CandidateSourceElements {
        content_fingerprints: HashSet<String>,
        kind_name_keys: HashSet<String>,
        kind_file_name_keys: HashSet<String>,
    },
}

pub(super) fn invoke(
    semantic: &dyn SemanticPersistence,
    snapshot_writes: &SemanticSnapshotWrites,
    plugin_id: &str,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    let request = serde_json::from_value(input.clone())
        .map_err(|error| invalid_input(&input, &error.to_string()))?;
    authorize(plugin_id, &request)?;
    if let Some(response) = invoke_snapshot_write(semantic, snapshot_writes, plugin_id, &request) {
        return response;
    }
    dispatch(semantic, request).map_err(execution_failed)
}

fn invoke_snapshot_write(
    semantic: &dyn SemanticPersistence,
    writes: &SemanticSnapshotWrites,
    plugin_id: &str,
    request: &SemanticStorageRequest,
) -> Option<Result<Value, HostCapabilityError>> {
    match request {
        SemanticStorageRequest::BeginProjectSnapshot {
            snapshot_id,
            project_root,
            partition_paths,
            page_count,
        } => Some(writes.begin(
            plugin_id,
            snapshot_id,
            project_root,
            partition_paths.clone(),
            *page_count,
        )),
        SemanticStorageRequest::StageProjectSnapshot {
            snapshot_id,
            page_index,
            elements,
            relationships,
        } => Some(writes.stage(
            plugin_id,
            snapshot_id,
            *page_index,
            elements.clone(),
            relationships.clone(),
        )),
        SemanticStorageRequest::CommitProjectSnapshot { snapshot_id } => {
            Some(writes.commit(semantic, plugin_id, snapshot_id))
        }
        SemanticStorageRequest::AbortProjectSnapshot { snapshot_id } => {
            Some(writes.abort(plugin_id, snapshot_id))
        }
        _ => None,
    }
}

fn dispatch(
    semantic: &dyn SemanticPersistence,
    request: SemanticStorageRequest,
) -> lumvise_db_core::Result<Value> {
    match request {
        SemanticStorageRequest::SyncStructure {
            project_root,
            elements,
            relationships,
        } => report_value(execute(
            semantic,
            SemanticOperation::SyncStructure {
                project_root,
                elements,
                relationships,
            },
        )?),
        SemanticStorageRequest::SyncPartition {
            partition,
            elements,
            relationships,
        } => report_value(execute(
            semantic,
            SemanticOperation::SyncPartition {
                partition,
                elements,
                relationships,
            },
        )?),
        SemanticStorageRequest::StoreElementNameVectors {
            project_root,
            vectors,
        } => match execute(
            semantic,
            SemanticOperation::StoreElementNameVectors {
                project_root,
                vectors,
            },
        )? {
            SemanticResult::StoredElementNameVectors { count } => Ok(json!({"stored": count})),
            result => unexpected_result("StoredElementNameVectors", result),
        },
        SemanticStorageRequest::Element {
            semantic_element_id,
        } => match execute(
            semantic,
            SemanticOperation::Element {
                semantic_element_id,
            },
        )? {
            SemanticResult::Element(element) => Ok(json!({"element": element})),
            result => unexpected_result("Element", result),
        },
        SemanticStorageRequest::ElementsByIds {
            project_root,
            semantic_element_ids,
        } => elements_value(execute(
            semantic,
            SemanticOperation::ElementsByIds {
                project_root,
                semantic_element_ids,
            },
        )?),
        SemanticStorageRequest::ElementsByIdsIncludingInactive {
            project_root,
            semantic_element_ids,
        } => elements_value(execute(
            semantic,
            SemanticOperation::ElementsByIdsIncludingInactive {
                project_root,
                semantic_element_ids,
            },
        )?),
        SemanticStorageRequest::ArtifactsByIds { artifact_ids } => artifacts_value(execute(
            semantic,
            SemanticOperation::ArtifactsByIds { artifact_ids },
        )?),
        SemanticStorageRequest::SearchElementCandidates {
            project_root,
            query,
            limit,
        } => elements_value(execute(
            semantic,
            SemanticOperation::SearchElementCandidates {
                project_root,
                query,
                limit,
            },
        )?),
        SemanticStorageRequest::SearchElementNameVectors {
            project_root,
            query,
            k,
            engine_id,
            model,
        } => match execute(
            semantic,
            SemanticOperation::SearchElementNameVectors {
                project_root,
                query,
                k,
                engine_id,
                model,
            },
        )? {
            SemanticResult::ElementVectorSearch(results) => Ok(json!({"results": results})),
            result => unexpected_result("ElementVectorSearch", result),
        },
        SemanticStorageRequest::SearchArtifactTextVectors {
            project_root,
            query,
            k,
            engine_id,
            model,
        } => match execute(
            semantic,
            SemanticOperation::SearchArtifactTextVectors {
                project_root,
                query,
                k,
                engine_id,
                model,
            },
        )? {
            SemanticResult::ArtifactVectorSearch(results) => Ok(json!({"results": results})),
            result => unexpected_result("ArtifactVectorSearch", result),
        },
        SemanticStorageRequest::StoreArtifactTextVectors {
            project_root,
            vectors,
        } => match execute(
            semantic,
            SemanticOperation::StoreArtifactTextVectors {
                project_root,
                vectors,
            },
        )? {
            SemanticResult::StoredArtifactTextVectors { count } => Ok(json!({"stored": count})),
            result => unexpected_result("StoredArtifactTextVectors", result),
        },
        SemanticStorageRequest::RelationshipsFrom {
            semantic_element_id,
        } => relationships_value(execute(
            semantic,
            SemanticOperation::RelationshipsFrom {
                semantic_element_id,
            },
        )?),
        SemanticStorageRequest::RelationshipsTouchingElements {
            semantic_element_ids,
        } => relationships_value(execute(
            semantic,
            SemanticOperation::RelationshipsTouchingElements {
                semantic_element_ids,
            },
        )?),
        SemanticStorageRequest::Artifact { artifact_id } => artifact_value(execute(
            semantic,
            SemanticOperation::Artifact { artifact_id },
        )?),
        SemanticStorageRequest::ArtifactsForElements {
            semantic_element_ids,
        } => artifacts_value(execute(
            semantic,
            SemanticOperation::ArtifactsForElements {
                semantic_element_ids,
            },
        )?),
        SemanticStorageRequest::ArtifactsForElementWithInheritance {
            semantic_element_id,
        } => artifacts_value(execute(
            semantic,
            SemanticOperation::ArtifactsForElementWithInheritance {
                semantic_element_id,
            },
        )?),
        SemanticStorageRequest::ArtifactDependents {
            target_kind,
            target_id,
        } => artifacts_value(execute(
            semantic,
            SemanticOperation::ArtifactDependents {
                target_kind,
                target_id,
            },
        )?),
        SemanticStorageRequest::UpsertArtifact {
            artifact,
            media_type,
        } => match execute(
            semantic,
            SemanticOperation::UpsertArtifact {
                artifact,
                media_type,
            },
        )? {
            SemanticResult::UpsertedArtifact { artifact_id } => {
                Ok(json!({"artifact_id": artifact_id}))
            }
            result => unexpected_result("UpsertedArtifact", result),
        },
        SemanticStorageRequest::RemoveArtifact { artifact_id } => removed_value(execute(
            semantic,
            SemanticOperation::RemoveArtifact { artifact_id },
        )?),
        SemanticStorageRequest::RemoveElement {
            semantic_element_id,
        } => removed_value(execute(
            semantic,
            SemanticOperation::RemoveElement {
                semantic_element_id,
            },
        )?),
        SemanticStorageRequest::ProjectRoots => {
            match execute(semantic, SemanticOperation::ProjectRoots)? {
                SemanticResult::ProjectRoots(project_roots) => {
                    Ok(json!({"project_roots": project_roots}))
                }
                result => unexpected_result("ProjectRoots", result),
            }
        }
        SemanticStorageRequest::SemanticRevision => {
            match execute(semantic, SemanticOperation::SemanticRevision)? {
                SemanticResult::SemanticRevision { commit_version } => {
                    Ok(json!({"commit_version": commit_version}))
                }
                result => unexpected_result("SemanticRevision", result),
            }
        }
        SemanticStorageRequest::ProjectArtifacts {
            project_root,
            artifact_namespace,
        } => artifacts_value(execute(
            semantic,
            SemanticOperation::ProjectArtifacts {
                project_root,
                artifact_namespace,
            },
        )?),
        SemanticStorageRequest::ProjectElementCounts { project_root } => {
            match execute(
                semantic,
                SemanticOperation::ProjectElementCounts { project_root },
            )? {
                SemanticResult::ProjectElementCounts {
                    commit_version,
                    published_at,
                    total_elements,
                    elements_by_kind,
                } => Ok(
                    json!({"commit_version":commit_version,"published_at":published_at,"total_elements":total_elements,"elements_by_kind":elements_by_kind}),
                ),
                result => unexpected_result("ProjectElementCounts", result),
            }
        }
        SemanticStorageRequest::ScopedRead { request } => {
            match execute(semantic, SemanticOperation::ScopedRead(request))? {
                SemanticResult::ScopedGraph(graph) => {
                    let root = graph.project_root.clone();
                    graph_records_value(serde_json::to_value(graph)?, &root)
                }
                result => unexpected_result("ScopedGraph", result),
            }
        }
        SemanticStorageRequest::ProjectSnapshot {
            scope,
            artifact_namespace,
        } => match execute(
            semantic,
            SemanticOperation::ProjectSnapshot {
                scope,
                artifact_namespace,
            },
        )? {
            SemanticResult::ProjectSnapshot(snapshot) => project_snapshot_value(snapshot),
            result => unexpected_result("ProjectSnapshot", result),
        },
        SemanticStorageRequest::SelectiveSubgraph {
            project_root,
            root_element_id,
            artifact_namespace,
        } => match execute(
            semantic,
            SemanticOperation::SelectiveSubgraph {
                project_root,
                root_element_id,
                artifact_namespace,
            },
        )? {
            SemanticResult::SelectiveSubgraph(subgraph) => selective_subgraph_value(subgraph),
            result => unexpected_result("SelectiveSubgraph", result),
        },
        SemanticStorageRequest::ProjectRendererGraph {
            project_root,
            target_path,
            granularity,
            recursive,
            include_external,
            include_first_neighbors,
        } => match execute(
            semantic,
            SemanticOperation::ProjectRendererGraph(SemanticGraphProjectionRequest {
                project_root,
                target_path,
                granularity,
                recursive,
                include_external,
                include_first_neighbors,
            }),
        )? {
            SemanticResult::RendererGraphProjection(projection) => {
                Ok(serde_json::to_value(projection)?)
            }
            result => unexpected_result("RendererGraphProjection", result),
        },
        SemanticStorageRequest::CandidateSourceElements {
            content_fingerprints,
            kind_name_keys,
            kind_file_name_keys,
        } => elements_value(execute(
            semantic,
            SemanticOperation::CandidateSourceElements {
                content_fingerprints,
                kind_name_keys,
                kind_file_name_keys,
            },
        )?),
        SemanticStorageRequest::BeginProjectSnapshot { .. }
        | SemanticStorageRequest::StageProjectSnapshot { .. }
        | SemanticStorageRequest::CommitProjectSnapshot { .. }
        | SemanticStorageRequest::AbortProjectSnapshot { .. } => {
            unreachable!("snapshot requests are dispatched before persistence")
        }
    }
}

fn execute(
    semantic: &dyn SemanticPersistence,
    operation: SemanticOperation,
) -> lumvise_db_core::Result<SemanticResult> {
    semantic.execute(operation, &InvocationControl::sixty_seconds())
}

fn report_value(result: SemanticResult) -> lumvise_db_core::Result<Value> {
    match result {
        SemanticResult::SyncStructure(report) | SemanticResult::SyncPartition(report) => {
            Ok(json!({"report": report}))
        }
        result => unexpected_result("SemanticBatchSyncReport", result),
    }
}

fn elements_value(result: SemanticResult) -> lumvise_db_core::Result<Value> {
    match result {
        SemanticResult::Elements(elements) => Ok(json!({"elements": elements})),
        result => unexpected_result("Elements", result),
    }
}

fn relationships_value(result: SemanticResult) -> lumvise_db_core::Result<Value> {
    match result {
        SemanticResult::Relationships(relationships) => Ok(json!({"relationships": relationships})),
        result => unexpected_result("Relationships", result),
    }
}

fn selective_subgraph_value(
    subgraph: Option<lumvise_db_core::SemanticSelectiveSubgraph>,
) -> lumvise_db_core::Result<Value> {
    let Some(mut subgraph) = subgraph else {
        return Ok(json!({"subgraph": Value::Null}));
    };
    let project_root = subgraph.project_root.clone();
    let mut value = serde_json::to_value(&mut subgraph)?;
    for element in value["elements"].as_array_mut().into_iter().flatten() {
        element
            .as_object_mut()
            .map(|fields| fields.remove("match_evidence"));
    }
    for element in value["external_elements"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        element
            .as_object_mut()
            .map(|fields| fields.remove("match_evidence"));
    }
    for relationship in value["relationships"].as_array_mut().into_iter().flatten() {
        relationship["lifecycle"] = json!("active");
    }
    for artifact in value["artifacts"].as_array_mut().into_iter().flatten() {
        artifact["project_root"] = json!(&project_root);
    }
    Ok(json!({"subgraph": value}))
}

fn artifact_value(result: SemanticResult) -> lumvise_db_core::Result<Value> {
    match result {
        SemanticResult::Artifact(artifact) => Ok(json!({"artifact": artifact})),
        result => unexpected_result("Artifact", result),
    }
}

fn artifacts_value(result: SemanticResult) -> lumvise_db_core::Result<Value> {
    match result {
        SemanticResult::Artifacts(artifacts) => Ok(json!({"artifacts": artifacts})),
        result => unexpected_result("Artifacts", result),
    }
}

fn removed_value(result: SemanticResult) -> lumvise_db_core::Result<Value> {
    match result {
        SemanticResult::Removed { removed } => Ok(json!({"removed": removed})),
        result => unexpected_result("Removed", result),
    }
}

fn unexpected_result(expected: &str, result: SemanticResult) -> lumvise_db_core::Result<Value> {
    Err(lumvise_db_core::DbError::invalid_value(
        format!("{result:?}"),
        format!("semantic persistence result {expected}"),
    ))
}

fn project_snapshot_value(
    snapshot: lumvise_db_core::SemanticProjectSnapshot,
) -> lumvise_db_core::Result<Value> {
    let project_root = snapshot.project_root.clone();
    graph_records_value(serde_json::to_value(snapshot)?, &project_root)
}

fn graph_records_value(mut value: Value, project_root: &str) -> lumvise_db_core::Result<Value> {
    for element in value["elements"].as_array_mut().into_iter().flatten() {
        element
            .as_object_mut()
            .map(|fields| fields.remove("match_evidence"));
    }
    for relationship in value["relationships"].as_array_mut().into_iter().flatten() {
        relationship["lifecycle"] = json!("active");
    }
    for artifact in value["artifacts"].as_array_mut().into_iter().flatten() {
        artifact["project_root"] = json!(&project_root);
    }
    Ok(value)
}

fn authorize(plugin_id: &str, request: &SemanticStorageRequest) -> Result<(), HostCapabilityError> {
    let allowed = match request {
        SemanticStorageRequest::SyncStructure { .. }
        | SemanticStorageRequest::SyncPartition { .. }
        | SemanticStorageRequest::BeginProjectSnapshot { .. }
        | SemanticStorageRequest::StageProjectSnapshot { .. }
        | SemanticStorageRequest::CommitProjectSnapshot { .. }
        | SemanticStorageRequest::AbortProjectSnapshot { .. }
        | SemanticStorageRequest::RemoveElement { .. }
        | SemanticStorageRequest::StoreElementNameVectors { .. }
        | SemanticStorageRequest::StoreArtifactTextVectors { .. }
        | SemanticStorageRequest::ProjectRendererGraph { .. } => plugin_id == SEMANTIC_PLUGIN_ID,
        SemanticStorageRequest::ProjectArtifacts { .. }
        | SemanticStorageRequest::ElementsByIdsIncludingInactive { .. }
        | SemanticStorageRequest::CandidateSourceElements { .. } => {
            plugin_id == KNOWLEDGE_PLUGIN_ID
        }
        SemanticStorageRequest::SelectiveSubgraph { .. } => plugin_id == KNOWLEDGE_PLUGIN_ID,
        SemanticStorageRequest::ArtifactsByIds { .. } => plugin_id == SEMANTIC_PLUGIN_ID,
        SemanticStorageRequest::UpsertArtifact { .. }
        | SemanticStorageRequest::RemoveArtifact { .. } => {
            matches!(plugin_id, SEMANTIC_PLUGIN_ID | KNOWLEDGE_PLUGIN_ID)
        }
        SemanticStorageRequest::ProjectRoots => matches!(
            plugin_id,
            SEMANTIC_PLUGIN_ID | KNOWLEDGE_PLUGIN_ID | NUCLEUS_PLUGIN_ID | "builtin.assistant"
        ),
        _ => matches!(
            plugin_id,
            SEMANTIC_PLUGIN_ID | KNOWLEDGE_PLUGIN_ID | NUCLEUS_PLUGIN_ID
        ),
    };
    if allowed {
        Ok(())
    } else {
        Err(HostCapabilityError::new(
            SEMANTIC_STORAGE,
            "host_capability_forbidden",
            format!("plugin `{plugin_id}` cannot perform semantic storage request `{request:?}`"),
            false,
        ))
    }
}

fn default_true() -> bool {
    true
}

fn default_media_type() -> String {
    "text/markdown; charset=utf-8".into()
}

fn invalid_input(input: &Value, error: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        SEMANTIC_STORAGE,
        "invalid_host_capability_input",
        format!("invalid storage.semantic input `{input}`; expected version 1 request: {error}"),
        false,
    )
}

fn execution_failed(error: lumvise_db_core::DbError) -> HostCapabilityError {
    HostCapabilityError::new(
        SEMANTIC_STORAGE,
        "host_capability_execution_failed",
        format!("storage.semantic operation failed: {error}"),
        false,
    )
}

#[cfg(test)]
mod tests {
    use lumvise_db_core::{
        LocalPersistence, ProjectSnapshotScope, RelationalOperation, RelationalPersistence,
        RelationalResult, SemanticOperation, SemanticPersistence, SemanticResult,
    };
    use lumvise_resource_routing::InvocationControl;
    use std::sync::Arc;

    fn broker_for(db: Arc<LocalPersistence>) -> AppCoreHostCapabilityBroker {
        let semantic: Arc<dyn SemanticPersistence> = db.clone();
        let relational: Arc<dyn RelationalPersistence> = db;
        AppCoreHostCapabilityBroker::new(semantic, relational)
    }

    fn semantic_result(db: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
        <LocalPersistence as SemanticPersistence>::execute(
            db,
            operation,
            &InvocationControl::sixty_seconds(),
        )
        .expect("semantic persistence operation")
    }

    fn relational_result(
        db: &LocalPersistence,
        operation: RelationalOperation,
    ) -> RelationalResult {
        <LocalPersistence as RelationalPersistence>::execute(
            db,
            operation,
            &InvocationControl::sixty_seconds(),
        )
        .expect("relational persistence operation")
    }

    fn active_project_elements(db: &LocalPersistence) -> Vec<lumvise_db_core::SemanticElement> {
        match semantic_result(
            db,
            SemanticOperation::ProjectSnapshot {
                scope: ProjectSnapshotScope::ProjectRoot("/repo".into()),
                artifact_namespace: None,
            },
        ) {
            SemanticResult::ProjectSnapshot(snapshot) => snapshot
                .elements
                .into_iter()
                .filter(|element| element.lifecycle == "active")
                .collect(),
            result => panic!("unexpected project snapshot result: {result:?}"),
        }
    }
    fn artifact(db: &LocalPersistence, artifact_id: &str) -> lumvise_db_core::SemanticArtifact {
        match semantic_result(
            db,
            SemanticOperation::Artifact {
                artifact_id: artifact_id.into(),
            },
        ) {
            SemanticResult::Artifact(Some(artifact)) => artifact,
            result => panic!("unexpected artifact result: {result:?}"),
        }
    }

    fn artifact_blob_len(db: &LocalPersistence, content_ref: &str) -> usize {
        match semantic_result(
            db,
            SemanticOperation::ArtifactBlobGet {
                content_ref: content_ref.into(),
            },
        ) {
            SemanticResult::ArtifactBlob(Some(blob)) => blob.content.len(),
            result => panic!("unexpected artifact blob result: {result:?}"),
        }
    }

    fn plugin_data_is_empty(db: &LocalPersistence, plugin_id: &str, table_name: &str) -> bool {
        matches!(
            relational_result(
                db,
                RelationalOperation::ListPluginData {
                    plugin_id: plugin_id.into(),
                    table_name: table_name.into(),
                },
            ),
            RelationalResult::PluginDataRows(rows) if rows.is_empty()
        )
    }

    fn persistence() -> Arc<LocalPersistence> {
        Arc::new(LocalPersistence::in_memory().expect("local persistence"))
    }

    use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};

    use super::*;
    use crate::AppCoreHostCapabilityBroker;

    #[test]
    fn unrelated_plugin_cannot_write_semantic_structure() {
        let broker = broker_for(persistence());
        let error = broker.invoke(request(
            "third.party",
            json!({"operation": "sync_structure", "project_root": "/repo",
                "elements": [], "relationships": []}),
        ));
        assert!(error.is_err());
    }

    #[test]
    fn assistant_may_read_project_roots_without_semantic_write_access() {
        let broker = broker_for(persistence());
        let roots = broker
            .invoke(request(
                "builtin.assistant",
                json!({"operation":"project_roots"}),
            ))
            .expect("Assistant project membership read");
        assert_eq!(roots["project_roots"], json!([]));
        assert!(
            broker
                .invoke(request(
                    "builtin.assistant",
                    json!({"operation":"sync_structure",
            "project_root":"/repo", "elements":[], "relationships":[]})
                ))
                .is_err()
        );
    }

    #[test]
    fn semantic_plugin_stores_and_searches_native_element_vectors() {
        let broker = broker_for(persistence());
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "sync_structure", "project_root": "/repo",
                    "elements": [element_for("/repo", "near"), element_for("/repo", "far")],
                    "relationships": []}),
            ))
            .unwrap();
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({
                    "operation": "store_element_name_vectors",
                    "project_root": "/repo",
                    "vectors": [
                        {"semantic_element_id": "near", "project_root": "/repo",
                            "source_text": "lib.rs",
                            "vector": {"engine_id": "neural.embed", "model": null,
                                "dimensions": 2, "vector": [1.0, 0.0],
                                "normalized": false, "metadata": {}}},
                        {"semantic_element_id": "far", "project_root": "/repo",
                            "source_text": "lib.rs",
                            "vector": {"engine_id": "neural.embed", "model": null,
                                "dimensions": 2, "vector": [0.0, 1.0],
                                "normalized": false, "metadata": {}}}
                    ]
                }),
            ))
            .unwrap();

        let output = broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "search_element_name_vectors", "project_root": "/repo",
                    "query": [1.0, 0.0], "k": 1, "engine_id": "neural.embed"}),
            ))
            .unwrap();
        assert_eq!(output["results"].as_array().unwrap().len(), 1);
        assert_eq!(output["results"][0]["id"], "near");
    }

    #[test]
    fn staged_project_snapshot_is_invisible_until_atomic_commit() {
        let db = persistence();
        let broker = broker_for(Arc::clone(&db));
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "sync_structure", "project_root": "/repo",
                    "elements": [element_for("/repo", "old")], "relationships": []}),
            ))
            .unwrap();
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "begin_project_snapshot", "snapshot_id": "job-1",
                    "project_root": "/repo", "partition_paths": [], "page_count": 2}),
            ))
            .unwrap();
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({
                    "operation": "stage_project_snapshot", "snapshot_id": "job-1", "page_index": 0,
                    "elements": [element_for("/repo", "first")], "relationships": []
                }),
            ))
            .unwrap();

        let before = active_project_elements(db.as_ref());
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].semantic_element_id, "old");

        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({
                    "operation": "stage_project_snapshot", "snapshot_id": "job-1", "page_index": 1,
                    "elements": [element_for("/repo", "second")], "relationships": []
                }),
            ))
            .unwrap();
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({
                    "operation": "commit_project_snapshot", "snapshot_id": "job-1"
                }),
            ))
            .unwrap();

        let output = broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "elements_by_ids", "project_root": "/repo",
                    "semantic_element_ids": ["second", "missing"]}),
            ))
            .unwrap();
        let after = active_project_elements(db.as_ref());
        assert_eq!(
            after
                .iter()
                .filter(|element| element.lifecycle == "active")
                .count(),
            2
        );
        assert!(
            after
                .iter()
                .all(|element| element.semantic_element_id != "old")
        );
        assert_eq!(output["elements"].as_array().unwrap().len(), 1);
        assert_eq!(output["elements"][0]["semantic_element_id"], "second");
    }

    #[test]
    fn staged_project_snapshot_rejects_out_of_order_pages() {
        let broker = broker_for(persistence());
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({
                    "operation": "begin_project_snapshot", "snapshot_id": "job-order",
                    "project_root": "/repo", "partition_paths": [], "page_count": 2
                }),
            ))
            .unwrap();

        let error = broker.invoke(request(SEMANTIC_PLUGIN_ID, json!({
            "operation": "stage_project_snapshot", "snapshot_id": "job-order", "page_index": 1,
            "elements": [], "relationships": []
        }))).unwrap_err();

        assert!(format!("{error:?}").contains("semantic_snapshot_page_out_of_order"));
    }

    #[test]
    fn project_snapshot_returns_one_complete_protobuf_value() {
        let db = persistence();
        let broker = broker_for(Arc::clone(&db));
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "sync_structure", "project_root": "/repo",
                    "elements": [element_for("/repo", "first"), element_for("/repo", "second")],
                    "relationships": [{"project_root": "/repo", "source_element_id": "first",
                        "target_element_id": "second", "relationship_kind": "calls",
                        "label": "calls", "metadata": {}}]}),
            ))
            .unwrap();

        let snapshot = broker
            .invoke(request(
                KNOWLEDGE_PLUGIN_ID,
                json!({"operation": "project_snapshot", "scope": {"project_root": "/repo"},
                    "artifact_namespace": "knowledge"}),
            ))
            .unwrap();

        assert_eq!(snapshot["elements"].as_array().unwrap().len(), 2);
        assert_eq!(snapshot["relationships"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["relationships"][0]["lifecycle"], "active");
        assert!(snapshot.get("snapshot_id").is_none());
        assert!(snapshot.get("next_offset").is_none());
    }

    #[test]
    fn large_knowledge_content_spills_to_sql_and_keeps_graph_node() {
        let db = persistence();
        let broker = broker_for(Arc::clone(&db));
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "sync_structure", "project_root": "/repo",
                    "elements": [element()], "relationships": []}),
            ))
            .unwrap();
        let content = "x".repeat(8 * 1024 + 1);
        broker
            .invoke(request(
                KNOWLEDGE_PLUGIN_ID,
                json!({"operation": "upsert_artifact", "artifact": {
                    "artifact_id": "knowledge-1", "semantic_element_id": "element-1",
                    "artifact_kind": "decision", "title": "Decision",
                    "content_ref": null, "content": content,
                    "searchable_text": null, "content_size_bytes": null,
                    "metadata": {"knowledge": {}}
                }}),
            ))
            .unwrap();
        let artifact = artifact(db.as_ref(), "knowledge-1");
        assert_eq!(artifact.content.as_deref(), Some(content.as_str()));
        let content_ref = artifact.content_ref.unwrap();
        assert_eq!(artifact_blob_len(db.as_ref(), &content_ref), 8 * 1024 + 1);
        let snapshot = broker
            .invoke(request(
                KNOWLEDGE_PLUGIN_ID,
                json!({"operation": "project_snapshot", "scope": {"project_root": "/repo"},
                    "artifact_namespace": "knowledge"}),
            ))
            .unwrap();
        assert_eq!(snapshot["artifacts"][0]["content"], content);
        let attached = broker
            .invoke(request(
                KNOWLEDGE_PLUGIN_ID,
                json!({"operation": "artifacts_for_elements",
                    "semantic_element_ids": ["element-1"]}),
            ))
            .unwrap();
        assert_eq!(attached["artifacts"][0]["content"], content);
        assert!(plugin_data_is_empty(
            db.as_ref(),
            SEMANTIC_PLUGIN_ID,
            "semantic_elements",
        ));
        assert!(plugin_data_is_empty(
            db.as_ref(),
            KNOWLEDGE_PLUGIN_ID,
            "knowledge_artifacts",
        ));
    }

    #[test]
    fn project_artifact_read_excludes_artifacts_owned_by_other_projects() {
        let db = persistence();
        let broker = broker_for(Arc::clone(&db));
        for (project_root, element_id) in [("/repo-a", "element-a"), ("/repo-b", "element-b")] {
            broker
                .invoke(request(
                    SEMANTIC_PLUGIN_ID,
                    json!({"operation": "sync_structure", "project_root": project_root,
                        "elements": [element_for(project_root, element_id)], "relationships": []}),
                ))
                .unwrap();
            broker
                .invoke(request(
                    KNOWLEDGE_PLUGIN_ID,
                    json!({"operation": "upsert_artifact", "artifact": {
                        "artifact_id": format!("artifact-{element_id}"),
                        "semantic_element_id": element_id, "artifact_kind": "definition",
                        "title": element_id, "content_ref": null, "content": "content",
                        "searchable_text": null, "content_size_bytes": null,
                        "metadata": {"knowledge": {}}
                    }}),
                ))
                .unwrap();
        }
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "upsert_artifact", "artifact": {
                    "artifact_id": "source-artifact", "semantic_element_id": "element-a",
                    "artifact_kind": "source", "title": "Source", "content_ref": null,
                    "content": "source", "searchable_text": null,
                    "content_size_bytes": null, "metadata": {"source": {}}
                }}),
            ))
            .unwrap();

        let output = broker
            .invoke(request(
                KNOWLEDGE_PLUGIN_ID,
                json!({"operation": "project_snapshot", "scope": {"project_root": "/repo-a"},
                    "artifact_namespace": "knowledge"}),
            ))
            .unwrap();

        assert_eq!(output["artifacts"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn candidate_source_elements_is_authorized_for_knowledge_only() {
        let broker = broker_for(persistence());
        let denied = broker.invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "candidate_source_elements", "content_fingerprints": [],
                "kind_name_keys": [], "kind_file_name_keys": []}),
        ));
        assert!(denied.is_err());
        let allowed = broker.invoke(request(
            KNOWLEDGE_PLUGIN_ID,
            json!({"operation": "candidate_source_elements", "content_fingerprints": [],
                "kind_name_keys": [], "kind_file_name_keys": []}),
        ));
        assert!(allowed.is_ok());
    }

    #[test]
    fn candidate_source_elements_unions_identity_key_matches_across_projects() {
        let db = persistence();
        let broker = broker_for(Arc::clone(&db));
        let mut fingerprint_source = element_for("/repo-a", "fp-source");
        fingerprint_source["content_fingerprint"] = json!("fp1:0000000000000001:same-hash");
        let mut fingerprint_twin = element_for("/repo-b", "fp-twin");
        fingerprint_twin["content_fingerprint"] = json!("fp1:0000000000000001:same-hash");
        let mut kind_name_twin = element_for("/repo-b", "kind-name-twin");
        kind_name_twin["name"] = json!("Shared");
        kind_name_twin["element_kind"] = json!("function");
        let mut kind_file_twin = element_for("/repo-b", "kind-file-twin");
        kind_file_twin["path"] = json!("other/shared.rs");
        kind_file_twin["element_kind"] = json!("file");
        let unrelated = element_for("/repo-b", "unrelated");
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "sync_structure", "project_root": "/repo-a",
                    "elements": [fingerprint_source], "relationships": []}),
            ))
            .unwrap();
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "sync_structure", "project_root": "/repo-b",
                    "elements": [fingerprint_twin, kind_name_twin, kind_file_twin, unrelated],
                    "relationships": []}),
            ))
            .unwrap();

        let output = broker
            .invoke(request(
                KNOWLEDGE_PLUGIN_ID,
                json!({"operation": "candidate_source_elements",
                    "content_fingerprints": ["fp1:0000000000000001:same-hash"],
                    "kind_name_keys": ["function\u{1}shared"],
                    "kind_file_name_keys": ["file\u{1}shared.rs"]}),
            ))
            .unwrap();
        let mut ids = output["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|element| element["semantic_element_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            vec!["fp-source", "fp-twin", "kind-file-twin", "kind-name-twin"]
        );
    }

    fn element() -> Value {
        element_for("/repo", "element-1")
    }

    fn element_for(project_root: &str, semantic_element_id: &str) -> Value {
        json!({
            "project_root": project_root, "semantic_element_id": semantic_element_id,
            "semantic_source_id": "source-1", "path": "src/lib.rs",
            "element_kind": "file", "name": "lib.rs", "parent_element_id": null,
            "content_fingerprint": null, "start_line": null, "end_line": null,
            "lifecycle": "active", "match_evidence": null, "metadata": {}
        })
    }

    fn request(plugin_id: &str, input: Value) -> HostCapabilityRequest {
        HostCapabilityRequest {
            plugin_id: plugin_id.into(),
            invocation_id: "invocation".into(),
            call_id: "call".into(),
            capability_id: SEMANTIC_STORAGE.into(),
            required_version: "^1.0".into(),
            input,
        }
    }
}
