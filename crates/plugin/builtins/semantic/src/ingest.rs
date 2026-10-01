use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use crate::{
    models::{
        IndexBatchRequest, IndexLogRequest, LatestIndexLogRequest, SemanticArtifact,
        SemanticElement, SemanticRelationship, SemanticRelationshipInput, SemanticSource,
    },
    parse, require_non_empty,
    storage::{self, INDEX_LOGS, INDEX_LOGS_BY_PROVIDER, SOURCES},
};

pub(crate) fn ingest(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let batch: IndexBatchRequest = parse(input, "semantic index batch")?;
    validate_batch(&batch)?;
    if batch.ingestion_job_id.is_some() {
        return ingest_snapshot_page(&batch, context);
    }
    let referenced_ids = referenced_existing_element_ids(&batch);
    let existing_elements = if referenced_ids.is_empty() {
        Vec::new()
    } else {
        storage::graph_elements_by_ids(context, &batch.project_root, &referenced_ids)?
    };
    let incoming_elements = incoming_elements(&batch, &existing_elements);
    let incoming_relationships =
        incoming_relationships(&batch, &incoming_elements, &existing_elements);
    let mut source_mutations = Vec::new();
    store_sources(&mut source_mutations, &batch)?;
    storage::apply_mutations(context, &source_mutations)?;
    sync_graph(context, &batch, &incoming_elements, &incoming_relationships)?;
    let removed_artifacts = remove_graph_artifacts(context, &batch)?;
    let indexed_artifacts = store_graph_artifacts(context, &batch, &incoming_elements)?;
    Ok(json!({
        "accepted": true,
        "semantic_sources_upserted": batch.semantic_sources.len(),
        "semantic_elements_upserted": incoming_elements.len(),
        "semantic_relationships_upserted": incoming_relationships.len(),
        "semantic_artifacts_upserted": indexed_artifacts.len(),
        "semantic_artifacts_removed": removed_artifacts,
        "ingestion_job_id": null,
        "ingestion_status": "completed",
        "snapshot_freshness": {
            "status": "fresh", "latest_completed_snapshot": null,
            "active_ingestion_job_id": null
        }
    }))
}

fn ingest_snapshot_page(
    batch: &IndexBatchRequest,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let job_id = batch
        .ingestion_job_id
        .as_deref()
        .ok_or_else(|| invalid_snapshot_page(batch))?;
    let page_index = batch
        .ingestion_page_index
        .ok_or_else(|| invalid_snapshot_page(batch))?;
    let page_count = batch
        .ingestion_page_count
        .ok_or_else(|| invalid_snapshot_page(batch))?;
    if !batch.semantic_artifacts.is_empty() || !batch.removed_artifact_ids.is_empty() {
        return Err(PluginError::new(
            "invalid_semantic_snapshot_page",
            format!(
                "semantic snapshot `{job_id}` page `{page_index}` contains provider artifact mutations; expected graph-only index pages and Knowledge-owned artifacts through the Knowledge interface"
            ),
            false,
        ));
    }
    if page_index == 0 {
        storage::begin_project_snapshot(
            context,
            job_id,
            &batch.project_root,
            &partition_paths(batch),
            page_count,
        )?;
    }
    let elements = incoming_elements(batch, &[]);
    let relationships = incoming_relationships_unchecked(batch, &elements);
    let mut source_mutations = Vec::new();
    store_sources(&mut source_mutations, batch)?;
    storage::apply_mutations(context, &source_mutations)?;
    storage::stage_project_snapshot(
        context,
        job_id,
        page_index,
        &elements.values().collect::<Vec<_>>(),
        &relationships.values().collect::<Vec<_>>(),
    )?;
    let completed = page_index + 1 == page_count;
    if completed {
        storage::commit_project_snapshot(context, job_id)?;
    }
    Ok(json!({
        "accepted": true,
        "semantic_sources_upserted": batch.semantic_sources.len(),
        "semantic_elements_upserted": elements.len(),
        "semantic_relationships_upserted": relationships.len(),
        "semantic_artifacts_upserted": 0,
        "semantic_artifacts_removed": 0,
        "ingestion_job_id": job_id,
        "ingestion_status": if completed { "completed" } else { "running" },
        "snapshot_freshness": {
            "status": if completed { "fresh" } else { "running" },
            "latest_completed_snapshot": null,
            "active_ingestion_job_id": if completed { None } else { Some(job_id) }
        }
    }))
}

