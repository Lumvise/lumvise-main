use std::{cmp::Ordering, collections::BTreeMap, sync::Mutex};

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{KnowledgeArtifact, artifact, artifact::SearchRequest, storage};

const EMBED_BATCH_SIZE: usize = 64;
// Match neural.embed's input contract without dropping long artifact content.
const EMBED_TEXT_BYTES: usize = 32 * 1024;
const EMBED_REQUEST_BYTES: usize = 256 * 1024;

#[derive(Clone, Deserialize, Serialize)]
struct KnowledgeTextVector {
    record_id: String,
    content_fingerprint: String,
    vector: Vec<f32>,
}

/// Owns derived vectors for this plugin process. Artifact reads still authorize every search.
#[derive(Default)]
pub(crate) struct KnowledgeSearchVectors {
    records: Mutex<BTreeMap<String, Option<KnowledgeTextVector>>>,
}

impl KnowledgeSearchVectors {
    pub(crate) fn rank_texts(
        &self,
        query: &str,
        texts: &[(String, String)],
        context: &mut PluginContext<'_>,
    ) -> Option<BTreeMap<String, f32>> {
        let query = embed(context, &[query])?.pop()?;
        let mut scores = BTreeMap::new();
        let missing = texts
            .iter()
            .filter(|(id, text)| !self.append_cached_score(id, text, &query, context, &mut scores))
            .collect::<Vec<_>>();
        for batch in missing.chunks(EMBED_BATCH_SIZE) {
            self.rank_missing(batch, &query, context, &mut scores)?;
        }
        Some(scores)
    }

    fn append_cached_score(
        &self,
        id: &str,
        text: &str,
        query: &[f32],
        context: &mut PluginContext<'_>,
        scores: &mut BTreeMap<String, f32>,
    ) -> bool {
        let Some(record) = self
            .find(id, context)
            .filter(|record| record.content_fingerprint == text_fingerprint(text))
        else {
            return false;
        };
        let Some(score) = cosine(query, &record.vector) else {
            return false;
        };
        scores.insert(id.to_owned(), score);
        true
    }

    fn rank_missing(
        &self,
        texts: &[&(String, String)],
        query: &[f32],
        context: &mut PluginContext<'_>,
        scores: &mut BTreeMap<String, f32>,
    ) -> Option<()> {
        let inputs = texts
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>();
        let embeddings = embed(context, &inputs)?;
        let mut rows = Vec::new();
        for ((id, text), vector) in texts.iter().zip(embeddings) {
            scores.insert(id.clone(), cosine(query, &vector)?);
            rows.push((
                id.clone(),
                KnowledgeTextVector {
                    record_id: id.clone(),
                    content_fingerprint: text_fingerprint(text),
                    vector,
                },
            ));
        }
        let _ = self.store_rows(context, rows);
        Some(())
    }

    fn store_rows(
        &self,
        context: &mut PluginContext<'_>,
        rows: Vec<(String, KnowledgeTextVector)>,
    ) -> Result<(), PluginError> {
        storage::put_rows(context, storage::VECTORS, &rows)?;
        self.remember(rows);
        Ok(())
    }

    fn find(
        &self,
        artifact_id: &str,
        context: &mut PluginContext<'_>,
    ) -> Option<KnowledgeTextVector> {
        if let Some(cached) = self.records.lock().ok()?.get(artifact_id).cloned() {
            return cached;
        }
        let stored = storage::get(context, storage::VECTORS, artifact_id).ok()?;
        self.records
            .lock()
            .ok()?
            .entry(artifact_id.to_owned())
            .or_insert(stored)
            .clone()
    }

    fn remember(&self, rows: Vec<(String, KnowledgeTextVector)>) {
        if let Ok(mut records) = self.records.lock() {
            records.extend(rows.into_iter().map(|(id, vector)| (id, Some(vector))));
        }
    }
}

pub(crate) fn search(
    request: &SearchRequest,
    context: &mut PluginContext<'_>,
    vectors: &KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    crate::artifact::require(&request.query, "query")?;
    let artifacts = artifacts_for_search(request, context)?
        .into_iter()
        .filter(|item| artifact::filter_matches(item, request))
        .collect::<Vec<_>>();
    let (mode, mut results) = vector_results(request, &artifacts, context, vectors)
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
    if let Some(element_id) = request.semantic_element_id.as_deref() {
        return storage::artifacts_for_elements(
            context,
            &std::collections::HashSet::from([element_id.to_owned()]),
            request.project_root.as_deref(),
        );
    }
    if let Some(project_root) = request.project_root.as_deref() {
        return storage::list_project_knowledge(context, project_root);
    }
    storage::list_all_knowledge(context)
}

