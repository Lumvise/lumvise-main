mod dependency;
mod rebuild;
pub(crate) use dependency::dependency_tree;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

use crate::{
    models::{
        DEFAULT_TREE_DEPTH, ElementAtLocationRequest, SemanticElement, SemanticRelationship,
        SemanticTreeRequest,
    },
    parse, require_non_empty, serialize_response,
    storage::{self, SEARCH_INDEX_STATE},
};
use lumvise_contracts::{SearchContextRequestV2, SearchContextResponseV2, SemanticSearchResultV2};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_STRUCTURAL_DEPTH: usize = 128;

pub(crate) fn semantic_tree(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: SemanticTreeRequest = parse(input, "semantic tree input")?;
    let (scope, root_id) = match (
        request
            .project_root
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
        request
            .semantic_element_id
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
    ) {
        (_, Some(id)) => (json!({"semantic_element": id}), Some(id.to_owned())),
        (Some(root), None) => (json!({"project_root": root}), None),
        _ => return Err(invalid_input("", "project_root or semantic_element_id")),
    };
    let max_depth = structural_depth(request.max_depth, DEFAULT_TREE_DEPTH)?;
    let snapshot = storage::structure_snapshot::<SemanticElement, SemanticRelationship>(
        context,
        &scope,
        max_depth,
        request.include_inactive.unwrap_or(false),
    )?;
    let mut elements = snapshot.elements;
    elements.retain(|element| request.include_inactive.unwrap_or(false) || is_active(element));
    sort_elements(&mut elements);
    let relationships = active_relationships(snapshot.relationships);
    let children = children_by_parent(&relationships);
    let parents = parent_by_child(&relationships);
    let by_id = elements
        .iter()
        .map(|element| (element.semantic_element_id.as_str(), element))
        .collect::<HashMap<_, _>>();
    let roots = root_ids(&elements, &parents, root_id.as_deref());
    let nodes = roots
        .iter()
        .filter_map(|id| {
            tree_node(
                id,
                0,
                max_depth,
                &by_id,
                &children,
                &parents,
                &mut HashSet::new(),
            )
        })
        .collect::<Vec<_>>();
    let visible_ids = nodes.iter().flat_map(tree_node_ids).collect::<HashSet<_>>();
    Ok(json!({
        "project_root": snapshot.project_root,
        "semantic_element_id": request.semantic_element_id,
        "total_nodes": visible_ids.len(),
        "max_depth": max_depth,
        "roots": nodes,
        "commit_version": snapshot.commit_version,
        "published_at": snapshot.published_at
    }))
}

fn structural_depth(value: Option<usize>, default: usize) -> Result<usize, PluginError> {
    let depth = value.unwrap_or(default);
    if depth <= MAX_STRUCTURAL_DEPTH {
        return Ok(depth);
    }
    Err(invalid_input(
        &depth.to_string(),
        "structural max_depth from 0 through 128",
    ))
}

pub(crate) fn element_at_location(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: ElementAtLocationRequest = parse(input, "semantic element location input")?;
    require_non_empty(&request.project_root, "project_root")?;
    require_non_empty(&request.path, "path")?;
    if request.line < 1 {
        return Err(invalid_input(&request.line.to_string(), "line >= 1"));
    }
    let path = normalize_location_path(&request.project_root, &request.path);
    let snapshot = storage::location_snapshot::<SemanticElement, SemanticRelationship>(
        context,
        &request.project_root,
        path.as_deref(),
        request.line,
        request.include_inactive.unwrap_or(false),
    )?;
    let mut elements = snapshot.elements;
    elements.retain(|element| {
        request
            .element_kind
            .as_ref()
            .is_none_or(|kind| &element.element_kind == kind)
    });
    let relationships = active_relationships(snapshot.relationships);
    let depths = parent_depths(&elements, &relationships);
    let mut matches = elements.iter().collect::<Vec<_>>();
    matches.sort_by_key(|element| std::cmp::Reverse(location_rank(element, &depths)));
    let matched = matches.first().copied();
    Ok(json!({
        "project_root": request.project_root,
        "path": path.unwrap_or_else(|| normalize_path(&request.path)),
        "line": request.line,
        "semantic_element_id": matched.map(|element| &element.semantic_element_id),
        "element": matched.map(|element| element.view(element.parent_element_id.clone())),
        "candidates": matches.into_iter().map(|element| element.view(element.parent_element_id.clone())).collect::<Vec<_>>(),
        "commit_version": snapshot.commit_version,
        "published_at": snapshot.published_at
    }))
}