fn invalid_snapshot_page(batch: &IndexBatchRequest) -> PluginError {
    PluginError::new(
        "invalid_semantic_snapshot_page",
        format!(
            "invalid semantic snapshot metadata job={:?} page={:?} count={:?}; expected all three fields",
            batch.ingestion_job_id, batch.ingestion_page_index, batch.ingestion_page_count
        ),
        false,
    )
}

fn sync_graph(
    context: &mut PluginContext<'_>,
    batch: &IndexBatchRequest,
    elements: &BTreeMap<String, SemanticElement>,
    relationships: &BTreeMap<String, SemanticRelationship>,
) -> Result<(), PluginError> {
    let paths = partition_paths(batch);
    if paths.is_empty() {
        let elements = elements.values().collect::<Vec<_>>();
        let relationships = relationships.values().collect::<Vec<_>>();
        storage::sync_structure(context, &batch.project_root, &elements, &relationships)?;
        return Ok(());
    }
    storage::sync_partition(
        context,
        &batch.project_root,
        &paths,
        &elements.values().collect::<Vec<_>>(),
        &relationships.values().collect::<Vec<_>>(),
    )?;
    Ok(())
}

fn partition_paths(batch: &IndexBatchRequest) -> Vec<String> {
    batch
        .replace_paths
        .iter()
        .chain(batch.removed_paths.iter())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn store_graph_artifacts(
    context: &mut PluginContext<'_>,
    batch: &IndexBatchRequest,
    elements: &BTreeMap<String, SemanticElement>,
) -> Result<Vec<SemanticArtifact>, PluginError> {
    let mut stored = Vec::new();
    for item in &batch.semantic_artifacts {
        let Some(target) = active_element(context, &item.semantic_element_id, elements)? else {
            continue;
        };
        let artifact = artifact_from_input(item, &target.project_root);
        storage::upsert_graph_artifact(context, &artifact)?;
        stored.push(artifact);
    }
    Ok(stored)
}

fn remove_graph_artifacts(
    context: &mut PluginContext<'_>,
    batch: &IndexBatchRequest,
) -> Result<usize, PluginError> {
    let mut removed = 0;
    for artifact_id in &batch.removed_artifact_ids {
        removed += usize::from(storage::remove_graph_artifact(context, artifact_id)?);
    }
    Ok(removed)
}

fn artifact_from_input(
    item: &crate::models::SemanticArtifactInput,
    project_root: &str,
) -> SemanticArtifact {
    SemanticArtifact {
        project_root: project_root.into(),
        artifact_id: item.artifact_id.clone(),
        semantic_element_id: item.semantic_element_id.clone(),
        artifact_kind: item.artifact_kind.clone(),
        title: item.title.clone(),
        content_ref: item.content_ref.clone(),
        content: item.content.clone(),
        searchable_text: item.searchable_text.clone(),
        content_size_bytes: item.content_size_bytes,
        metadata: item.metadata.clone().unwrap_or_else(|| json!({})),
    }
}

pub(crate) fn record_log(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: IndexLogRequest = parse(input, "semantic index log")?;
    require_non_empty(&request.provider_instance_id, "provider_instance_id")?;
    require_non_empty(&request.index_log_id, "index_log_id")?;
    require_non_empty(&request.status, "status")?;
    let key = request.project_root.as_deref().map_or_else(
        || storage::global_key("l", &[&request.provider_instance_id, &request.index_log_id]),
        |root| {
            storage::project_key(
                root,
                "l",
                &[&request.provider_instance_id, &request.index_log_id],
            )
        },
    );
    let provider_key = storage::scoped_key(
        "v",
        &request.provider_instance_id,
        "l",
        &[&request.index_log_id],
    );
    let existing: Option<Value> = storage::get(context, INDEX_LOGS, &key)?;
    let now = Utc::now().to_rfc3339();
    let created_at = existing
        .as_ref()
        .and_then(|value| value["created_at"].as_str())
        .unwrap_or(&now)
        .to_owned();
    let record = json!({
        "provider_instance_id": request.provider_instance_id,
        "semantic_source_id": request.semantic_source_id,
        "plugin_id": request.plugin_id,
        "project_root": request.project_root,
        "index_log_id": request.index_log_id,
        "status": request.status,
        "metrics": request.metrics,
        "content_fingerprint": request.content_fingerprint,
        "error": request.error,
        "started_at": request.started_at,
        "completed_at": request.completed_at,
        "created_at": created_at,
        "updated_at": now
    });
    storage::put(context, INDEX_LOGS, &key, &record)?;
    storage::put(context, INDEX_LOGS_BY_PROVIDER, &provider_key, &record)?;
    Ok(json!({"index_log": record}))
}

pub(crate) fn latest_log(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: LatestIndexLogRequest = parse(input, "semantic index log query")?;
    let (table, prefix) = if let Some(provider) = request.provider_instance_id.as_deref() {
        (
            INDEX_LOGS_BY_PROVIDER,
            Some(storage::scoped_prefix("v", provider, "l")),
        )
    } else {
        (
            INDEX_LOGS,
            request
                .project_root
                .as_deref()
                .map(|root| storage::project_prefix(root, "l")),
        )
    };
    let latest = storage::list::<Value>(context, table, prefix.as_deref())?
        .into_iter()
        .filter(|record| {
            request
                .provider_instance_id
                .as_ref()
                .is_none_or(|id| record["provider_instance_id"].as_str() == Some(id))
        })
        .filter(|record| {
            request
                .project_root
                .as_ref()
                .is_none_or(|root| record["project_root"].as_str() == Some(root))
        })
        .max_by_key(|record| record["updated_at"].as_str().unwrap_or_default().to_owned());
    Ok(json!({"index_log": latest}))
}

fn validate_batch(batch: &IndexBatchRequest) -> Result<(), PluginError> {
    require_non_empty(&batch.provider_instance_id, "provider_instance_id")?;
    require_non_empty(&batch.project_root, "project_root")?;
    let snapshot_field_count = [
        batch.ingestion_job_id.is_some(),
        batch.ingestion_page_index.is_some(),
        batch.ingestion_page_count.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if snapshot_field_count != 0 && snapshot_field_count != 3 {
        return Err(invalid_snapshot_page(batch));
    }
    if let (Some(index), Some(count)) = (batch.ingestion_page_index, batch.ingestion_page_count)
        && (count == 0 || index >= count)
    {
        return Err(PluginError::new(
            "invalid_semantic_snapshot_page",
            format!(
                "invalid semantic snapshot page `{index}` of `{count}`; expected positive count and index below count"
            ),
            false,
        ));
    }
    for source in &batch.semantic_sources {
        require_non_empty(&source.semantic_source_id, "semantic_source_id")?;
    }
    let mut element_ids = BTreeSet::new();
    for element in &batch.semantic_elements {
        require_non_empty(&element.semantic_element_id, "semantic_element_id")?;
        if !element_ids.insert(element.semantic_element_id.as_str()) {
            return Err(PluginError::new(
                "invalid_semantic_index_batch",
                format!(
                    "duplicate semantic element `{}`; expected one incoming record per semantic element id",
                    element.semantic_element_id,
                ),
                false,
            ));
        }
        require_non_empty(&element.semantic_source_id, "semantic_source_id")?;
        require_non_empty(&element.path, "path")?;
    }
    let upserted = batch
        .semantic_artifacts
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect::<BTreeSet<_>>();
    for artifact_id in &batch.removed_artifact_ids {
        require_non_empty(artifact_id, "removed_artifact_ids entry")?;
        if upserted.contains(artifact_id.as_str()) {
            return Err(PluginError::new(
                "invalid_semantic_index_batch",
                format!(
                    "semantic artifact `{artifact_id}` is both removed and upserted; expected one mutation per artifact"
                ),
                false,
            ));
        }
    }
    Ok(())
}

fn incoming_relationships_unchecked(
    batch: &IndexBatchRequest,
    elements: &BTreeMap<String, SemanticElement>,
) -> BTreeMap<String, SemanticRelationship> {
    let mut relationships = batch
        .semantic_relationships
        .iter()
        .map(|item| {
            let relationship = relationship_from_input(batch, item);
            (relationship_key(&relationship), relationship)
        })
        .collect::<BTreeMap<_, _>>();
    for element in elements.values() {
        let Some(parent_id) = element.parent_element_id.as_deref() else {
            continue;
        };
        let relationship = parent_relationship(batch, parent_id, &element.semantic_element_id);
        relationships.insert(relationship_key(&relationship), relationship);
    }
    relationships
}

fn referenced_existing_element_ids(batch: &IndexBatchRequest) -> Vec<String> {
    let incoming = batch
        .semantic_elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<BTreeSet<_>>();
    batch
        .semantic_relationships
        .iter()
        .flat_map(|relationship| {
            [
                Some(relationship.source_element_id.as_str()),
                relationship.target_element_id.as_deref(),
            ]
            .into_iter()
            .flatten()
        })
        .chain(
            batch
                .semantic_elements
                .iter()
                .filter_map(|element| element.parent_element_id.as_deref()),
        )
        .filter(|id| !incoming.contains(id))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn incoming_elements(
    batch: &IndexBatchRequest,
    existing: &[SemanticElement],
) -> BTreeMap<String, SemanticElement> {
    let mut elements = batch
        .semantic_elements
        .iter()
        .map(|item| {
            let element = SemanticElement {
                project_root: batch.project_root.clone(),
                semantic_element_id: item.semantic_element_id.clone(),
                semantic_source_id: item.semantic_source_id.clone(),
                path: item.path.clone(),
                element_kind: item.semantic_element_type.clone(),
                name: item.semantic_element_name.clone(),
                parent_element_id: item.parent_element_id.clone(),
                content_fingerprint: item.content_fingerprint.clone(),
                start_line: item.start_line,
                end_line: item.end_line,
                lifecycle: "active".into(),
                metadata: json!({
                    "provider_instance_id": batch.provider_instance_id,
                    "indexer_metadata": item.metadata.clone().unwrap_or_else(|| json!({}))
                }),
            };
            (element.semantic_element_id.clone(), element)
        })
        .collect::<BTreeMap<_, _>>();
    for relationship in &batch.semantic_relationships {
        let Some(external) = external_element(batch, relationship, &elements, existing) else {
            continue;
        };
        elements
            .entry(external.semantic_element_id.clone())
            .or_insert(external);
    }
    apply_denormalized_parents(&mut elements, &batch.semantic_relationships);
    elements
}

fn external_element(
    batch: &IndexBatchRequest,
    relationship: &SemanticRelationshipInput,
    elements: &BTreeMap<String, SemanticElement>,
    existing: &[SemanticElement],
) -> Option<SemanticElement> {
    if relationship.target_element_id.is_some() {
        return None;
    }
    let source = elements.get(&relationship.source_element_id).or_else(|| {
        existing
            .iter()
            .find(|element| element.semantic_element_id == relationship.source_element_id)
    })?;
    let target = relationship
        .target_locator
        .as_deref()
        .or(relationship.target_label.as_deref())
        .unwrap_or("external");
    let id = format!("external:{}", stable_external_hash(target));
    Some(SemanticElement {
        project_root: batch.project_root.clone(),
        semantic_element_id: id,
        semantic_source_id: source.semantic_source_id.clone(),
        path: source.path.clone(),
        element_kind: "external".into(),
        name: relationship
            .target_label
            .clone()
            .or_else(|| relationship.target_locator.clone())
            .unwrap_or_else(|| "external".into()),
        parent_element_id: None,
        content_fingerprint: Some(format!(
            "fp1:{hash}:external:{hash}",
            hash = stable_external_hash(target),
        )),
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        metadata: json!({"external": true, "target_locator": relationship.target_locator}),
    })
}

fn apply_denormalized_parents(
    elements: &mut BTreeMap<String, SemanticElement>,
    relationships: &[SemanticRelationshipInput],
) {
    for relationship in relationships.iter().filter(|item| {
        item.relationship_kind == "contains" || item.relationship_label == "contains"
    }) {
        let Some(target) = relationship.target_element_id.as_ref() else {
            continue;
        };
        if let Some(element) = elements.get_mut(target) {
            element.parent_element_id = Some(relationship.source_element_id.clone());
        }
    }
}

fn incoming_relationships(
    batch: &IndexBatchRequest,
    elements: &BTreeMap<String, SemanticElement>,
    existing: &[SemanticElement],
) -> BTreeMap<String, SemanticRelationship> {
    let known_ids = elements
        .keys()
        .map(String::as_str)
        .chain(
            existing
                .iter()
                .filter(|element| element.lifecycle == "active")
                .map(|element| element.semantic_element_id.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let mut relationships = BTreeMap::new();
    for item in &batch.semantic_relationships {
        let relationship = relationship_from_input(batch, item);
        insert_known_relationship(&mut relationships, relationship, &known_ids);
    }
    for element in elements.values() {
        let Some(parent_id) = element.parent_element_id.as_deref() else {
            continue;
        };
        let relationship = parent_relationship(batch, parent_id, &element.semantic_element_id);
        insert_known_relationship(&mut relationships, relationship, &known_ids);
    }
    relationships
}

fn relationship_from_input(
    batch: &IndexBatchRequest,
    item: &SemanticRelationshipInput,
) -> SemanticRelationship {
    let target_id = item.target_element_id.clone().unwrap_or_else(|| {
        let target = item
            .target_locator
            .as_deref()
            .or(item.target_label.as_deref())
            .unwrap_or("external");
        format!("external:{}", stable_external_hash(target))
    });
    SemanticRelationship {
        project_root: batch.project_root.clone(),
        source_element_id: item.source_element_id.clone(),
        target_element_id: target_id,
        relationship_kind: item.relationship_kind.clone(),
        label: item.relationship_label.clone(),
        lifecycle: "active".into(),
        metadata: json!({
            "target_label": item.target_label, "target_locator": item.target_locator
        }),
    }
}

fn parent_relationship(
    batch: &IndexBatchRequest,
    parent_id: &str,
    child_id: &str,
) -> SemanticRelationship {
    SemanticRelationship {
        project_root: batch.project_root.clone(),
        source_element_id: parent_id.to_owned(),
        target_element_id: child_id.to_owned(),
        relationship_kind: "contains".into(),
        label: "contains".into(),
        lifecycle: "active".into(),
        metadata: json!({"derived_from": "parent_element_id"}),
    }
}

fn insert_known_relationship(
    relationships: &mut BTreeMap<String, SemanticRelationship>,
    relationship: SemanticRelationship,
    known_ids: &BTreeSet<&str>,
) {
    if !known_ids.contains(relationship.source_element_id.as_str())
        || !known_ids.contains(relationship.target_element_id.as_str())
    {
        return;
    }
    relationships.insert(relationship_key(&relationship), relationship);
}

fn store_sources(
    mutations: &mut Vec<storage::Mutation>,
    batch: &IndexBatchRequest,
) -> Result<(), PluginError> {
    for item in &batch.semantic_sources {
        let source = SemanticSource {
            semantic_source_id: item.semantic_source_id.clone(),
            kind: item.kind.clone(),
            name: item.name.clone(),
            root_path: item.root_path.clone(),
            root_uri: item.root_uri.clone(),
            project_root: batch.project_root.clone(),
        };
        let key = storage::project_key(&source.project_root, "s", &[&source.semantic_source_id]);
        mutations.push(storage::put_mutation(SOURCES, &key, &source)?);
    }
    Ok(())
}

fn active_element(
    context: &mut PluginContext<'_>,
    element_id: &str,
    incoming: &BTreeMap<String, SemanticElement>,
) -> Result<Option<SemanticElement>, PluginError> {
    if let Some(element) = incoming
        .get(element_id)
        .filter(|element| element.lifecycle == "active")
    {
        return Ok(Some(element.clone()));
    }
    let element = storage::graph_element(context, element_id)?;
    Ok(element.filter(|element: &SemanticElement| element.lifecycle == "active"))
}

fn relationship_key(relationship: &SemanticRelationship) -> String {
    storage::project_key(
        &relationship.project_root,
        "r",
        &[
            &relationship.source_element_id,
            &relationship.target_element_id,
            &relationship.relationship_kind,
            &relationship.label,
        ],
    )
}

fn stable_external_hash(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}
