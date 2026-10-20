//! C4 refresh owns derivation and intent; Knowledge documents own every report body.

use super::*;
use lumvise_contracts::{KnowledgeDocumentKind, KnowledgeDocumentReference};
use std::collections::{BTreeMap, BTreeSet};

const C4_REPORT_INTENTS: &str = "knowledge_c4_report_intents";

#[derive(Deserialize, Serialize)]
struct C4ReportIntent {
    project_root: String,
    target_id: String,
    #[serde(default)]
    document_id: Option<String>,
    #[serde(default)]
    failure: Option<ReportRefreshFailure>,
}

#[derive(Clone, Deserialize, Serialize)]
struct ReportRefreshFailure {
    reason_code: String,
    reason: String,
}

struct PublishedReport {
    document_id: String,
    document: Option<KnowledgeDocument>,
    created: bool,
    failure: Option<ReportRefreshFailure>,
}

/// Ensure one scoped C4 document and report pending work or a preserved-content hold.
/// Example: ensure_c4({project_root, target_id}, context) returns the canonical document identity.
pub(crate) fn ensure_c4(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request = parse_c4_request(input)?;
    artifact_generation::poll(context, Some(&request.project_root))?;
    let scoped = scoped_c4_context(&request, context)?;
    let (elements, submission) = submit_scoped_summaries(
        &scoped,
        request.refresh.unwrap_or(false) || request.retry_failed.unwrap_or(false),
        artifact_generation::SubmitPriority::Interactive,
        context,
    )?;
    let state = artifact_generation::state_for(&elements, context)?;
    let result = publish_report(&scoped, request.refresh.unwrap_or(false), context)?;
    remember_intent(
        &request.project_root,
        &scoped.target.semantic_element_id,
        &result,
        context,
    )?;
    if result.failure.is_some() {
        return Ok(held_response(result));
    }
    Ok(c4_response(
        &request,
        &scoped,
        &elements,
        result.document.expect("published report"),
        result.created,
        state,
        submission,
    ))
}

fn parse_c4_request(input: Value) -> Result<C4Request, PluginError> {
    let request: C4Request = crate::parse(input, "scoped C4 document request")?;
    artifact::require(&request.project_root, "project_root")?;
    if request.target_id.is_none() && request.target_path.is_none() {
        return Err(missing(
            "invalid_knowledge_input",
            "target_id,target_path",
            "one scoped C4 target",
        ));
    }
    Ok(request)
}

fn submit_scoped_summaries(
    scoped: &ScopedC4Context,
    retry: bool,
    priority: artifact_generation::SubmitPriority,
    context: &mut PluginContext<'_>,
) -> Result<
    (
        Vec<SemanticElement>,
        artifact_generation::ArtifactSubmission,
    ),
    PluginError,
> {
    let elements = crate::c4_report::functional_summary_elements(
        &scoped.elements,
        &scoped.target,
        &scoped.relationships,
    );
    let submission = artifact_generation::submit_missing(
        &elements,
        &scoped.documents,
        retry,
        priority,
        context,
    )?;
    Ok((elements, submission))
}

/// Read source scope, canonical document identities and the last held refresh without mutation.
/// Example: debug_c4({project_root, target_id}, context) explains why a report awaits review.
pub(crate) fn debug_c4(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request = parse_c4_request(input)?;
    let scoped = scoped_c4_context(&request, context)?;
    let document_id = scoped
        .report
        .as_ref()
        .map(|document| document.document_id.clone())
        .unwrap_or_else(|| crate::c4_report::report_id(&scoped.target.semantic_element_id));
    let intent = storage::get::<C4ReportIntent>(
        context,
        C4_REPORT_INTENTS,
        &intent_key(&request.project_root, &scoped.target.semantic_element_id),
    )?;
    Ok(json!({"status":"ok", "projectRoot":request.project_root,
        "requestedTarget":element_snapshot(&scoped.requested), "reportTarget":element_snapshot(&scoped.target),
        "scope":{"descendantElementCount":scoped.elements.len(), "scopedElementCount":scoped.elements.len(),
            "descendantElementIds":element_ids(&scoped.elements), "scopedElementIds":element_ids(&scoped.elements)},
        "functionalDocuments":scoped.documents.iter().map(|document| json!({"documentId":document.document_id,
            "sourceElementId":functional::source_element_id(document), "job":document.metadata["job"],
            "jobStatus":document.metadata["job_status"]})).collect::<Vec<_>>(),
        "report":{"documentId":document_id,"exists":scoped.report.is_some(),"document":scoped.report,
            "failure":intent.and_then(|intent| intent.failure),"dependencyFingerprint":scoped.fingerprint}}))
}

