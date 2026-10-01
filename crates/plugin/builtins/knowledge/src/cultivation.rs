use chrono::Utc;
use std::collections::HashSet;

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    KnowledgeArtifact, artifact, artifact_generation, functional, semantic_context,
    semantic_context::{SemanticContext, SemanticElement},
    storage,
};

const VALID_MODES: &[&str] = &[
    "cultivate_from_scratch",
    "refresh_cultivation",
    "deepen_abstraction",
    "synthesize_nucleus",
    "diagnose_on_request",
];
const MAX_C4_ELEMENTS: usize = 120;
const C4_REPORT_INTENTS: &str = "knowledge_c4_report_intents";

#[derive(Deserialize, Serialize)]
struct C4ReportIntent {
    project_root: String,
    target_id: String,
}

#[derive(Deserialize)]
struct CultivationRequest {
    mode: String,
    project_root: String,
    target_id: Option<String>,
    /// Path form of the scope. The workspace UI only knows paths, so without
    /// this every "Generate knowledge" click cultivated the whole project and
    /// overran the invocation deadline.
    target_path: Option<String>,
}

#[derive(Deserialize)]
struct C4Request {
    project_root: String,
    target_id: Option<String>,
    target_path: Option<String>,
    refresh: Option<bool>,
    /// Resubmits elements whose earlier generation failed, without forcing a
    /// report rewrite. `refresh` implies it.
    retry_failed: Option<bool>,
    provider_id: Option<String>,
    model: Option<String>,
}
pub(crate) fn run(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: CultivationRequest = crate::parse(input, "Knowledge cultivation request")?;
    validate_cultivation(&request)?;
    let mut semantic = semantic_context::load(context, &request.project_root, None)?;
    if let Some(root) = cultivation_root(&semantic.elements, &request)? {
        semantic_context::retain_subtree(&mut semantic, &root, &request.project_root)?;
    }
    let artifact_ids = if request.mode == "synthesize_nucleus" {
        Vec::new()
    } else {
        materialize_functional(&request, &semantic, context)?
    };
    let run = run_value(&request, artifact_ids);
    storage::put(
        context,
        storage::CULTIVATION_RUNS,
        run["run_id"].as_str().unwrap_or_default(),
        &run,
    )?;
    Ok(json!({"run": run}))
}

/// The element a cultivation run is scoped to, from `target_id` or the
/// equivalent `target_path` (same matching rule as the C4 target resolver).
fn cultivation_root(
    elements: &[SemanticElement],
    request: &CultivationRequest,
) -> Result<Option<String>, PluginError> {
    if let Some(id) = request.target_id.as_deref() {
        return Ok(Some(id.to_owned()));
    }
    let Some(path) = request.target_path.as_deref() else {
        return Ok(None);
    };
    let path = path.trim_matches('/');
    let element = elements
        .iter()
        .filter(|item| {
            crate::c4_text::c4_target_path(item) == path || item.path.trim_matches('/') == path
        })
        .min_by_key(|item| target_rank(&item.element_kind))
        .ok_or_else(|| {
            missing(
                "semantic_element_not_found",
                path,
                "cultivation target path",
            )
        })?;
    Ok(Some(element.semantic_element_id.clone()))
}

pub(crate) fn get_run(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let run_id = crate::required_input(&input, "run_id")?;
    let run = storage::get::<Value>(context, storage::CULTIVATION_RUNS, &run_id)?
        .ok_or_else(|| missing("cultivation_run_not_found", &run_id, "cultivation run"))?;
    Ok(json!({"run": run}))
}

pub(crate) fn ensure_c4(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: C4Request = crate::parse(input, "scoped C4 nucleus request")?;
    artifact::require(&request.project_root, "project_root")?;
    if request.target_id.is_none() && request.target_path.is_none() {
        return Err(missing(
            "invalid_knowledge_input",
            "target_id,target_path",
            "one scoped C4 target",
        ));
    }
    artifact_generation::poll(context, Some(&request.project_root))?;
    let scoped = scoped_c4_context(&request, context)?;
    let generation_elements = crate::c4_report::functional_summary_elements(
        &scoped.elements,
        &scoped.target,
        &scoped.relationships,
    );
    let submission = artifact_generation::submit_missing(
        &generation_elements,
        &scoped.artifacts,
        request.refresh.unwrap_or(false) || request.retry_failed.unwrap_or(false),
        artifact_generation::SubmitPriority::Interactive,
        context,
    )?;
    let state = artifact_generation::state_for(&generation_elements, context)?;
    let report = crate::c4_report::report(
        &request.project_root,
        &scoped.target,
        &scoped.elements,
        &scoped.relationships,
        &scoped.artifacts,
        &scoped.fingerprint,
    );
    let existing = storage::get_knowledge(context, &report.artifact_id)?;
    let created = existing.is_none();
    let current = existing.as_ref().is_some_and(|item| {
        item.metadata["nucleus"]["dependency_fingerprint"] == scoped.fingerprint
    });
    if created || request.refresh.unwrap_or(false) || !current {
        storage::put_knowledge(context, &report)?;
    }
    remember_c4_report_intent(
        &request.project_root,
        &scoped.target.semantic_element_id,
        context,
    )?;
    let artifact = existing
        .filter(|_| !request.refresh.unwrap_or(false) && current)
        .unwrap_or(report);
    Ok(c4_response(
        &request,
        &scoped,
        &generation_elements,
        artifact,
        created,
        state,
        submission,
    ))
}