pub(crate) fn search(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: SearchContextRequestV2 = parse(input, "semantic search input")?;
    require_non_empty(&request.query, "query")?;
    let limit = search_limit(request.limit)?;
    let candidate_limit = limit.saturating_mul(8).max(64);
    let mut elements: Vec<SemanticElement> = storage::graph_search_element_candidates(
        context,
        request.project_root.as_deref(),
        &request.query,
        candidate_limit,
    )?;
    elements.retain(|element| keeps(&request, element));
    let vector_search = declared_index_state(context, request.project_root.as_deref())
        .filter(|state| state == "rebuilding" || state == "failed")
        .map_or_else(
            || vector_results(context, &request, &elements, candidate_limit),
            |state| VectorSearch {
                index_state: state,
                results: None,
            },
        );
    let (mode, mut results) = match vector_search.results {
        Some(results) => ("vector", results),
        None => (
            "lexical_fallback",
            lexical_results(&request.query, &elements),
        ),
    };
    results.sort_by(|left, right| {
        right.score.total_cmp(&left.score).then(
            left.element
                .semantic_element_id
                .cmp(&right.element.semantic_element_id),
        )
    });
    results.truncate(limit);
    serialize_response(SearchContextResponseV2 {
        query: request.query.trim().into(),
        project_root: request.project_root,
        mode: mode.into(),
        index_state: vector_search.index_state,
        candidate_count: elements.len(),
        results,
    })
}

#[derive(Debug, Deserialize, Serialize)]
struct SearchIndexState {
    project_root: String,
    state: String,
    indexed_nodes: usize,
    error: Option<String>,
}

const MAX_NEURAL_EMBED_TEXTS: usize = 128;

pub(crate) fn rebuild_search_index(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    rebuild::invoke(input, context)
}

fn search_limit(limit: Option<usize>) -> Result<usize, PluginError> {
    let limit = limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(invalid_input(
            &limit.to_string(),
            "search limit from 1 through 100",
        ));
    }
    Ok(limit)
}

/// Whether a stored element belongs in this search's result set.
fn keeps(request: &SearchContextRequestV2, element: &SemanticElement) -> bool {
    request
        .project_root
        .as_ref()
        .is_none_or(|root| &element.project_root == root)
        && (request.include_inactive.unwrap_or(false) || is_active(element))
        && request
            .element_kind
            .as_ref()
            .is_none_or(|kind| &element.element_kind == kind)
}

struct VectorSearch {
    index_state: String,
    results: Option<Vec<SemanticSearchResultV2>>,
}

