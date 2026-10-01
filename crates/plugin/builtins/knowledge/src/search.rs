use std::{cmp::Ordering, collections::BTreeMap};

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{KnowledgeArtifact, artifact, artifact::SearchRequest, storage};

const EMBED_BATCH_SIZE: usize = 64;

#[derive(Clone, Deserialize, Serialize)]
struct KnowledgeVector {
    artifact_id: String,
    content_fingerprint: String,
    vector: Vec<f32>,
}

pub(crate) fn search(
    request: &SearchRequest,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    crate::artifact::require(&request.query, "query")?;
    let artifacts = artifacts_for_search(request, context)?
        .into_iter()
        .filter(|item| artifact::filter_matches(item, request))
        .collect::<Vec<_>>();
    let (mode, mut results) = vector_results(request, &artifacts, context)
        .unwrap_or_else(|| ("lexical", lexical_results(request, &artifacts)));
    // Literal title/tag/content matches must remain discoverable even when
    // an embedding model ranks unrelated records above them.
    let literal_scores = artifacts
        .iter()
        .map(|item| {
            (
                item.artifact_id.as_str(),
                lexical_score(item, &request.query).unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    results.sort_by(|left, right| {
        let score = |item: &Value| {
            literal_scores
                .get(item["artifact_id"].as_str().unwrap_or_default())
                .copied()
                .unwrap_or_default()
        };
        score(right)
            .total_cmp(&score(left))
            .then_with(|| result_order(left, right))
    });
    results.truncate(request.limit.unwrap_or(20));
    Ok(json!({"mode": mode, "results": results}))
}

fn artifacts_for_search(
    request: &SearchRequest,
    context: &mut PluginContext<'_>,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    if let Some(project_root) = request.project_root.as_deref() {
        return storage::list_project_knowledge(context, project_root);
    }
    if let Some(element_id) = request.semantic_element_id.as_deref() {
        return storage::artifacts_for_elements(
            context,
            &std::collections::HashSet::from([element_id.to_owned()]),
        );
    }
    storage::list_all_knowledge(context)
}

fn vector_results(
    request: &SearchRequest,
    artifacts: &[KnowledgeArtifact],
    context: &mut PluginContext<'_>,
) -> Option<(&'static str, Vec<Value>)> {
    let query = embed(context, &[request.query.as_str()])?.pop()?;
    let stored = stored_vectors(context);
    let mut results = Vec::with_capacity(artifacts.len());
    let missing = artifacts
        .iter()
        .filter(|artifact| !append_stored_result(artifact, &stored, &query, &mut results))
        .collect::<Vec<_>>();
    for batch in missing.chunks(EMBED_BATCH_SIZE) {
        append_embedded_results(batch, &query, &mut results, context)?;
    }
    Some(("vector", results))
}

pub(crate) fn rebuild(
    project_root: Option<&str>,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let artifacts = rebuild_artifacts(project_root, context)?;
    Ok(json!({"job": rebuild_batches(&artifacts, context)?}))
}

fn rebuild_artifacts(
    project_root: Option<&str>,
    context: &mut PluginContext<'_>,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    match project_root {
        Some(root) => storage::list_project_knowledge(context, root),
        None => storage::list_all_knowledge(context),
    }
}

fn rebuild_batches(
    artifacts: &[KnowledgeArtifact],
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let mut rebuilt = 0;
    for batch in artifacts.chunks(EMBED_BATCH_SIZE) {
        match persist_batch(batch, context) {
            Ok(()) => rebuilt += batch.len(),
            Err(error) if error.code == "host_capability_unavailable" => {
                return Ok(unavailable_rebuild(rebuilt, error));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(json!({"status": "completed", "vectors_rebuilt": rebuilt}))
}

fn unavailable_rebuild(vectors_rebuilt: usize, error: PluginError) -> Value {
    json!({
        "status": "unavailable",
        "vectors_rebuilt": vectors_rebuilt,
        "reason": error.message
    })
}

fn stored_vectors(context: &mut PluginContext<'_>) -> BTreeMap<String, KnowledgeVector> {
    storage::list::<KnowledgeVector>(context, storage::VECTORS, None)
        .unwrap_or_default()
        .into_iter()
        .map(|record| (record.artifact_id.clone(), record))
        .collect()
}

fn append_stored_result(
    artifact: &KnowledgeArtifact,
    stored: &BTreeMap<String, KnowledgeVector>,
    query: &[f32],
    results: &mut Vec<Value>,
) -> bool {
    let Some(record) = stored.get(&artifact.artifact_id) else {
        return false;
    };
    if record.content_fingerprint != content_fingerprint(artifact) {
        return false;
    }
    let Some(score) = cosine(query, &record.vector) else {
        return false;
    };
    results.push(result(artifact, score));
    true
}

fn append_embedded_results(
    artifacts: &[&KnowledgeArtifact],
    query: &[f32],
    results: &mut Vec<Value>,
    context: &mut PluginContext<'_>,
) -> Option<()> {
    let texts = artifacts
        .iter()
        .map(|item| searchable_text(item))
        .collect::<Vec<_>>();
    let vectors = embed(
        context,
        &texts.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    for (artifact, vector) in artifacts.iter().zip(&vectors) {
        results.push(result(artifact, cosine(query, vector)?));
    }
    let _ = persist_vectors(artifacts, vectors, context);
    Some(())
}

fn persist_batch(
    artifacts: &[KnowledgeArtifact],
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let texts = artifacts.iter().map(searchable_text).collect::<Vec<_>>();
    let vectors = required_embeddings(
        context,
        &texts.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    persist_vectors(&artifacts.iter().collect::<Vec<_>>(), vectors, context)
}

fn persist_vectors(
    artifacts: &[&KnowledgeArtifact],
    vectors: Vec<Vec<f32>>,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let rows = artifacts
        .iter()
        .zip(vectors)
        .map(|(artifact, vector)| {
            let record = KnowledgeVector {
                artifact_id: artifact.artifact_id.clone(),
                content_fingerprint: content_fingerprint(artifact),
                vector,
            };
            (artifact.artifact_id.clone(), record)
        })
        .collect::<Vec<_>>();
    // Match embedding batches to existing atomic storage batches, avoiding
    // two process round trips for each individual cached vector.
    storage::put_rows(context, storage::VECTORS, &rows)
}

fn content_fingerprint(artifact: &KnowledgeArtifact) -> String {
    format!("{:x}", md5::compute(searchable_text(artifact).as_bytes()))
}

fn lexical_results(request: &SearchRequest, artifacts: &[KnowledgeArtifact]) -> Vec<Value> {
    artifacts
        .iter()
        .filter_map(|item| lexical_score(item, &request.query).map(|score| result(item, score)))
        .collect()
}

fn embed(context: &mut PluginContext<'_>, texts: &[&str]) -> Option<Vec<Vec<f32>>> {
    required_embeddings(context, texts).ok()
}

fn required_embeddings(
    context: &mut PluginContext<'_>,
    texts: &[&str],
) -> Result<Vec<Vec<f32>>, PluginError> {
    let output = context.host_call("neural.embed", json!({"texts": texts}))?;
    let vectors = output["vectors"]
        .as_array()
        .ok_or_else(|| invalid_embeddings(&output, texts.len()))?
        .iter()
        .map(decode_vector)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| invalid_embeddings(&output, texts.len()))?;
    let dimensions = vectors.first().map_or(0, Vec::len);
    if vectors.len() == texts.len()
        && dimensions > 0
        && vectors.iter().all(|vector| vector.len() == dimensions)
    {
        return Ok(vectors);
    }
    Err(invalid_embeddings(&output, texts.len()))
}

fn invalid_embeddings(output: &Value, expected_count: usize) -> PluginError {
    PluginError::new(
        "invalid_embedding_response",
        format!(
            "invalid neural.embed response `{output}`; expected {expected_count} equal-dimension vectors"
        ),
        false,
    )
}

fn decode_vector(value: &Value) -> Option<Vec<f32>> {
    value
        .as_array()?
        .iter()
        .map(|number| {
            let value = number.as_f64()? as f32;
            value.is_finite().then_some(value)
        })
        .collect()
}

fn cosine(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    Some(if left_norm <= f32::EPSILON || right_norm <= f32::EPSILON {
        0.0
    } else {
        dot / (left_norm * right_norm)
    })
}

fn lexical_score(artifact: &KnowledgeArtifact, query: &str) -> Option<f32> {
    let query = query.trim().to_ascii_lowercase();
    let title = artifact.title.to_ascii_lowercase();
    let content = artifact.content.to_ascii_lowercase();
    if title == query {
        return Some(1.0);
    }
    if title.contains(&query) {
        return Some(0.8);
    }
    if content.contains(&query)
        || artifact
            .tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase().contains(&query))
    {
        return Some(0.5);
    }
    None
}

fn searchable_text(artifact: &KnowledgeArtifact) -> String {
    format!(
        "{}\n{}\n{}",
        artifact.title,
        artifact.tags.join(" "),
        artifact.content
    )
}

fn result(artifact: &KnowledgeArtifact, score: f32) -> Value {
    json!({
        "artifact_id": artifact.artifact_id,
        "semantic_element_id": artifact.semantic_element_id,
        "knowledge_type": artifact.knowledge_type,
        "title": artifact.title,
        "score": score
    })
}

fn result_order(left: &Value, right: &Value) -> Ordering {
    let left_score = left["score"].as_f64().unwrap_or_default();
    let right_score = right["score"].as_f64().unwrap_or_default();
    right_score.total_cmp(&left_score).then_with(|| {
        left["artifact_id"]
            .as_str()
            .cmp(&right["artifact_id"].as_str())
    })
}