/// Forget automatic refresh after the independent report is explicitly deleted.
/// Example: forget_c4_report_intent(root, source_id, context) never deletes a document body.
pub(crate) fn forget_c4_report_intent(
    root: &str,
    target: &str,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    storage::delete(context, C4_REPORT_INTENTS, &intent_key(root, target))
}

fn remember_intent(
    root: &str,
    target: &str,
    result: &PublishedReport,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let intent = C4ReportIntent {
        project_root: root.into(),
        target_id: target.into(),
        document_id: Some(result.document_id.clone()),
        failure: result.failure.clone(),
    };
    storage::put(
        context,
        C4_REPORT_INTENTS,
        &intent_key(root, target),
        &intent,
    )
}

fn intent_key(root: &str, target: &str) -> String {
    format!("{root}\0{target}")
}

/// Reconcile retained report identity before proposing another body for this scope.
/// Example: existing_report(snapshot.documents, target_id) prefers the established generated ID.
pub(super) fn existing_report(
    documents: &[KnowledgeDocument],
    target: &str,
) -> Option<KnowledgeDocument> {
    let canonical = crate::c4_report::report_id(target);
    documents
        .iter()
        .filter(|document| report_source(document) == Some(target))
        .min_by_key(|document| {
            (
                document.document_id != canonical,
                document.document_id.as_str(),
            )
        })
        .cloned()
}

fn report_source(document: &KnowledgeDocument) -> Option<&str> {
    if document.document_kind != KnowledgeDocumentKind::Report
        || !document.tags.iter().any(|tag| tag == "scoped-c4")
    {
        return None;
    }
    document.metadata["nucleus"]["target_element_id"].as_str()
}

fn publish_report(
    scoped: &ScopedC4Context,
    force: bool,
    context: &mut PluginContext<'_>,
) -> Result<PublishedReport, PluginError> {
    let existing = scoped.report.as_ref();
    if let Some(document) = unchanged_report(scoped, force) {
        return Ok(PublishedReport {
            document_id: document.document_id.clone(),
            document: Some(document.clone()),
            created: false,
            failure: None,
        });
    }
    let proposal = report_proposal(scoped);
    let document_id = proposal.document_id.clone();
    let result = save_report(scoped, proposal, context);
    match result {
        Ok(document) => Ok(PublishedReport {
            document_id,
            document: Some(document),
            created: existing.is_none(),
            failure: None,
        }),
        Err(error) if documents::is_generation_hold(&error) => Ok(PublishedReport {
            document_id,
            document: existing.cloned(),
            created: false,
            failure: Some(ReportRefreshFailure {
                reason_code: error.code,
                reason: error.message,
            }),
        }),
        Err(error) => Err(error),
    }
}

fn unchanged_report(scoped: &ScopedC4Context, force: bool) -> Option<&KnowledgeDocument> {
    scoped.report.as_ref().filter(|document| {
        !force
            && crate::lifecycle::is_active(&document.lifecycle, Utc::now())
            && document.metadata["nucleus"]["dependency_fingerprint"] == scoped.fingerprint
    })
}