/// Scores elements by their better vector hit: the name vector or the section text vector.
/// Text hits may name elements outside the lexical candidates, so a body-only query still
/// finds its section; those elements are resolved by id and kept only when `keeps` accepts them.
fn vector_results(
    context: &mut PluginContext<'_>,
    request: &SearchContextRequestV2,
    candidates: &[SemanticElement],
    text_limit: usize,
) -> VectorSearch {
    let candidate_roots = candidates
        .iter()
        .map(|element| element.project_root.as_str())
        .collect::<BTreeSet<_>>();
    let text_roots = candidate_roots
        .iter()
        .copied()
        .chain(request.project_root.as_deref())
        .collect::<BTreeSet<_>>();
    if text_roots.is_empty() {
        return VectorSearch {
            index_state: "ready".into(),
            results: Some(Vec::new()),
        };
    }
    let Some(embedded) = embed_texts(context, &[request.query.as_str()], EmbedTask::Query).ok()
    else {
        return failed_vector_search();
    };
    let Some(query_vector) = embedded.vectors.first().cloned() else {
        return failed_vector_search();
    };
    let elements_by_id = candidates
        .iter()
        .map(|element| (element.semantic_element_id.as_str(), element))
        .collect::<HashMap<_, _>>();
    let mut best: HashMap<String, (f32, SemanticElement)> = HashMap::new();
    for root in &candidate_roots {
        let Some(hits) = vector_hits(
            context,
            "search_element_name_vectors",
            root,
            &query_vector,
            candidates.len(),
            &embedded,
        ) else {
            return failed_vector_search();
        };
        for (id, score) in hits {
            if let Some(element) = elements_by_id.get(id.as_str()) {
                keep_best(&mut best, score, element);
            }
        }
    }
    let mut unlisted: BTreeMap<&str, Vec<(String, f32)>> = BTreeMap::new();
    for root in &text_roots {
        let Some(hits) = vector_hits(
            context,
            "search_element_text_vectors",
            root,
            &query_vector,
            text_limit,
            &embedded,
        ) else {
            return failed_vector_search();
        };
        for (id, score) in hits {
            match elements_by_id.get(id.as_str()) {
                Some(element) => keep_best(&mut best, score, element),
                None => unlisted.entry(root).or_default().push((id, score)),
            }
        }
    }
    for (root, hits) in unlisted {
        let ids = hits.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        let Ok(resolved) = storage::graph_elements_by_ids::<SemanticElement>(context, root, &ids)
        else {
            return failed_vector_search();
        };
        let scores = hits
            .iter()
            .map(|(id, score)| (id.as_str(), *score))
            .collect::<HashMap<_, _>>();
        for element in resolved.iter().filter(|element| keeps(request, element)) {
            if let Some(&score) = scores.get(element.semantic_element_id.as_str()) {
                keep_best(&mut best, score, element);
            }
        }
    }
    let results = best
        .into_values()
        .map(|(score, element)| SemanticSearchResultV2 {
            element: element.view(element.parent_element_id.clone()),
            score,
        })
        .collect::<Vec<_>>();
    if results.is_empty() {
        return VectorSearch {
            index_state: "missing".into(),
            results: None,
        };
    }
    VectorSearch {
        index_state: "ready".into(),
        results: Some(results),
    }
}

fn keep_best(
    best: &mut HashMap<String, (f32, SemanticElement)>,
    score: f32,
    element: &SemanticElement,
) {
    match best.get_mut(&element.semantic_element_id) {
        Some(entry) if score > entry.0 => *entry = (score, element.clone()),
        Some(_) => {}
        None => {
            best.insert(
                element.semantic_element_id.clone(),
                (score, element.clone()),
            );
        }
    }
}

fn vector_hits(
    context: &mut PluginContext<'_>,
    operation: &str,
    project_root: &str,
    query: &[f32],
    k: usize,
    embedded: &EmbedBatch,
) -> Option<Vec<(String, f32)>> {
    let output = context
        .host_call(
            "storage.semantic",
            json!({
                "operation": operation,
                "project_root": project_root,
                "query": query,
                "k": k,
                "engine_id": embedded.engine_id,
                "model": embedded.model,
            }),
        )
        .ok()?;
    output["results"]
        .as_array()?
        .iter()
        .map(|record| {
            Some((
                record["id"].as_str()?.to_owned(),
                record["score"].as_f64()? as f32,
            ))
        })
        .collect()
}

fn failed_vector_search() -> VectorSearch {
    VectorSearch {
        index_state: "failed".into(),
        results: None,
    }
}

fn declared_index_state(
    context: &mut PluginContext<'_>,
    project_root: Option<&str>,
) -> Option<String> {
    let project_root = project_root?;
    let state_key = storage::project_key(project_root, "search-index", &["state"]);
    storage::get::<SearchIndexState>(context, SEARCH_INDEX_STATE, &state_key)
        .ok()
        .flatten()
        .map(|state| state.state)
}