fn remember_c4_report_intent(
    project_root: &str,
    target_id: &str,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let intent = C4ReportIntent {
        project_root: project_root.to_owned(),
        target_id: target_id.to_owned(),
    };
    storage::put(
        context,
        C4_REPORT_INTENTS,
        &c4_report_intent_key(project_root, target_id),
        &intent,
    )
}

pub(crate) fn forget_c4_report_intent(
    project_root: &str,
    target_id: &str,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    storage::delete(
        context,
        C4_REPORT_INTENTS,
        &c4_report_intent_key(project_root, target_id),
    )
}

fn c4_report_intents(
    project_root: &str,
    context: &mut PluginContext<'_>,
) -> Result<Vec<C4ReportIntent>, PluginError> {
    let prefix = format!("{project_root}\0");
    storage::list(context, C4_REPORT_INTENTS, Some(&prefix))
}

fn c4_report_intent_key(project_root: &str, target_id: &str) -> String {
    format!("{project_root}\0{target_id}")
}

pub(crate) fn debug_c4(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: C4Request = crate::parse(input, "scoped C4 diagnostic request")?;
    let scoped = scoped_c4_context(&request, context)?;
    let report_id = crate::c4_report::report_id(&scoped.target.semantic_element_id);
    let report = storage::get_knowledge(context, &report_id)?;
    Ok(json!({
        "status": "ok", "projectRoot": request.project_root,
        "requestedTarget": element_snapshot(&scoped.requested),
        "reportTarget": element_snapshot(&scoped.target),
        "scope": {"descendantElementCount": scoped.elements.len(),
            "scopedElementCount": scoped.elements.len(),
            "descendantElementIds": element_ids(&scoped.elements),
            "scopedElementIds": element_ids(&scoped.elements)},
        "functionalArtifacts": scoped.artifacts.iter().map(|item| json!({
            "artifactId": item.artifact_id, "semanticElementId": item.semantic_element_id,
            "job": item.metadata["job"], "jobStatus": item.metadata["job_status"]
        })).collect::<Vec<_>>(),
        "report": {"artifactId": report_id, "exists": report.is_some(), "artifact": report,
            "dependencyFingerprint": scoped.fingerprint}
    }))
}

pub(crate) fn refresh_reports_for_change_batch(
    request: &lumvise_plugin_sdk::StorageTriggerRequest,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let changed = request
        .changed
        .iter()
        .map(|item| item.entity_id.clone())
        .collect::<Vec<_>>();
    refresh_reports_for_changed_elements(&request.project_root, &changed, context)
}