fn report_proposal(scoped: &ScopedC4Context) -> KnowledgeDocument {
    let mut proposal = crate::c4_report::report(
        &scoped.target.project_root,
        &scoped.target,
        &scoped.elements,
        &scoped.relationships,
        &scoped.documents,
        &scoped.fingerprint,
    );
    if let Some(existing) = scoped.report.as_ref() {
        proposal.document_id = existing.document_id.clone();
    }
    proposal
}

fn save_report(
    scoped: &ScopedC4Context,
    proposal: KnowledgeDocument,
    context: &mut PluginContext<'_>,
) -> Result<KnowledgeDocument, PluginError> {
    documents::save_generated(
        context,
        &documents::DocumentGenerator::C4Report.owner(&scoped.target.semantic_element_id),
        proposal,
        scoped
            .report
            .as_ref()
            .map(|document| document.lifecycle.revision),
    )
}

fn held_response(result: PublishedReport) -> Value {
    let failure = result.failure.expect("held report failure");
    json!({"status":"held", "created":false,"document_id":result.document_id,"document":result.document,
        "reason_code":failure.reason_code,"reason":failure.reason})
}

/// Route a storage batch to source-derived refresh while retaining independent bodies.
/// Example: refresh_reports_for_change_batch(batch, context) does not infer document ownership from citations.
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

/// Refresh affected C4 scopes after indexed source or Context changes.
/// Example: refresh_reports_for_changed_elements(root, changed_ids, context) retains reports when sources disappear.
pub(crate) fn refresh_reports_for_changed_elements(
    root: &str,
    changed: &[String],
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    if changed.is_empty() {
        return Ok(());
    }
    let (changed, elements) = changed_sources(root, changed, context)?;
    refresh_functional(&elements, context)?;
    let anchors = ancestor_ids(root, &changed, &elements, context)?;
    let candidates = documents::project_documents(context, root)?
        .into_iter()
        .filter(|document| affected_report(document, &anchors, &changed))
        .map(|document| (document.document_id.clone(), document))
        .collect();
    refresh_candidates(root, &anchors, candidates, context)
}

fn changed_sources(
    root: &str,
    ids: &[String],
    context: &mut PluginContext<'_>,
) -> Result<(HashSet<String>, Vec<SemanticElement>), PluginError> {
    let mut changed = ids.iter().cloned().collect::<HashSet<_>>();
    let selected = storage::elements_by_ids(context, root, &changed)?;
    let partition = documents::partition_current_elements(context, root, selected)?;
    let additional = partition
        .documents
        .iter()
        .filter_map(functional::source_element_id)
        .filter(|id| !changed.contains(*id))
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let mut elements = partition.source_elements;
    if !additional.is_empty() {
        let selected = storage::elements_by_ids(context, root, &additional)?;
        elements.extend(
            documents::partition_current_elements(context, root, selected)?.source_elements,
        );
        changed.extend(additional);
    }
    Ok((changed, elements))
}

fn affected_report(
    document: &KnowledgeDocument,
    anchors: &BTreeSet<String>,
    changed: &HashSet<String>,
) -> bool {
    let Some(target) = report_source(document) else {
        return false;
    };
    anchors.contains(target) || document.references.iter().any(|reference|
        matches!(reference, KnowledgeDocumentReference::SemanticElement {semantic_element_id} if changed.contains(semantic_element_id)))
}

fn ancestor_ids(
    root: &str,
    changed: &HashSet<String>,
    elements: &[SemanticElement],
    context: &mut PluginContext<'_>,
) -> Result<BTreeSet<String>, PluginError> {
    let mut anchors = changed.iter().cloned().collect::<BTreeSet<_>>();
    let mut frontier = elements
        .iter()
        .filter_map(|element| element.parent_element_id.clone())
        .collect::<HashSet<_>>();
    while !frontier.is_empty() {
        frontier.retain(|id| !anchors.contains(id));
        if frontier.is_empty() {
            break;
        }
        anchors.extend(frontier.iter().cloned());
        frontier = storage::elements_by_ids(context, root, &frontier)?
            .into_iter()
            .filter_map(|element| element.parent_element_id)
            .collect();
    }
    Ok(anchors)
}