pub(crate) struct EmbedBatch {
    pub(crate) vectors: Vec<Vec<f32>>,
    pub(crate) engine_id: String,
    pub(crate) model: Option<String>,
    pub(crate) profile_fingerprint: Option<String>,
}

impl EmbedBatch {
    fn empty() -> Self {
        Self {
            vectors: Vec::new(),
            engine_id: String::new(),
            model: None,
            profile_fingerprint: None,
        }
    }
}

/// What a `neural.embed` call embeds for: search queries and stored content are embedded differently.
#[derive(Clone, Copy)]
pub(crate) enum EmbedTask {
    Document,
    Query,
}

impl EmbedTask {
    fn wire(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Query => "query",
        }
    }
}

/// Batches by the existing inference count contract, never by transport byte budgets.
pub(crate) fn embed_batch_ranges(texts: &[&str]) -> Vec<Range<usize>> {
    (0..texts.len())
        .step_by(MAX_NEURAL_EMBED_TEXTS)
        .map(|start| start..(start + MAX_NEURAL_EMBED_TEXTS).min(texts.len()))
        .collect()
}

pub(crate) fn embed_texts(
    context: &mut PluginContext<'_>,
    texts: &[&str],
    task: EmbedTask,
) -> Result<EmbedBatch, PluginError> {
    embed_texts_guarded(context, texts, task, None)
}

pub(crate) fn embed_texts_guarded(
    context: &mut PluginContext<'_>,
    texts: &[&str],
    task: EmbedTask,
    expected_profile: Option<&str>,
) -> Result<EmbedBatch, PluginError> {
    let mut combined = EmbedBatch::empty();
    for range in embed_batch_ranges(texts) {
        let mut input = json!({"texts": &texts[range.clone()], "task": task.wire()});
        if let Some(profile) = expected_profile {
            input["expected_profile_fingerprint"] = json!(profile);
        }
        let output = context.host_call("neural.embed", input)?;
        let batch = parse_embed_output(output, range.len(), expected_profile)?;
        if !combined.vectors.is_empty()
            && (combined.engine_id != batch.engine_id
                || combined.model != batch.model
                || combined.profile_fingerprint != batch.profile_fingerprint)
        {
            return Err(PluginError::new(
                "rebuild_profile_conflict",
                "embedding service changed between inference batches",
                true,
            ));
        }
        combined.engine_id = batch.engine_id;
        combined.model = batch.model;
        combined.profile_fingerprint = batch.profile_fingerprint;
        combined.vectors.extend(batch.vectors);
    }
    let dimensions = combined.vectors.first().map_or(0, Vec::len);
    if dimensions == 0
        || combined
            .vectors
            .iter()
            .any(|vector| vector.len() != dimensions)
    {
        return Err(embedding_error(
            "expected equal non-empty vector dimensions",
        ));
    }
    Ok(combined)
}

fn parse_embed_output(
    output: Value,
    expected: usize,
    expected_profile: Option<&str>,
) -> Result<EmbedBatch, PluginError> {
    let values = output["vectors"]
        .as_array()
        .filter(|values| values.len() == expected)
        .ok_or_else(|| embedding_error("expected one vector per source text"))?;
    let vectors = values
        .iter()
        .map(|value| vector(value).ok_or_else(|| embedding_error("expected finite vector numbers")))
        .collect::<Result<Vec<_>, _>>()?;
    let profile_fingerprint = output["profile_fingerprint"].as_str().map(str::to_string);
    if expected_profile.is_some_and(|expected| Some(expected) != profile_fingerprint.as_deref()) {
        return Err(PluginError::new(
            "rebuild_profile_conflict",
            "embedding profile differs from pinned plan",
            true,
        ));
    }
    Ok(EmbedBatch {
        vectors,
        engine_id: output["engine_id"]
            .as_str()
            .ok_or_else(|| embedding_error("expected engine_id"))?
            .into(),
        model: output["model"].as_str().map(str::to_string),
        profile_fingerprint,
    })
}

fn embedding_error(reason: impl Into<String>) -> PluginError {
    PluginError::new(
        "semantic_embedding_failed",
        format!("embedding batch failed: {}", reason.into()),
        true,
    )
}