pub(crate) fn refresh_reports_for_changed_elements(
    project_root: &str,
    changed_element_ids: &[String],
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    use std::collections::{BTreeMap, BTreeSet, HashSet};

    let changed: BTreeSet<String> = changed_element_ids.iter().cloned().collect();
    if changed.is_empty() {
        return Ok(());
    }
    let changed_ids = changed.iter().cloned().collect::<HashSet<_>>();
    let changed_elements = storage::elements_by_ids(context, project_root, &changed_ids)?;
    let changed_artifacts = if changed_elements.is_empty() {
        Vec::new()
    } else {
        storage::artifacts_for_elements(context, &changed_ids)?
    };
    refresh_existing_functional_summaries(&changed_elements, &changed_artifacts, context)?;
    let mut elements_by_id = changed_elements
        .into_iter()
        .map(|element| (element.semantic_element_id.clone(), element))
        .collect::<BTreeMap<_, _>>();
    let mut resolved_ids = changed.clone();
    let mut anchors = BTreeSet::new();
    let mut frontier = changed.clone();
    let mut seen = BTreeSet::new();
    while !frontier.is_empty() {
        let missing_ids = frontier
            .iter()
            .filter(|id| !resolved_ids.contains(*id))
            .cloned()
            .collect::<HashSet<_>>();
        if !missing_ids.is_empty() {
            resolved_ids.extend(missing_ids.iter().cloned());
            for element in storage::elements_by_ids(context, project_root, &missing_ids)? {
                elements_by_id.insert(element.semantic_element_id.clone(), element);
            }
        }
        let mut next = BTreeSet::new();
        for element in frontier.iter().filter_map(|id| elements_by_id.get(id)) {
            if !seen.insert(element.semantic_element_id.clone()) {
                continue;
            }
            anchors.insert(element.semantic_element_id.clone());
            if let Some(parent) = element.parent_element_id.as_ref()
                && !seen.contains(parent)
            {
                next.insert(parent.clone());
            }
        }
        frontier = next;
    }
    if anchors.is_empty() {
        return Ok(());
    }
    let mut reports = BTreeMap::new();
    for anchor in &anchors {
        let anchor_artifacts = if changed.contains(anchor) {
            changed_artifacts
                .iter()
                .filter(|artifact| artifact.semantic_element_id == *anchor)
                .cloned()
                .collect()
        } else {
            storage::artifacts_for_elements(context, &HashSet::from([anchor.clone()]))?
        };
        for report in anchor_artifacts {
            if !report.tags.iter().any(|tag| tag == "scoped-c4") {
                continue;
            }
            let target_id = report.metadata["nucleus"]["target_element_id"]
                .as_str()
                .unwrap_or(&report.semantic_element_id)
                .to_owned();
            if anchors.contains(&target_id)
                || report.metadata["nucleus"]["linked_element_ids"]
                    .as_array()
                    .is_some_and(|linked| {
                        linked
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|id| changed.contains(id))
                    })
            {
                reports.insert(report.artifact_id.clone(), report);
            }
        }
    }
    for intent in c4_report_intents(project_root, context)? {
        if !anchors.contains(&intent.target_id) {
            continue;
        }
        let matching_ids = reports
            .iter()
            .filter(|(_, report)| {
                report.metadata["nucleus"]["target_element_id"]
                    .as_str()
                    .unwrap_or(&report.semantic_element_id)
                    == intent.target_id.as_str()
            })
            .map(|(artifact_id, _)| artifact_id.clone())
            .collect::<Vec<_>>();
        if matching_ids.is_empty() {
            refresh_selected_report(project_root, &intent.target_id, None, context)?;
        } else {
            for artifact_id in matching_ids {
                let existing = reports
                    .remove(&artifact_id)
                    .expect("report collected from refresh candidates");
                refresh_selected_report(project_root, &intent.target_id, Some(existing), context)?;
            }
        }
    }
    for report in reports.into_values() {
        let target_id = report.metadata["nucleus"]["target_element_id"]
            .as_str()
            .unwrap_or(&report.semantic_element_id)
            .to_owned();
        refresh_selected_report(project_root, &target_id, Some(report), context)?;
    }
    Ok(())
}