fn vector_results(
    request: &SearchRequest,
    artifacts: &[KnowledgeArtifact],
    context: &mut PluginContext<'_>,
    vectors: &KnowledgeSearchVectors,
) -> Option<(&'static str, Vec<Value>)> {
    let texts = artifacts
        .iter()
        .map(|artifact| (artifact.artifact_id.clone(), searchable_text(artifact)))
        .collect::<Vec<_>>();
    let scores = vectors.rank_texts(&request.query, &texts, context)?;
    let results = artifacts
        .iter()
        .filter_map(|artifact| {
            scores
                .get(&artifact.artifact_id)
                .map(|score| result(artifact, *score))
        })
        .collect();
    Some(("vector", results))
}

pub(crate) fn rebuild(
    project_root: Option<&str>,
    context: &mut PluginContext<'_>,
    vectors: &KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    let artifacts = rebuild_artifacts(project_root, context)?;
    Ok(json!({"job": rebuild_batches(&artifacts, context, vectors)?}))
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
    vectors: &KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    let mut rebuilt = 0;
    for batch in artifacts.chunks(EMBED_BATCH_SIZE) {
        match persist_batch(batch, context, vectors) {
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

fn persist_batch(
    artifacts: &[KnowledgeArtifact],
    context: &mut PluginContext<'_>,
    cache: &KnowledgeSearchVectors,
) -> Result<(), PluginError> {
    let texts = artifacts.iter().map(searchable_text).collect::<Vec<_>>();
    let vectors = required_embeddings(
        context,
        &texts.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    persist_vectors(
        &artifacts.iter().collect::<Vec<_>>(),
        vectors,
        context,
        cache,
    )
}

fn persist_vectors(
    artifacts: &[&KnowledgeArtifact],
    vectors: Vec<Vec<f32>>,
    context: &mut PluginContext<'_>,
    cache: &KnowledgeSearchVectors,
) -> Result<(), PluginError> {
    let rows = artifacts
        .iter()
        .zip(vectors)
        .map(|(artifact, vector)| {
            let record = KnowledgeTextVector {
                record_id: artifact.artifact_id.clone(),
                content_fingerprint: content_fingerprint(artifact),
                vector,
            };
            (artifact.artifact_id.clone(), record)
        })
        .collect::<Vec<_>>();
    // Match embedding batches to existing atomic storage batches, avoiding
    // two process round trips for each individual cached vector.
    cache.store_rows(context, rows)
}

fn text_fingerprint(text: &str) -> String {
    format!("{:x}", md5::compute(text.as_bytes()))
}

fn content_fingerprint(artifact: &KnowledgeArtifact) -> String {
    text_fingerprint(&searchable_text(artifact))
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
    let chunks = texts
        .iter()
        .map(|text| embedding_chunks(text))
        .collect::<Vec<_>>();
    let inputs = chunks.iter().flatten().copied().collect::<Vec<_>>();
    let vectors = embed_bounded_chunks(context, &inputs)?;
    let dimensions = vectors.first().map_or(0, Vec::len);
    if vectors.iter().any(|vector| vector.len() != dimensions) {
        return Err(invalid_embeddings(
            &json!({"vectors": vectors}),
            inputs.len(),
        ));
    }
    Ok(merge_chunk_embeddings(&chunks, &vectors))
}

fn embedding_chunks(mut text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    while text.len() > EMBED_TEXT_BYTES {
        let mut end = EMBED_TEXT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[..end]);
        text = &text[end..];
    }
    chunks.push(text);
    chunks
}

fn embed_bounded_chunks(
    context: &mut PluginContext<'_>,
    mut chunks: &[&str],
) -> Result<Vec<Vec<f32>>, PluginError> {
    let mut vectors = Vec::with_capacity(chunks.len());
    while !chunks.is_empty() {
        let count = embedding_batch_count(chunks);
        vectors.extend(embed_request(context, &chunks[..count])?);
        chunks = &chunks[count..];
    }
    Ok(vectors)
}

fn embedding_batch_count(chunks: &[&str]) -> usize {
    let mut bytes = 0;
    chunks
        .iter()
        .take(EMBED_BATCH_SIZE)
        .take_while(|text| {
            bytes += text.len();
            bytes <= EMBED_REQUEST_BYTES
        })
        .count()
}

fn merge_chunk_embeddings(chunks: &[Vec<&str>], vectors: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let mut offset = 0;
    chunks
        .iter()
        .map(|parts| {
            let end = offset + parts.len();
            let merged = average_chunk_embedding(parts, &vectors[offset..end]);
            offset = end;
            merged
        })
        .collect()
}

fn average_chunk_embedding(chunks: &[&str], vectors: &[Vec<f32>]) -> Vec<f32> {
    if vectors.len() == 1 {
        return vectors[0].clone();
    }
    let total_bytes = chunks.iter().map(|text| text.len()).sum::<usize>() as f64;
    let mut average = vec![0.0_f64; vectors[0].len()];
    for (text, vector) in chunks.iter().zip(vectors) {
        let weight = text.len() as f64 / total_bytes;
        for (sum, value) in average.iter_mut().zip(vector) {
            *sum += f64::from(*value) * weight;
        }
    }
    average.into_iter().map(|value| value as f32).collect()
}

fn embed_request(
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
        "status": artifact.lifecycle.status,
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
