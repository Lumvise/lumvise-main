//! Lazy vector-index maintenance driven by Semantic's own storage-change events.
//!
//! Registers as a `StorageTrigger` on Semantic element and artifact upserts.
//! Each delivery deduplicates non-removal ids, performs one batch read per
//! entity category, embeds in existing chunks of at most 128 texts, and stores
//! replacement vectors once per category. Deletion needs no handling: the
//! storage layer cascades vector-node removal when an entity is removed.

use lumvise_plugin_sdk::{
    PluginContext, PluginError, StorageTriggerDisposition, StorageTriggerRequest,
};
use serde_json::{Value, json};

use crate::models::{SemanticArtifact, SemanticElement};
use crate::query::embed_texts;
use crate::storage;

pub(crate) fn storage_change(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request = StorageTriggerRequest::from_value(input).map_err(|error| {
        PluginError::new(
            "invalid_storage_change_batch",
            format!("invalid v-next StorageTrigger batch: {error}"),
            false,
        )
    })?;
    let element_ids = request
        .changed
        .iter()
        .filter(|change| {
            change.disposition != StorageTriggerDisposition::Removal
                && change.entity_kind == "semantic_element"
        })
        .map(|change| change.entity_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let artifact_ids = request
        .changed
        .iter()
        .filter(|change| {
            change.disposition != StorageTriggerDisposition::Removal
                && change.entity_kind == "semantic_artifact"
        })
        .map(|change| change.entity_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let elements = storage::graph_elements_by_ids::<SemanticElement>(
        context,
        &request.project_root,
        &element_ids.iter().cloned().collect::<Vec<_>>(),
    )?;
    let artifacts = storage::graph_artifacts_by_ids::<SemanticArtifact>(
        context,
        &artifact_ids.iter().cloned().collect::<Vec<_>>(),
    )?;
    let mut element_records = Vec::new();
    for element in elements
        .into_iter()
        .filter(|element| element.lifecycle == "active" && !element.name.trim().is_empty())
    {
        element_records.push((
            element.semantic_element_id.clone(),
            element.name.trim().to_owned(),
        ));
    }
    let mut artifact_records = artifacts
        .into_iter()
        .filter_map(|artifact| {
            artifact
                .searchable_text
                .or(artifact.content)
                .map(|text| (artifact.artifact_id, artifact.semantic_element_id, text))
                .filter(|(_, _, text)| !text.trim().is_empty())
        })
        .collect::<Vec<_>>();
    artifact_records.sort_by(|left, right| left.0.cmp(&right.0));
    let element_count = index_elements(context, &request.project_root, element_records)?;
    let artifact_count = index_artifacts(context, &request.project_root, artifact_records)?;
    Ok(json!({
        "acknowledged": true,
        "processed": element_count + artifact_count,
    }))
}

fn index_elements(
    context: &mut PluginContext<'_>,
    project_root: &str,
    records: Vec<(String, String)>,
) -> Result<usize, PluginError> {
    if records.is_empty() {
        return Ok(0);
    }
    let texts = records
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>();
    let embedded = embed_texts(context, &texts).ok_or_else(embedding_failed)?;
    let crate::query::EmbedBatch {
        vectors,
        engine_id,
        model,
    } = embedded;
    let records = records
        .into_iter()
        .zip(vectors)
        .map(|((id, text), vector)| {
            json!({"semantic_element_id": id, "project_root": project_root,
            "source_text": text, "vector": vector_record(&engine_id, &model, vector)})
        })
        .collect();
    storage::store_element_name_vectors(context, project_root, records)
}

fn index_artifacts(
    context: &mut PluginContext<'_>,
    project_root: &str,
    records: Vec<(String, String, String)>,
) -> Result<usize, PluginError> {
    if records.is_empty() {
        return Ok(0);
    }
    let texts = records
        .iter()
        .map(|(_, _, text)| text.trim())
        .collect::<Vec<_>>();
    let embedded = embed_texts(context, &texts).ok_or_else(embedding_failed)?;
    let crate::query::EmbedBatch {
        vectors,
        engine_id,
        model,
    } = embedded;
    let records = records
        .into_iter()
        .zip(vectors)
        .map(|((id, semantic_element_id, text), vector)| {
            json!({"artifact_id": id, "semantic_element_id": semantic_element_id,
                "source_text": text.trim(),
                "vector": vector_record(&engine_id, &model, vector)})
        })
        .collect();
    storage::store_artifact_text_vectors(context, project_root, records)
}

fn vector_record(engine_id: &str, model: &Option<String>, vector: Vec<f32>) -> Value {
    json!({"engine_id": engine_id, "model": model,
        "dimensions": vector.len(), "vector": vector, "normalized": false, "metadata": {}})
}

fn embedding_failed() -> PluginError {
    PluginError::new("semantic_embedding_failed", "embedding batch failed", true)
}