fn refresh_existing_functional_summaries(
    changed_elements: &[SemanticElement],
    artifacts: &[KnowledgeArtifact],
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    if changed_elements.is_empty() || artifacts.is_empty() {
        return Ok(());
    }
    let existing_ids = artifacts
        .iter()
        .filter(|artifact| {
            artifact
                .tags
                .iter()
                .any(|tag| tag == "cultivation-functional")
        })
        .map(|artifact| artifact.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    let candidates = changed_elements
        .iter()
        .filter(|element| existing_ids.contains(element.semantic_element_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(());
    }
    artifact_generation::submit_missing(
        &candidates,
        artifacts,
        false,
        artifact_generation::SubmitPriority::Background,
        context,
    )?;
    Ok(())
}

fn refresh_selected_report(
    project_root: &str,
    target_id: &str,
    existing: Option<KnowledgeArtifact>,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let Some(subgraph) = storage::selective_subgraph(context, project_root, target_id)? else {
        if let Some(existing) = existing {
            storage::remove_knowledge(context, &existing.artifact_id)?;
        }
        return Ok(());
    };
    let target = subgraph
        .elements
        .iter()
        .find(|element| element.semantic_element_id == target_id && element.lifecycle == "active")
        .cloned()
        .ok_or_else(|| {
            missing(
                "semantic_target_inactive",
                target_id,
                "active report target",
            )
        })?;
    let artifacts = storage::decode_selective_knowledge_artifacts(&subgraph.artifacts)?;
    let fingerprint =
        crate::c4_report::fingerprint(&subgraph.elements, &subgraph.relationships, &artifacts);
    let generation_elements = crate::c4_report::functional_summary_elements(
        &subgraph.elements,
        &target,
        &subgraph.relationships,
    );
    artifact_generation::submit_missing(
        &generation_elements,
        &artifacts,
        false,
        artifact_generation::SubmitPriority::Background,
        context,
    )?;
    let report = crate::c4_report::report(
        project_root,
        &target,
        &subgraph.elements,
        &subgraph.relationships,
        &artifacts,
        &fingerprint,
    );
    let current = existing.as_ref().is_some_and(|existing| {
        existing.metadata["nucleus"]["dependency_fingerprint"].as_str()
            == Some(fingerprint.as_str())
            && existing.metadata["nucleus"]["target_element_id"].as_str() == Some(target_id)
    });
    if !current {
        storage::put_knowledge(context, &report)?;
    }
    Ok(())
}

fn c4_response(
    request: &C4Request,
    scoped: &ScopedC4Context,
    generation_elements: &[SemanticElement],
    artifact: KnowledgeArtifact,
    created: bool,
    state: artifact_generation::ArtifactGenerationState,
    submission: artifact_generation::ArtifactSubmission,
) -> Value {
    let missing = artifact_generation::missing_element_ids(generation_elements, &scoped.artifacts);
    let artifact_id = artifact.artifact_id.clone();
    let target_path = artifact.metadata["obsidian"]["path"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let failed: std::collections::BTreeSet<&str> = state
        .failures
        .iter()
        .map(|failure| failure.semantic_element_id.as_str())
        .collect();
    let failed_ids: Vec<&String> = missing
        .iter()
        .filter(|id| failed.contains(id.as_str()))
        .collect();
    let outstanding = missing.len() - failed_ids.len();
    let first_failure = state.failures.first();
    if missing.is_empty() {
        return json!({"status": "ready", "created": created, "artifact": artifact,
            "provider_id": request.provider_id, "model": request.model});
    }
    if outstanding == 0 && state.pending_element_ids.is_empty() {
        // Nothing generated at all is a real failure (engine or credentials);
        // otherwise the report is usable and names what it lacks.
        if failed_ids.len() >= artifact_generation::generatable_count(generation_elements)
            && let Some(failure) = first_failure
        {
            return json!({"status": "not_ready", "reason_code": failure.reason_code,
                "reason": failure.reason, "target_fingerprint": scoped.fingerprint,
                "artifact_id": artifact_id, "target_path": target_path});
        }
        return json!({"status": "ready", "created": created, "artifact": artifact,
            "provider_id": request.provider_id, "model": request.model,
            "failed_element_ids": failed_ids,
            "failure_reason": first_failure.map(|failure| failure.reason.as_str())
                .unwrap_or_default()});
    }
    if submission.provider_unavailable && state.pending_element_ids.is_empty() {
        return json!({"status": "not_ready", "reason_code": "provider_unavailable",
            "reason": "no connected MCP provider advertises semantic artifact generation",
            "target_fingerprint": scoped.fingerprint, "artifact_id": artifact_id,
            "target_path": target_path});
    }
    json!({"status": "pending", "request_id": c4_request_id(scoped),
        "target_fingerprint": scoped.fingerprint, "missing_element_ids": missing,
        "pending_element_ids": state.pending_element_ids,
        "failed_element_ids": failed_ids,
        "submitted": submission.submitted, "artifact_id": artifact_id,
        "target_path": target_path})
}

fn c4_request_id(scoped: &ScopedC4Context) -> String {
    format!(
        "{}:{}",
        crate::c4_report::report_id(&scoped.target.semantic_element_id),
        scoped.fingerprint
    )
}

struct ScopedC4Context {
    requested: SemanticElement,
    target: SemanticElement,
    elements: Vec<SemanticElement>,
    relationships: Vec<semantic_context::SemanticRelationship>,
    artifacts: Vec<KnowledgeArtifact>,
    fingerprint: String,
}

fn scoped_c4_context(
    request: &C4Request,
    context: &mut PluginContext<'_>,
) -> Result<ScopedC4Context, PluginError> {
    let snapshot = semantic_context::load_projection_snapshot(context, &request.project_root)?;
    let semantic = snapshot.semantic;
    let requested = requested_target(&semantic.elements, request)?;
    let target = report_target(&semantic.elements, &semantic.relationships, &requested);
    let elements = scoped_elements(&semantic.elements, &semantic.relationships, &target);
    let relationships = scoped_relationships(&semantic.relationships, &elements, &target);
    let artifacts = scoped_artifacts(&snapshot.artifacts, &elements);
    let fingerprint = crate::c4_report::fingerprint(&elements, &relationships, &artifacts);
    Ok(ScopedC4Context {
        requested,
        target,
        elements,
        relationships,
        artifacts,
        fingerprint,
    })
}

fn requested_target(
    elements: &[SemanticElement],
    request: &C4Request,
) -> Result<SemanticElement, PluginError> {
    if let Some(id) = request.target_id.as_deref() {
        return elements
            .iter()
            .find(|item| item.semantic_element_id == id)
            .cloned()
            .ok_or_else(|| missing("semantic_element_not_found", id, "semantic target element"));
    }
    let path = request
        .target_path
        .as_deref()
        .unwrap_or_default()
        .trim_matches('/');
    elements
        .iter()
        .filter(|item| {
            crate::c4_text::c4_target_path(item) == path || item.path.trim_matches('/') == path
        })
        .min_by_key(|item| target_rank(&item.element_kind))
        .cloned()
        .ok_or_else(|| missing("semantic_element_not_found", path, "semantic target path"))
}

/// A "module" is frequently just a one-line `mod x;` declaration with no
/// children of its own (the real content lives in a separate file); picking
/// it directly as a report scope produced an empty, useless diagram. Such a
/// target is treated as unsuited, same as any other non-scope element kind,
/// so the existing sibling/ancestor fallback below finds real content.
fn report_target(
    elements: &[SemanticElement],
    relationships: &[semantic_context::SemanticRelationship],
    requested: &SemanticElement,
) -> SemanticElement {
    if report_scope(requested) && has_scoped_content(elements, requested, relationships) {
        return requested.clone();
    }
    elements
        .iter()
        .filter(|item| item.semantic_element_id != requested.semantic_element_id)
        .filter(|item| {
            item.path == requested.path
                && report_scope(item)
                && has_scoped_content(elements, item, relationships)
        })
        .min_by_key(|item| target_rank(&item.element_kind))
        .cloned()
        .or_else(|| ancestor_scope(elements, relationships, requested))
        .unwrap_or_else(|| requested.clone())
}

fn has_scoped_content(
    elements: &[SemanticElement],
    target: &SemanticElement,
    relationships: &[semantic_context::SemanticRelationship],
) -> bool {
    !crate::components::scoped_component_groups(elements, target, relationships).is_empty()
}

fn ancestor_scope(
    elements: &[SemanticElement],
    relationships: &[semantic_context::SemanticRelationship],
    element: &SemanticElement,
) -> Option<SemanticElement> {
    let mut parent = element.parent_element_id.as_deref();
    while let Some(id) = parent {
        let value = elements
            .iter()
            .find(|item| item.semantic_element_id == id)?;
        if report_scope(value) && has_scoped_content(elements, value, relationships) {
            return Some(value.clone());
        }
        parent = value.parent_element_id.as_deref();
    }
    None
}

fn report_scope(element: &SemanticElement) -> bool {
    matches!(
        element.element_kind.as_str(),
        "project"
            | "workspace"
            | "folder"
            | "directory"
            | "module"
            | "package"
            | "dataset"
            | "file"
    )
}

fn target_rank(kind: &str) -> u8 {
    match kind {
        "folder" | "module" => 0,
        "file" => 1,
        _ => 2,
    }
}

fn scoped_elements(
    elements: &[SemanticElement],
    relationships: &[semantic_context::SemanticRelationship],
    target: &SemanticElement,
) -> Vec<SemanticElement> {
    let roots = std::collections::BTreeSet::from([target.semantic_element_id.clone()]);
    let descendants = semantic_context::descendant_ids(elements, &roots)
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let mut values = elements
        .iter()
        .filter(|item| descendants.contains(&item.semantic_element_id) || path_scoped(item, target))
        .cloned()
        .collect::<Vec<_>>();
    let child_ids = elements
        .iter()
        .filter(|item| {
            item.parent_element_id.as_deref() == Some(target.semantic_element_id.as_str())
        })
        .map(|item| item.semantic_element_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let external_ids = crate::components::one_hop_external_ids(
        elements,
        relationships,
        &child_ids,
        &descendants,
        &target.semantic_element_id,
    );
    values.extend(
        elements
            .iter()
            .filter(|item| external_ids.contains(&item.semantic_element_id))
            .cloned(),
    );
    values.sort_by(|left, right| {
        scoped_priority(left, target, &external_ids)
            .cmp(&scoped_priority(right, target, &external_ids))
            .then(left.path.cmp(&right.path))
            .then(left.semantic_element_id.cmp(&right.semantic_element_id))
    });
    values.dedup_by(|left, right| left.semantic_element_id == right.semantic_element_id);
    values.truncate(MAX_C4_ELEMENTS);
    values
}

#[cfg(test)]
mod scheduling_tests;
#[cfg(test)]
mod scope_tests;

fn path_scoped(element: &SemanticElement, target: &SemanticElement) -> bool {
    match target.element_kind.as_str() {
        "folder" | "module" => {
            let parent = target.path.trim_end_matches('/');
            element.path == parent || element.path.starts_with(&format!("{parent}/"))
        }
        "file" => element.path == target.path,
        _ => false,
    }
}

fn scoped_priority(
    element: &SemanticElement,
    target: &SemanticElement,
    external_ids: &std::collections::BTreeSet<String>,
) -> u8 {
    if element.semantic_element_id == target.semantic_element_id {
        0
    } else if element.parent_element_id.as_deref() == Some(&target.semantic_element_id)
        || external_ids.contains(&element.semantic_element_id)
    {
        1
    } else if matches!(element.element_kind.as_str(), "folder" | "module" | "file") {
        2
    } else {
        3
    }
}

/// Relationships the report may draw. Endpoint membership alone is too
/// narrow: `scoped_elements` carries a bounded budget that ranks functions
/// last, so the call endpoints are usually truncated away — filtering on the
/// surviving element ids would drop every call edge the diagram exists to
/// show. An endpoint whose own path lies inside the target scope is kept
/// regardless, and the component rollup maps it back to its owning file.
fn scoped_relationships(
    relationships: &[semantic_context::SemanticRelationship],
    elements: &[SemanticElement],
    target: &SemanticElement,
) -> Vec<semantic_context::SemanticRelationship> {
    let ids = elements
        .iter()
        .map(|item| item.semantic_element_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let in_scope_path = |id: &str| -> bool {
        crate::components::element_path_from_id(id).is_some_and(|path| {
            match target.element_kind.as_str() {
                "folder" | "module" => {
                    let parent = target.path.trim_end_matches('/');
                    path == parent || path.starts_with(&format!("{parent}/"))
                }
                "file" => path == target.path,
                _ => false,
            }
        })
    };
    relationships
        .iter()
        .filter(|item| {
            ids.contains(item.source_element_id.as_str())
                || ids.contains(item.target_element_id.as_str())
                || in_scope_path(&item.source_element_id)
                || in_scope_path(&item.target_element_id)
        })
        .cloned()
        .collect()
}

fn scoped_artifacts(
    project_artifacts: &[KnowledgeArtifact],
    elements: &[SemanticElement],
) -> Vec<KnowledgeArtifact> {
    let fingerprints = elements
        .iter()
        .map(|item| {
            (
                item.semantic_element_id.as_str(),
                item.content_fingerprint.as_deref(),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    project_artifacts
        .iter()
        .filter(|item| {
            fingerprints
                .get(item.semantic_element_id.as_str())
                .is_some_and(|fingerprint| {
                    item.metadata["provenance"]["content_fingerprint"].as_str() == *fingerprint
                })
        })
        .filter(|item| item.tags.iter().any(|tag| tag == "cultivation-functional"))
        .cloned()
        .collect()
}

fn element_snapshot(element: &SemanticElement) -> Value {
    json!({"semanticElementId": element.semantic_element_id, "elementKind": element.element_kind,
        "name": element.name, "path": element.path, "parentElementId": element.parent_element_id})
}

fn element_ids(elements: &[SemanticElement]) -> Vec<String> {
    elements
        .iter()
        .map(|item| item.semantic_element_id.clone())
        .collect()
}

fn validate_cultivation(request: &CultivationRequest) -> Result<(), PluginError> {
    artifact::require(&request.project_root, "project_root")?;
    if !VALID_MODES.contains(&request.mode.as_str()) {
        return Err(PluginError::new(
            "invalid_knowledge_input",
            format!(
                "invalid cultivation mode `{}`; expected one of {VALID_MODES:?}",
                request.mode
            ),
            false,
        ));
    }
    if request.mode == "deepen_abstraction"
        && request.target_id.as_deref().is_none_or(str::is_empty)
    {
        return Err(missing(
            "invalid_knowledge_input",
            "target_id",
            "semantic element id to deepen",
        ));
    }
    Ok(())
}

fn materialize_functional(
    request: &CultivationRequest,
    semantic: &SemanticContext,
    context: &mut PluginContext<'_>,
) -> Result<Vec<String>, PluginError> {
    let mut elements = semantic
        .elements
        .iter()
        .filter(|item| item.lifecycle == "active" && !ignored_path(&item.path))
        .collect::<Vec<_>>();
    let parents = parent_index(&semantic.elements);
    elements.sort_by_key(|item| std::cmp::Reverse(element_depth(item, &parents)));
    let existing = storage::list_project_knowledge(context, &request.project_root)?
        .into_iter()
        .map(|artifact| (artifact.artifact_id.clone(), artifact))
        .collect::<std::collections::HashMap<_, _>>();
    let touching = touching_index(&semantic.relationships);
    let mut built = std::collections::BTreeMap::<String, (KnowledgeArtifact, bool)>::new();
    let mut ids = Vec::new();
    for element in elements {
        let children = child_artifacts(element, &semantic.elements, &built);
        // A composite derives from its children, so it may only be reused when
        // every child was reused unchanged; otherwise it is rebuilt from the
        // fresh children.
        let children_fresh = children.iter().all(|(_, reused)| *reused);
        let children = children
            .into_iter()
            .map(|(artifact, _)| artifact)
            .collect::<Vec<_>>();
        let artifact_id = functional::artifact_id_for(element);
        let reused = if children_fresh {
            functional::reusable_existing(
                &request.mode,
                element,
                touching
                    .get(element.semantic_element_id.as_str())
                    .copied()
                    .unwrap_or(0),
                existing.get(&artifact_id),
            )
        } else {
            None
        };
        let (artifact, was_reused) = match reused {
            Some(artifact) => (artifact, true),
            None => {
                let artifact =
                    materialized_artifact(request, semantic, element, &children, context);
                if !functional::is_usable(&artifact) {
                    continue;
                }
                storage::put_knowledge(context, &artifact)?;
                (artifact, false)
            }
        };
        ids.push(artifact_id);
        built.insert(element.semantic_element_id.clone(), (artifact, was_reused));
    }
    ids.sort();
    Ok(ids)
}

fn materialized_artifact(
    request: &CultivationRequest,
    semantic: &SemanticContext,
    element: &SemanticElement,
    children: &[KnowledgeArtifact],
    context: &mut PluginContext<'_>,
) -> KnowledgeArtifact {
    if children.is_empty() {
        return functional::leaf_artifact(
            &request.mode,
            element,
            &semantic.relationships,
            &semantic.artifacts,
            context,
        );
    }
    functional::composite_artifact(&request.mode, element, &semantic.relationships, children)
}
fn child_artifacts(
    parent: &SemanticElement,
    elements: &[SemanticElement],
    built: &std::collections::BTreeMap<String, (KnowledgeArtifact, bool)>,
) -> Vec<(KnowledgeArtifact, bool)> {
    let mut values = elements
        .iter()
        .filter(|item| item.parent_element_id.as_deref() == Some(&parent.semantic_element_id))
        .filter_map(|item| built.get(&item.semantic_element_id).cloned())
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.semantic_element_id.cmp(&right.0.semantic_element_id));
    values
}

type ParentIndex<'a> = std::collections::HashMap<&'a str, Option<&'a str>>;

fn parent_index(elements: &[SemanticElement]) -> ParentIndex<'_> {
    elements
        .iter()
        .map(|item| {
            (
                item.semantic_element_id.as_str(),
                item.parent_element_id.as_deref(),
            )
        })
        .collect()
}

fn element_depth(element: &SemanticElement, parents: &ParentIndex<'_>) -> usize {
    let mut current = element.parent_element_id.as_deref();
    let mut depth = 0;
    while let Some(parent) = current {
        depth += 1;
        current = parents.get(parent).copied().flatten();
    }
    depth
}

/// Touching-relationship counts for every endpoint, computed in one pass
/// instead of rescanning the relationship list per element.
fn touching_index(
    relationships: &[semantic_context::SemanticRelationship],
) -> std::collections::HashMap<&str, usize> {
    let mut counts = std::collections::HashMap::new();
    for relationship in relationships {
        *counts
            .entry(relationship.source_element_id.as_str())
            .or_insert(0) += 1;
        *counts
            .entry(relationship.target_element_id.as_str())
            .or_insert(0) += 1;
    }
    counts
}
fn ignored_path(path: &str) -> bool {
    let path = path.trim_start_matches("./");
    [
        ".git/",
        ".lumvise/",
        ".fastembed_cache/",
        "target/",
        "node_modules/",
        "dist/",
        "build/",
        ".next/",
        "coverage/",
        "vendor/",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}

fn run_value(request: &CultivationRequest, artifact_ids: Vec<String>) -> Value {
    json!({
        "run_id": format!("{}-{}", request.mode, safe_id(&request.project_root)),
        "mode": request.mode, "project_root": request.project_root,
        "artifact_ids": artifact_ids, "abstraction_ids": [], "c4_artifact_ids": [],
        "nucleus_ids": [], "report_ids": [],
        "diagnostics_created": request.mode == "diagnose_on_request",
        "updated_at": Utc::now().to_rfc3339()
    })
}

fn safe_id(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_ascii_lowercase()
}

fn missing(code: &str, value: &str, expected: &str) -> PluginError {
    PluginError::new(
        code,
        format!("missing `{value}`; expected {expected}"),
        false,
    )
}

#[cfg(test)]
mod report_target_tests {
    use super::*;
    use serde_json::json;

    fn element(id: &str, kind: &str, parent: Option<&str>) -> SemanticElement {
        SemanticElement {
            project_root: "/repo".into(),
            semantic_element_id: id.into(),
            semantic_source_id: "source".into(),
            path: "crates/x/mod.rs".into(),
            element_kind: kind.into(),
            name: id.into(),
            parent_element_id: parent.map(str::to_owned),
            content_fingerprint: None,
            start_line: Some(1),
            end_line: Some(1),
            lifecycle: "active".into(),
            metadata: json!({}),
        }
    }

    #[test]
    fn empty_module_declaration_redirects_to_its_owning_file() {
        // `mod adapter;` and `mod capabilities;` are one-line re-export
        // declarations with no children of their own; only the containing
        // file actually owns real content.
        let elements = vec![
            element("file:mod.rs", "file", None),
            element("module:adapter", "module", Some("file:mod.rs")),
            element("module:capabilities", "module", Some("file:mod.rs")),
        ];
        let requested = elements[1].clone();
        let target = report_target(&elements, &[], &requested);
        assert_eq!(target.semantic_element_id, "file:mod.rs");
    }

    #[test]
    fn module_with_real_children_is_kept_as_its_own_scope() {
        let elements = vec![
            element("file:mod.rs", "file", None),
            element("module:adapter", "module", Some("file:mod.rs")),
            element("fn:adapter::run", "function", Some("module:adapter")),
        ];
        let requested = elements[1].clone();
        let target = report_target(&elements, &[], &requested);
        assert_eq!(target.semantic_element_id, "module:adapter");
    }
}

#[cfg(test)]
mod c4_response_tests {
    use super::*;

    fn element(id: &str) -> SemanticElement {
        SemanticElement {
            project_root: "/project".into(),
            semantic_element_id: id.into(),
            semantic_source_id: "src-1".into(),
            path: format!("src/{id}.rs"),
            element_kind: "function".into(),
            name: id.into(),
            parent_element_id: None,
            content_fingerprint: Some("fp-1".into()),
            start_line: Some(1),
            end_line: Some(10),
            lifecycle: "active".into(),
            metadata: json!({}),
        }
    }

    fn failure(id: &str) -> artifact_generation::ArtifactGenerationFailure {
        artifact_generation::ArtifactGenerationFailure {
            project_root: "/project".into(),
            semantic_element_id: id.into(),
            content_fingerprint: "fp-1".into(),
            reason_code: "generation_failed".into(),
            reason: format!("LLM rejected {id}"),
        }
    }

    fn respond(ids: &[&str], missing: &[&str], pending: &[&str], failed: &[&str]) -> Value {
        let elements: Vec<_> = ids.iter().map(|id| element(id)).collect();
        let summary = crate::functional::FunctionalSummary {
            job: "Resolved behavior".into(),
            source_interface: "fn resolved()".into(),
            receives: vec![],
            outcome: "Returns a value".into(),
            effects: vec![],
        };
        let artifacts = elements
            .iter()
            .filter(|element| !missing.contains(&element.semantic_element_id.as_str()))
            .map(|element| crate::functional::generated_artifact(element, &[], &summary))
            .collect();
        let scoped = ScopedC4Context {
            requested: elements[0].clone(),
            target: elements[0].clone(),
            elements: elements.clone(),
            relationships: Vec::new(),
            artifacts,
            fingerprint: "fingerprint".into(),
        };
        let artifact =
            crate::c4_report::report("/project", &elements[0], &elements, &[], &[], "fingerprint");
        let request: C4Request =
            serde_json::from_value(json!({"project_root": "/project", "target_path": "src"}))
                .unwrap();
        c4_response(
            &request,
            &scoped,
            &scoped.elements,
            artifact,
            false,
            artifact_generation::ArtifactGenerationState {
                pending_element_ids: pending.iter().map(|id| id.to_string()).collect(),
                failures: failed.iter().map(|id| failure(id)).collect(),
            },
            artifact_generation::ArtifactSubmission {
                submitted: 0,
                provider_unavailable: false,
            },
        )
    }

    #[test]
    fn one_failed_element_does_not_block_the_scope_while_others_generate() {
        let response = respond(&["a", "b", "c"], &["b", "c"], &["c"], &["b"]);
        assert_eq!(response["status"], "pending");
        assert_eq!(response["failed_element_ids"], json!(["b"]));
    }

    #[test]
    fn a_scope_with_only_failures_left_is_ready_but_partial() {
        let response = respond(&["a", "b", "c"], &["b"], &[], &["b"]);
        assert_eq!(response["status"], "ready");
        assert_eq!(response["failed_element_ids"], json!(["b"]));
        assert_eq!(response["failure_reason"], "LLM rejected b");
    }

    #[test]
    fn a_scope_where_nothing_generated_reports_the_failure() {
        let response = respond(&["a", "b"], &["a", "b"], &[], &["a", "b"]);
        assert_eq!(response["status"], "not_ready");
        assert_eq!(response["reason_code"], "generation_failed");
    }
}