fn vector(value: &Value) -> Option<Vec<f32>> {
    value
        .as_array()?
        .iter()
        .map(|number| {
            let value = number.as_f64()? as f32;
            value.is_finite().then_some(value)
        })
        .collect()
}

fn lexical_results(query: &str, elements: &[SemanticElement]) -> Vec<SemanticSearchResultV2> {
    let query = query.trim().to_ascii_lowercase();
    elements
        .iter()
        .filter_map(|element| {
            lexical_score(
                &element.name.to_ascii_lowercase(),
                &element.semantic_element_id.to_ascii_lowercase(),
                &query,
            )
            .map(|score| SemanticSearchResultV2 {
                element: element.view(element.parent_element_id.clone()),
                score,
            })
        })
        .collect()
}

fn lexical_score(name: &str, id: &str, query: &str) -> Option<f32> {
    if name == query {
        return Some(1.0);
    }
    if name.contains(query) {
        return Some(0.8);
    }
    let tokens = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return id.contains(query).then_some(0.4);
    }
    let mut matched = 0;
    let mut field_score = 0.0;
    for token in &tokens {
        let score = if name.contains(token) {
            0.8
        } else if id.contains(token) {
            0.4
        } else {
            0.0
        };
        matched += usize::from(score > 0.0);
        field_score += score;
    }
    (matched > 0)
        .then_some(
            (matched as f32 / tokens.len() as f32 * 0.4)
                + (field_score / tokens.len() as f32 * 0.4),
        )
        .or_else(|| id.contains(query).then_some(0.4))
}

pub(crate) fn collect_descendants(
    id: &str,
    children: &HashMap<String, Vec<String>>,
    ids: &mut HashSet<String>,
) {
    for child in children.get(id).cloned().unwrap_or_default() {
        if ids.insert(child.clone()) {
            collect_descendants(&child, children, ids);
        }
    }
}

fn tree_node(
    id: &str,
    depth: usize,
    max_depth: usize,
    by_id: &HashMap<&str, &SemanticElement>,
    children: &HashMap<String, Vec<String>>,
    parents: &HashMap<String, String>,
    visited: &mut HashSet<String>,
) -> Option<Value> {
    let element = by_id.get(id)?;
    if !visited.insert(id.to_owned()) {
        return None;
    }
    let nodes = if depth >= max_depth {
        Vec::new()
    } else {
        children
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|child| {
                tree_node(
                    child,
                    depth + 1,
                    max_depth,
                    by_id,
                    children,
                    parents,
                    visited,
                )
            })
            .collect()
    };
    visited.remove(id);
    Some(json!({
        "element": element.view(parents.get(id).cloned()),
        "children": nodes
    }))
}

fn tree_node_ids(value: &Value) -> Vec<String> {
    let mut ids = value["element"]["semantic_element_id"]
        .as_str()
        .map(str::to_owned)
        .into_iter()
        .collect::<Vec<_>>();
    for child in value["children"].as_array().into_iter().flatten() {
        ids.extend(tree_node_ids(child));
    }
    ids
}

fn root_ids(
    elements: &[SemanticElement],
    parents: &HashMap<String, String>,
    requested: Option<&str>,
) -> Vec<String> {
    if let Some(id) = requested
        && elements
            .iter()
            .any(|element| element.semantic_element_id == id)
    {
        return vec![id.to_owned()];
    }
    let ids = elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    elements
        .iter()
        .filter(|element| {
            parents
                .get(&element.semantic_element_id)
                .is_none_or(|parent| !ids.contains(parent.as_str()))
        })
        .map(|element| element.semantic_element_id.clone())
        .collect()
}

fn children_by_parent(relationships: &[SemanticRelationship]) -> HashMap<String, Vec<String>> {
    let mut children = BTreeMap::<String, Vec<String>>::new();
    for relationship in relationships
        .iter()
        .filter(|relationship| is_contains(relationship))
    {
        children
            .entry(relationship.source_element_id.clone())
            .or_default()
            .push(relationship.target_element_id.clone());
    }
    children.into_iter().collect()
}