fn refresh_functional(
    elements: &[SemanticElement],
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let Some(first) = elements.first() else {
        return Ok(());
    };
    let ids = elements.iter().map(functional::document_id_for).collect();
    let documents = documents::find_documents(context, &first.project_root, &ids)?;
    let existing = documents
        .iter()
        .filter_map(functional::source_element_id)
        .collect::<HashSet<_>>();
    let candidates = elements
        .iter()
        .filter(|element| existing.contains(element.semantic_element_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !candidates.is_empty() {
        artifact_generation::submit_missing(
            &candidates,
            &documents,
            false,
            artifact_generation::SubmitPriority::Background,
            context,
        )?;
    }
    Ok(())
}

fn refresh_candidates(
    root: &str,
    anchors: &BTreeSet<String>,
    candidates: BTreeMap<String, KnowledgeDocument>,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let targets = candidates
        .values()
        .filter_map(report_source)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    for document in candidates.into_values() {
        let target = report_source(&document)
            .expect("selected C4 report")
            .to_owned();
        refresh_selected_report(root, &target, Some(document), context)?;
    }
    let intents: Vec<C4ReportIntent> =
        storage::list(context, C4_REPORT_INTENTS, Some(&format!("{root}\0")))?;
    for intent in intents.into_iter().filter(|intent| {
        anchors.contains(&intent.target_id) && !targets.contains(&intent.target_id)
    }) {
        let id = intent
            .document_id
            .unwrap_or_else(|| crate::c4_report::report_id(&intent.target_id));
        let existing = documents::find_documents(context, root, &HashSet::from([id]))?
            .into_iter()
            .next();
        refresh_selected_report(root, &intent.target_id, existing, context)?;
    }
    Ok(())
}

fn refresh_selected_report(
    root: &str,
    target_id: &str,
    existing: Option<KnowledgeDocument>,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let subgraph = storage::selective_subgraph(context, root, target_id)?;
    let target = subgraph.as_ref().and_then(|graph| {
        graph.elements.iter().find(|element| {
            element.semantic_element_id == target_id && element.lifecycle == "active"
        })
    });
    let Some(target) = target.cloned() else {
        let result = PublishedReport {
            document_id: existing
                .as_ref()
                .map(|document| document.document_id.clone())
                .unwrap_or_else(|| crate::c4_report::report_id(target_id)),
            document: existing,
            created: false,
            failure: Some(ReportRefreshFailure {
                reason_code: "source_unavailable".into(),
                reason: format!(
                    "Source `{target_id}` in `{root}` is unavailable; the independent report is retained."
                ),
            }),
        };
        return remember_intent(root, target_id, &result, context);
    };
    let subgraph = subgraph.expect("active source subgraph");
    let ids = subgraph
        .elements
        .iter()
        .map(functional::document_id_for)
        .collect();
    let documents = documents::find_documents(context, root, &ids)?;
    let documents = scoped_documents(&documents, &subgraph.elements);
    let fingerprint =
        crate::c4_report::fingerprint(&subgraph.elements, &subgraph.relationships, &documents);
    let elements = crate::c4_report::functional_summary_elements(
        &subgraph.elements,
        &target,
        &subgraph.relationships,
    );
    artifact_generation::submit_missing(
        &elements,
        &documents,
        false,
        artifact_generation::SubmitPriority::Background,
        context,
    )?;
    let scoped = ScopedC4Context {
        requested: target.clone(),
        target,
        elements: subgraph.elements,
        relationships: subgraph.relationships,
        documents,
        report: existing,
        fingerprint,
    };
    let result = publish_report(&scoped, false, context)?;
    remember_intent(root, target_id, &result, context)
}
