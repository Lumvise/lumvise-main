//! Versioned graph-storage protocol for compiled semantic plugins.
//!
//! Plugins own semantic rules. This adapter only authorizes coarse operations,
//! converts protocol values, and delegates to the selected semantic persistence
//! boundary through owned requests and results.

mod attachment_copy;
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
use super::semantic_snapshot_writes::{SemanticSnapshotPage, SemanticSnapshotWrites};

const SEMANTIC_PLUGIN_ID: &str = "builtin.semantic";
const KNOWLEDGE_PLUGIN_ID: &str = "builtin.knowledge";
const NUCLEUS_PLUGIN_ID: &str = "builtin.nucleus";

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum SemanticStorageRequest {
    ProjectIdentity {
        project_root: String,
    },
    RevisionSelection {
        project_root: String,
        selection_id: String,
    },
    SelectRevisions {
        update: lumvise_db_core::SemanticRevisionSelectionUpdate,
    },
    PlanRevisionRetention {
        project_root: String,
        operation_id: String,
        policy: lumvise_db_core::SemanticRevisionRetentionPolicy,
    },
    CollectRevisionRetention {
        plan: lumvise_db_core::SemanticRevisionRetentionPlan,
        policy: lumvise_db_core::SemanticRevisionRetentionPolicy,
    },
    RevisionSelectionMembers {
        query: lumvise_db_core::SemanticRevisionSelectionQuery,
    },
    RevisionFamilies {
        query: lumvise_db_core::SemanticRevisionFamilyQuery,
    },
    ResolveRevision {
        project_root: String,
        record: lumvise_db_core::SemanticRecordRef,
    },
    PublishRevisions {
        publication: lumvise_db_core::SemanticRevisionPublication,
    },
    CopyArtifactAttachment {
        source_artifact_id: String,
        target_artifact_id: String,
        content_ref: String,
    },
    SyncStructure {
        project_root: String,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    },
    BeginProjectSnapshot {
        snapshot_id: String,
        project_root: String,
        #[serde(default)]
        provider_instance_id: Option<String>,
        partition_paths: Vec<String>,
        page_count: usize,
        observation: lumvise_contracts::SemanticIndexObservation,
    },
    StageProjectSnapshot {
        snapshot_id: String,
        page_index: usize,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
        #[serde(default)]
        sources: Vec<lumvise_contracts::SemanticSourceUpsert>,
        #[serde(default)]
        artifacts: Vec<lumvise_db_core::SemanticArtifactRevisionWrite>,
        #[serde(default)]
        removed_artifact_ids: Vec<String>,
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
            provider_instance_id,
            partition_paths,
            page_count,
            observation,
        } => Some(writes.begin(
            plugin_id,
            snapshot_id,
            project_root,
            provider_instance_id.clone(),
            partition_paths.clone(),
            *page_count,
            observation.clone(),
        )),
        SemanticStorageRequest::StageProjectSnapshot {
            snapshot_id,
            page_index,
            elements,
            relationships,
            sources,
            artifacts,
            removed_artifact_ids,
        } => Some(writes.stage(
            plugin_id,
            snapshot_id,
            SemanticSnapshotPage {
                page_index: *page_index,
                elements: elements.clone(),
                relationships: relationships.clone(),
                sources: sources.clone(),
                artifacts: artifacts.clone(),
                removed_artifact_ids: removed_artifact_ids.clone(),
            },
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
        SemanticStorageRequest::RevisionSelection {
            project_root,
            selection_id,
        } => {
            match execute(
                semantic,
                SemanticOperation::RevisionSelection {
                    project_root,
                    selection_id,
                },
            )? {
                SemanticResult::RevisionSelection(selection) => Ok(json!({"selection": selection})),
                result => unexpected_result("RevisionSelection", result),
            }
        }
        SemanticStorageRequest::SelectRevisions { update } => {
            match execute(semantic, SemanticOperation::SelectRevisions(update))? {
                SemanticResult::RevisionPublication(publication) => {
                    Ok(json!({"publication": publication}))
                }
                result => unexpected_result("RevisionPublication", result),
            }
        }
        SemanticStorageRequest::PlanRevisionRetention {
            project_root,
            operation_id,
            policy,
        } => {
            match execute(
                semantic,
                SemanticOperation::PlanRevisionRetention {
                    project_root,
                    operation_id,
                    policy,
                },
            )? {
                SemanticResult::RevisionRetentionPlan(plan) => Ok(json!({"plan": plan})),
                result => unexpected_result("RevisionRetentionPlan", result),
            }
        }
        SemanticStorageRequest::CollectRevisionRetention { plan, policy } => {
            match execute(
                semantic,
                SemanticOperation::CollectRevisionRetention { plan, policy },
            )? {
                SemanticResult::RevisionRetention(receipt) => Ok(json!({"receipt": receipt})),
                result => unexpected_result("RevisionRetention", result),
            }
        }
        SemanticStorageRequest::ProjectIdentity { project_root } => {
            match execute(
                semantic,
                SemanticOperation::ProjectIdentity { project_root },
            )? {
                SemanticResult::ProjectIdentity(identity) => Ok(json!({"identity": identity})),
                result => unexpected_result("ProjectIdentity", result),
            }
        }
        SemanticStorageRequest::RevisionSelectionMembers { query } => {
            match execute(semantic, SemanticOperation::RevisionSelectionMembers(query))? {
                SemanticResult::RevisionObservation(membership) => {
                    Ok(json!({"membership": membership}))
                }
                result => unexpected_result("RevisionMembership", result),
            }
        }
        SemanticStorageRequest::RevisionFamilies { query } => {
            match execute(semantic, SemanticOperation::RevisionFamilies(query))? {
                SemanticResult::RevisionFamilies(remaps) => Ok(json!({"remaps":remaps})),
                result => unexpected_result("RevisionFamilies", result),
            }
        }
        SemanticStorageRequest::ResolveRevision {
            project_root,
            record,
        } => {
            match execute(
                semantic,
                SemanticOperation::ResolveRevision {
                    project_root,
                    record,
                },
            )? {
                SemanticResult::RevisionResolution(revision) => Ok(json!({"revision":revision})),
                result => unexpected_result("RevisionResolution", result),
            }
        }
        SemanticStorageRequest::PublishRevisions { publication } => {
            match execute(semantic, SemanticOperation::PublishRevisions(publication))? {
                SemanticResult::RevisionPublication(publication) => {
                    Ok(json!({"publication": publication}))
                }
                result => unexpected_result("RevisionPublication", result),
            }
        }
        SemanticStorageRequest::CopyArtifactAttachment {
            source_artifact_id,
            target_artifact_id,
            content_ref,
        } => attachment_copy::copy_attachment(
            semantic,
            &source_artifact_id,
            &target_artifact_id,
            &content_ref,
        ),
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
        SemanticStorageRequest::RevisionSelection { .. }
        | SemanticStorageRequest::SelectRevisions { .. }
        | SemanticStorageRequest::PlanRevisionRetention { .. }
        | SemanticStorageRequest::CollectRevisionRetention { .. } => {
            plugin_id == SEMANTIC_PLUGIN_ID
        }
        SemanticStorageRequest::PublishRevisions { publication } => {
            plugin_id == SEMANTIC_PLUGIN_ID
                || (plugin_id == KNOWLEDGE_PLUGIN_ID && publication.structure.is_none())
        }
        SemanticStorageRequest::ProjectIdentity { .. }
        | SemanticStorageRequest::RevisionSelectionMembers { .. }
        | SemanticStorageRequest::RevisionFamilies { .. }
        | SemanticStorageRequest::ResolveRevision { .. } => {
            matches!(plugin_id, SEMANTIC_PLUGIN_ID | KNOWLEDGE_PLUGIN_ID)
        }
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
        SemanticStorageRequest::ProjectArtifacts { .. } => {
            matches!(plugin_id, SEMANTIC_PLUGIN_ID | KNOWLEDGE_PLUGIN_ID)
        }
        SemanticStorageRequest::CopyArtifactAttachment { .. }
        | SemanticStorageRequest::ElementsByIdsIncludingInactive { .. }
        | SemanticStorageRequest::CandidateSourceElements { .. } => {
            plugin_id == KNOWLEDGE_PLUGIN_ID
        }
        SemanticStorageRequest::SelectiveSubgraph { .. } => plugin_id == KNOWLEDGE_PLUGIN_ID,
        SemanticStorageRequest::ArtifactsByIds { .. } => {
            matches!(plugin_id, SEMANTIC_PLUGIN_ID | KNOWLEDGE_PLUGIN_ID)
        }
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
mod tests;
