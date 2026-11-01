//! Scope-complete challenge facts: the source in the current head, affected reports and intersecting pins.
use super::super::read::{ReadPlan, SelectedQuery};
use super::*;
use crate::revision_index::{selected_successor, source_successors};
use std::collections::BTreeSet;

pub(in crate::policy_bytes::storage) fn request(
    plan: &mut ReadPlan,
    request: &ChallengeRequest,
) -> SemanticRevisionResult<()> {
    plan.key("challenge_receipt", &request.operation_id);
    plan.key("operation", &request.operation_id);
    if plan.rows.values().any(|row| {
        is_receipt(row) || row.key.kind == "operation" && row.key.identity == request.operation_id
    }) {
        return Ok(());
    }
    plan.selection("public", false);
    plan.records([SemanticRecordRef::artifact(&request.report_id)])?;
    if let Some(element) = request.input["elementId"].as_str() {
        plan.resolve_in("working", vec![SemanticRecordRef::element(element)])?;
    }
    if let Some(changes) = request.input["changes"].as_str() {
        SelectedQuery::Records {
            records: vec![SemanticRecordRef::artifact(changes)],
        }
        .request(plan, "public")?;
        plan.index("artifact_changes", changes, Some("public"));
    }
    let artifact = report(plan, &request.report_id)?;
    if let Some(artifact) = artifact {
        let head = selection(logical(&artifact)?)?;
        plan.selection(&head, true);
        plan.resolve_in(
            "working",
            vec![SemanticRecordRef::element(&artifact.semantic_element_id)],
        )?;
        // Approving a replacement takes the replaced report off the list, so its membership is needed.
        if let Some(changes) = artifact.metadata.get("changes").and_then(Value::as_str) {
            SelectedQuery::Records {
                records: vec![SemanticRecordRef::artifact(changes)],
            }
            .request(plan, "public")?;
        }
    } else if matches!(request.purpose, ChallengePurpose::Contribution)
        && request.input["reportId"].is_null()
    {
        plan.selection(&selection(&request.report_id)?, true);
    }
    Ok(())
}

fn report(plan: &ReadPlan, id: &str) -> SemanticRevisionResult<Option<SemanticArtifact>> {
    let key = serde_json::to_string(&SemanticRecordRef::artifact(id))?;
    let row = plan
        .rows
        .values()
        .find(|row| row.key.kind == "record" && row.key.identity == key);
    let body = row
        .and_then(|row| row.body_json.as_deref())
        .map(content::decode)
        .transpose()?;
    Ok(body.and_then(|body| match body.record {
        SemanticRevisionBody::Artifact(artifact) => Some(artifact),
        _ => None,
    }))
}