fn parent_by_child(relationships: &[SemanticRelationship]) -> HashMap<String, String> {
    relationships
        .iter()
        .filter(|relationship| is_contains(relationship))
        .map(|relationship| {
            (
                relationship.target_element_id.clone(),
                relationship.source_element_id.clone(),
            )
        })
        .collect()
}

fn parent_depths(
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
) -> HashMap<String, usize> {
    let parents = parent_by_child(relationships);
    elements
        .iter()
        .map(|element| {
            let mut seen = HashSet::new();
            let mut depth = 0;
            let mut current = parents.get(&element.semantic_element_id);
            while let Some(parent) = current {
                if !seen.insert(parent) {
                    break;
                }
                depth += 1;
                current = parents.get(parent);
            }
            (element.semantic_element_id.clone(), depth)
        })
        .collect()
}

fn location_rank(
    element: &SemanticElement,
    depths: &HashMap<String, usize>,
) -> (bool, i64, i64, usize) {
    let ranged = element.start_line.is_some() || element.end_line.is_some();
    let width = match (element.start_line, element.end_line) {
        (Some(start), Some(end)) => -(end - start).max(0),
        _ => i64::MIN,
    };
    (
        ranged,
        width,
        element.start_line.unwrap_or(0),
        *depths.get(&element.semantic_element_id).unwrap_or(&0),
    )
}

fn normalize_location_path(project_root: &str, path: &str) -> Option<String> {
    let path = path.trim();
    if !path.starts_with('/') {
        return Some(normalize_path(path));
    }

    let path = normalized_absolute_path(path);
    let project_root = normalized_absolute_path(project_root);
    if project_root == "/" {
        return Some(normalize_path(&path));
    }
    path.strip_prefix(&project_root)
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        .map(normalize_path)
}

fn normalized_absolute_path(path: &str) -> String {
    let path = normalize_path(path);
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        "/".to_owned()
    } else {
        format!("/{path}")
    }
}

fn normalize_path(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

fn active_relationships(relationships: Vec<SemanticRelationship>) -> Vec<SemanticRelationship> {
    relationships
        .into_iter()
        .filter(|relationship| relationship.lifecycle == "active")
        .collect()
}

fn is_active(element: &SemanticElement) -> bool {
    element.lifecycle == "active"
}

fn is_contains(relationship: &SemanticRelationship) -> bool {
    relationship.relationship_kind == "contains" || relationship.label == "contains"
}
fn invalid_input(value: &str, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_semantic_input",
        format!("invalid Semantic input `{value}`; expected {expected}"),
        false,
    )
}

fn sort_elements(elements: &mut [SemanticElement]) {
    elements.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.start_line.cmp(&right.start_line))
            .then(left.semantic_element_id.cmp(&right.semantic_element_id))
    });
}

/// Requests graph-owned aggregation; only grouped counts cross the plugin boundary.
pub(crate) fn project_element_counts(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let root = input["project_root"].as_str().unwrap_or_default();
    require_non_empty(root, "project_root")?;
    storage::project_element_counts(context, root)
}

#[cfg(test)]
mod tests {
    use super::embed_batch_ranges;

    #[test]
    fn embed_batches_hold_at_most_128_texts() {
        let texts = vec!["a"; 300];
        assert_eq!(embed_batch_ranges(&texts), vec![0..128, 128..256, 256..300]);
    }

    #[test]
    fn embed_batches_accept_large_texts_without_byte_budgets() {
        // Token-planned source ranges may be large in bytes.
        let text = "a".repeat(40 * 1024);
        let texts = vec![text.as_str(); 13];
        assert_eq!(embed_batch_ranges(&texts), vec![0..13]);
    }

    #[test]
    fn one_large_text_does_not_change_element_vector_association() {
        let big = "a".repeat(300 * 1024);
        let texts = ["x", big.as_str(), "y"];
        assert_eq!(embed_batch_ranges(&texts), vec![0..3]);
    }
}
