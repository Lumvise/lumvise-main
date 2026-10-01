use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use lumvise_contracts::OBSIDIAN_SYNC_SCHEMA_VERSION;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::KnowledgeArtifact;

const PROJECTION_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_CACHED_PROJECTS: usize = 4;

#[derive(Clone)]
struct CachedProjectProjection {
    commit_version: i64,
    created_at: Instant,
    projection: Arc<Value>,
}

#[derive(Default)]
pub(crate) struct ProjectProjectionCache {
    projections: Mutex<BTreeMap<String, CachedProjectProjection>>,
}

pub(crate) fn command(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let project_root = input["projectRoot"].as_str().ok_or_else(|| {
        PluginError::new(
            "invalid_knowledge_projection",
            format!("invalid projection input `{input}`; expected projectRoot string"),
            false,
        )
    })?;
    project(project_root, context)
}

pub(crate) fn project(
    project_root: &str,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let snapshot = crate::semantic_context::load_projection_snapshot(context, project_root)?;
    let mut projection =
        build_project_projection(project_root, snapshot.semantic, snapshot.artifacts)?;
    let signature = json!([projection["spaces"], projection["elements"]]).to_string();
    projection["projectRoot"] = json!(project_root);
    projection["generatedAt"] = json!(Utc::now().to_rfc3339());
    projection["syncToken"] = json!(hex::encode(Sha256::digest(signature)));
    Ok(projection)
}

pub(crate) fn sync(
    project_root: &str,
    client_hashes: &Value,
    applied_revision: Option<i64>,
    context: &mut PluginContext<'_>,
    cache: &ProjectProjectionCache,
) -> Result<Value, PluginError> {
    let cached = current_or_build_projection(cache, project_root, context)?;
    sync_batch(
        cached.projection.as_ref(),
        client_hashes,
        applied_revision,
        cached.commit_version,
    )
}

fn sync_batch(
    projection: &Value,
    client_hashes: &Value,
    applied_revision: Option<i64>,
    target_revision: i64,
) -> Result<Value, PluginError> {
    let elements = projection["elements"]
        .as_array()
        .ok_or_else(|| invalid_projection(projection))?;
    let page_hashes = element_hashes(elements);
    let changed_pages = elements
        .iter()
        .filter(|element| is_changed(element, client_hashes))
        .cloned()
        .collect::<Vec<_>>();
    let deleted_page_ids = deleted_page_ids(client_hashes, &page_hashes);
    let base_revision = applied_revision.filter(|revision| *revision <= target_revision);
    Ok(json!({"schemaVersion": OBSIDIAN_SYNC_SCHEMA_VERSION,
        "sourceId": projection["projectRoot"], "baseRevision": base_revision,
        "targetRevision": target_revision, "fullReset": applied_revision.is_none() || base_revision.is_none(),
        "generatedAt": projection["generatedAt"], "syncToken": projection["syncToken"],
        "spaces": projection["spaces"], "pageHashes": &page_hashes,
        "changedPages": changed_pages, "deletedPageIds": deleted_page_ids}))
}

fn current_or_build_projection(
    cache: &ProjectProjectionCache,
    project_root: &str,
    context: &mut PluginContext<'_>,
) -> Result<CachedProjectProjection, PluginError> {
    let revision = crate::semantic_context::current_revision(context)?;
    if let Some(cached) = cached_projection_for_revision(cache, project_root, revision)? {
        return Ok(cached);
    }
    let snapshot = crate::semantic_context::load_projection_snapshot(context, project_root)?;
    let sync_token = snapshot_sync_token(project_root, &snapshot.semantic, &snapshot.artifacts)?;
    let published_at = snapshot.published_at.clone();
    let mut projection =
        build_project_projection(project_root, snapshot.semantic, snapshot.artifacts)?;
    projection["projectRoot"] = json!(project_root);
    projection["generatedAt"] = json!(published_at);
    projection["syncToken"] = json!(&sync_token);
    cache_projection(
        cache,
        project_root,
        CachedProjectProjection {
            commit_version: snapshot.commit_version,
            created_at: Instant::now(),
            projection: Arc::new(projection),
        },
    )
}

fn build_project_projection(
    project_root: &str,
    semantic: crate::semantic_context::SemanticContext,
    artifacts: Vec<KnowledgeArtifact>,
) -> Result<Value, PluginError> {
    let reports = artifacts
        .iter()
        .filter(|artifact| crate::obsidian_reports::is_report(artifact))
        .map(|artifact| crate::obsidian_reports::project(project_root, artifact))
        .collect::<Vec<_>>();
    let mut projection = crate::project_structure::project(project_root, semantic, artifacts)?;
    append_report_projection(&mut projection, reports)?;
    Ok(projection)
}

fn append_report_projection(
    projection: &mut Value,
    reports: Vec<Value>,
) -> Result<(), PluginError> {
    if reports.is_empty() {
        return Ok(());
    }
    projection["spaces"]
        .as_array_mut()
        .ok_or_else(invalid_projection_shape)?
        .push(
            json!({"spaceId": "nuclei", "title": "Nuclei", "status": "ready",
            "writePolicy": "readonly"}),
        );
    projection["elements"]
        .as_array_mut()
        .ok_or_else(invalid_projection_shape)?
        .extend(reports);
    Ok(())
}