pub(super) fn project(
    request: &ChallengeRequest,
    state: &SemanticRevisionState,
    rows: &[StorageRow],
) -> SemanticRevisionResult<StorageResult> {
    let owner = source(request, state)?;
    let public = state
        .selection("public")
        .ok_or_else(|| invalid("public", "an existing public selection"))?;
    let owner_rejection = request.owner_override
        && matches!(request.purpose, ChallengePurpose::Vote)
        && request.input["approve"] == false;
    if !owner_rejection && !is_current_element(state, &owner)? {
        return Err(invalid(
            owner,
            "an active element of the project's current head",
        ));
    }
    validate_current_report(request, state)?;
    let records = state.records.iter().filter(|stored| {
        stored.record.kind == SemanticRecordKind::Artifact
            && (stored.record.record_id == request.report_id
                || request.input["changes"].as_str() == Some(&stored.record.record_id))
    });
    let mut nodes = views::artifact_nodes(records, rows)?;
    let revisions = nodes
        .iter()
        .filter_map(|node| {
            node["metadata"]["lumviseRevisionToken"]
                .as_str()
                .map(|token| {
                    (
                        node["id"].as_str().expect("node identity").to_owned(),
                        token.to_owned(),
                    )
                })
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for stored in &state.records {
        if stored.record == SemanticRecordRef::element(&owner) {
            if let SemanticRevisionBody::Element(element) = &stored.body {
                nodes.push(node(
                    "element",
                    &stored.record.record_id,
                    &element.name,
                    &json!(element),
                    None,
                )?);
            }
        }
    }
    let target_replaced = state.records.iter().any(|stored| matches!(&stored.body,SemanticRevisionBody::Artifact(artifact) if artifact.metadata.get("changes").and_then(Value::as_str) == request.input["changes"].as_str() && request.input["changes"].is_string() && public.records.contains(&stored.record)));
    encoded(
        "challenge_context",
        &json!({"nodes":nodes,"artifactCount":null,"targetReplaced":target_replaced,"revisions":revisions,"source_element_id":owner}),
    )
}

/// The public list holds approved reports only, so a source is judged by the project's `working` head: the
/// revision family of the named element must have reached an active element there. The named revision itself
/// stays the report's anchor, even after the head has moved on to a successor.
fn is_current_element(state: &SemanticRevisionState, id: &str) -> SemanticRevisionResult<bool> {
    let Some(head) = state.selection("working") else {
        return Ok(false);
    };
    let selected = head.records.iter().cloned().collect::<BTreeSet<_>>();
    let successors = source_successors(state, &head.selection.source_context);
    let Some(current) =
        selected_successor(&successors, &SemanticRecordRef::element(id), &selected)?
    else {
        return Ok(false);
    };
    Ok(state.records.iter().any(|stored| {
        stored.record == current
            && matches!(&stored.body, SemanticRevisionBody::Element(element) if element.lifecycle != "inactive")
    }))
}

fn validate_current_report(
    request: &ChallengeRequest,
    state: &SemanticRevisionState,
) -> SemanticRevisionResult<()> {
    if matches!(request.purpose, ChallengePurpose::Contribution)
        && request.input["reportId"].is_null()
    {
        return Ok(());
    }
    let reference = SemanticRecordRef::artifact(&request.report_id);
    let artifact = state
        .records
        .iter()
        .find_map(|stored| match &stored.body {
            SemanticRevisionBody::Artifact(artifact) if stored.record == reference => {
                Some(artifact)
            }
            _ => None,
        })
        .ok_or_else(|| invalid(&request.report_id, "an available current contribution"))?;
    let head = selection(logical(artifact)?)?;
    if ["public", &head].iter().any(|id| {
        state
            .selection(id)
            .is_some_and(|head| head.records.contains(&reference))
    }) {
        return Ok(());
    }
    Err(invalid(
        &request.report_id,
        "the current public or pending report revision",
    ))
}

pub(super) fn source(
    request: &ChallengeRequest,
    state: &SemanticRevisionState,
) -> SemanticRevisionResult<String> {
    request.input["elementId"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            state.records.iter().find_map(|stored| match &stored.body {
                SemanticRevisionBody::Artifact(artifact)
                    if artifact.artifact_id == request.report_id =>
                {
                    Some(artifact.semantic_element_id.clone())
                }
                _ => None,
            })
        })
        .ok_or_else(|| {
            invalid(
                &request.report_id,
                "an existing contributed report or exact public element",
            )
        })
}

pub(super) fn node(
    kind: &str,
    id: &str,
    title: &str,
    record: &Value,
    token: Option<&str>,
) -> SemanticRevisionResult<Value> {
    use sha2::{Digest, Sha256};
    let name = if kind == "element" {
        "semantic_element"
    } else {
        "knowledge_artifact"
    };
    Ok(
        json!({"id":format!("{kind}:{:x}",Sha256::digest(id.as_bytes())),"kind":name,"title":title,"rev":0,"metadata":{"lumviseRecord":serde_json::to_string(&json!({"kind":if kind == "element" {"Element"} else {"Artifact"},"value":[record,[]]}))?,"lumviseRevisionToken":token}}),
    )
}