fn cached_projection_for_revision(
    projection_cache: &ProjectProjectionCache,
    project_root: &str,
    revision: i64,
) -> Result<Option<CachedProjectProjection>, PluginError> {
    let mut cache = lock_projection_cache(projection_cache)?;
    discard_expired_projections(&mut cache);
    Ok(cache
        .get(project_root)
        .filter(|cached| cached.commit_version == revision)
        .cloned())
}

fn cache_projection(
    projection_cache: &ProjectProjectionCache,
    project_root: &str,
    projection: CachedProjectProjection,
) -> Result<CachedProjectProjection, PluginError> {
    let mut cache = lock_projection_cache(projection_cache)?;
    discard_expired_projections(&mut cache);
    while cache.len() >= MAX_CACHED_PROJECTS && !cache.contains_key(project_root) {
        let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, cached)| cached.created_at)
            .map(|(root, _)| root.clone())
        else {
            break;
        };
        cache.remove(&oldest);
    }
    cache.insert(project_root.to_owned(), projection.clone());
    Ok(projection)
}

fn lock_projection_cache(
    cache: &ProjectProjectionCache,
) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, CachedProjectProjection>>, PluginError> {
    cache
        .projections
        .lock()
        .map_err(|_| projection_cache_error())
}

fn discard_expired_projections(cache: &mut BTreeMap<String, CachedProjectProjection>) {
    cache.retain(|_, cached| cached.created_at.elapsed() < PROJECTION_CACHE_TTL);
}

fn snapshot_sync_token(
    project_root: &str,
    semantic: &crate::semantic_context::SemanticContext,
    artifacts: &[KnowledgeArtifact],
) -> Result<String, PluginError> {
    let encoded = serde_json::to_vec(&(
        project_root,
        &semantic.elements,
        &semantic.relationships,
        artifacts,
    ))
    .map_err(|error| {
        PluginError::new(
            "invalid_knowledge_projection",
            format!(
                "failed to encode Knowledge snapshot for project `{project_root}`; expected serializable graph snapshot: {error}"
            ),
            false,
        )
    })?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

pub(crate) fn page(
    project_root: &str,
    element_id: &str,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let projection = project(project_root, context)?;
    projection["elements"]
        .as_array()
        .and_then(|elements| elements.iter().find(|item| item["elementId"] == element_id))
        .cloned()
        .ok_or_else(|| PluginError::new(
            "knowledge_projection_not_found",
            format!("projection element `{element_id}` is missing; expected element in project `{project_root}`"),
            false,
        ))
}

#[cfg(test)]
pub(crate) fn sync_delta(projection: Value, client_hashes: &Value) -> Result<Value, PluginError> {
    let elements = projection["elements"]
        .as_array()
        .ok_or_else(|| invalid_projection(&projection))?;
    let page_hashes = element_hashes(elements);
    let changed_pages = elements
        .iter()
        .filter(|element| is_changed(element, client_hashes))
        .cloned()
        .collect::<Vec<_>>();
    let deleted_page_ids = deleted_page_ids(client_hashes, &page_hashes);
    Ok(json!({"sourceId": projection["projectRoot"],
        "generatedAt": projection["generatedAt"], "syncToken": projection["syncToken"],
        "spaces": projection["spaces"], "pageHashes": &page_hashes,
        "lumviseContentMd5": &page_hashes, "changedPages": changed_pages,
        "deletedPageIds": deleted_page_ids}))
}

fn is_changed(element: &Value, client_hashes: &Value) -> bool {
    client_hashes[element["elementId"].as_str().unwrap_or_default()] != element["contentMd5"]
}

fn deleted_page_ids(client_hashes: &Value, all_hashes: &BTreeMap<String, String>) -> Vec<String> {
    client_hashes
        .as_object()
        .into_iter()
        .flat_map(|hashes| hashes.keys())
        .filter(|id| !all_hashes.contains_key(*id))
        .cloned()
        .collect()
}

fn element_hashes(elements: &[Value]) -> BTreeMap<String, String> {
    elements
        .iter()
        .filter_map(|element| {
            Some((
                element["elementId"].as_str()?.into(),
                element["contentMd5"].as_str()?.into(),
            ))
        })
        .collect()
}

fn projection_cache_error() -> PluginError {
    PluginError::new(
        "knowledge_projection_cache_failed",
        "Knowledge projection cache lock is poisoned; expected available plugin state",
        true,
    )
}

fn invalid_projection(value: &Value) -> PluginError {
    PluginError::new(
        "invalid_knowledge_projection",
        format!("invalid Knowledge projection `{value}`; expected elements array"),
        false,
    )
}

fn invalid_projection_shape() -> PluginError {
    PluginError::new(
        "invalid_knowledge_projection",
        "invalid Knowledge projection shape; expected spaces and elements arrays",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_delta_returns_one_complete_reduced_batch() {
        let projection = test_projection(501);
        let batch = sync_delta(projection, &json!({})).expect("complete batch");

        assert_eq!(
            batch["pageHashes"].as_object().map(|items| items.len()),
            Some(501)
        );
        assert_eq!(batch["changedPages"].as_array().map(Vec::len), Some(501));
        assert!(batch.get("nextCursor").is_none());
    }

    fn test_projection(count: usize) -> Value {
        let elements = (0..count)
            .map(|index| {
                json!({"elementId": format!("file:{index:04}"),
                    "contentMd5": format!("hash:{index:04}"), "markdown": "# Test"})
            })
            .collect::<Vec<_>>();
        json!({"projectRoot": "/project", "generatedAt": "now",
            "syncToken": "snapshot", "spaces": [], "elements": elements})
    }
}
