//! Applies challenge-owned patches through the existing publication, freeze and selection owners.
use super::*;
use std::collections::BTreeSet;

pub(super) fn accept(
    request: &ChallengeRequest,
    before: &SemanticRevisionState,
    rows: &[StorageRow],
    prepared: &str,
    now: &SemanticRevisionTimestamp,
) -> SemanticRevisionResult<StorageResult> {
    context::project(request, before, rows)?;
    let prepared: Value = serde_json::from_str(prepared)?;
    let artifact = artifact(request, before, &prepared)?;
    let head_id = selection(logical(&artifact)?)?;
    let mut after = before.clone();
    ensure_head(&mut after, &head_id, &artifact, &request.operation_id, now)?;
    let publication = publication(request, &after, &head_id, artifact.clone())?;
    let plan =
        crate::publication::plan_owned_publication(&after, &publication, now, next(&after)?)?;
    let receipt = apply(&mut after, plan)?;
    let accepted = receipt
        .remaps
        .iter()
        .find(|remap| remap.incoming == SemanticRecordRef::artifact(&artifact.artifact_id))
        .map(|remap| remap.accepted.clone())
        .unwrap_or_else(|| SemanticRecordRef::artifact(&artifact.artifact_id));
    publish_if_approved(request, &mut after, &accepted, &artifact, now)?;
    let outcome = outcome(request, &prepared, &after, &accepted)?;
    let mut delta = super::super::delta(
        before,
        &after,
        &SemanticRevisionPublicationOutcome::Accepted(receipt),
    )?;
    if let StorageResult::Delta { rows, .. } = &mut delta {
        rows.push(super::receipt(&request.operation_id, &outcome)?);
    }
    let mut complete = content::attach(
        &serde_json::to_string(&delta)?,
        Some(&serde_json::to_string(
            &json!({"operation":"publication","request":publication}),
        )?),
        rows,
    )?;
    if let StorageResult::Delta { outcome_json, .. } = &mut complete {
        *outcome_json = serde_json::to_string(&outcome)?;
    }
    Ok(complete)
}

fn artifact(
    request: &ChallengeRequest,
    state: &SemanticRevisionState,
    prepared: &Value,
) -> SemanticRevisionResult<SemanticArtifact> {
    let incoming = prepared["nodes"]
        .as_array()
        .and_then(|nodes| nodes.first())
        .and_then(|node| node["metadata"]["lumviseRecord"].as_str())
        .ok_or_else(|| invalid(prepared.to_string(), "one prepared challenge artifact"))?;
    let incoming: Value = serde_json::from_str(incoming)?;
    let mut proposed: SemanticArtifact = serde_json::from_value(incoming["value"][0].clone())?;
    if proposed.artifact_id != request.report_id
        || proposed.semantic_element_id != context::source(request, state)?
    {
        return Err(invalid(
            proposed.artifact_id,
            "the exact prepared report and public source",
        ));
    }
    let old = state
        .records
        .iter()
        .find(|stored| stored.record == SemanticRecordRef::artifact(&request.report_id));
    if matches!(request.purpose, ChallengePurpose::Vote) {
        let Some(SemanticRevisionBody::Artifact(original)) = old.map(|stored| &stored.body) else {
            return Err(invalid(
                &request.report_id,
                "an available original contribution",
            ));
        };
        let mut artifact = original.clone();
        for field in ["publication", "votes"] {
            artifact.metadata[field] = proposed
                .metadata
                .get(field)
                .cloned()
                .ok_or_else(|| invalid(field, "a prepared vote patch"))?;
        }
        return Ok(artifact);
    }
    if proposed
        .metadata
        .get("contributorSubjectHash")
        .and_then(Value::as_str)
        != Some(&request.author())
    {
        return Err(invalid(
            &request.report_id,
            "the authenticated contributor pseudonym",
        ));
    }
    if let Some(old) = old {
        let actual = old
            .artifact_state
            .as_ref()
            .map(|state| state.edit_version.to_string());
        if actual.as_deref() != request.input["revision"].as_str() {
            return Err(invalid(
                request.input["revision"].to_string(),
                "the exact original artifact edit token",
            ));
        }
    }
    let logical = old
        .and_then(|stored| match &stored.body {
            SemanticRevisionBody::Artifact(artifact) => {
                artifact.metadata.get("contributionId").cloned()
            }
            _ => None,
        })
        .unwrap_or_else(|| json!(request.report_id));
    proposed.metadata["contributionId"] = logical;
    Ok(proposed)
}

fn ensure_head(
    state: &mut SemanticRevisionState,
    id: &str,
    artifact: &SemanticArtifact,
    operation: &str,
    now: &SemanticRevisionTimestamp,
) -> SemanticRevisionResult<()> {
    if state
        .selection(id)
        .is_some_and(|head| !head.records.is_empty())
    {
        return Ok(());
    }
    let mut records = vec![SemanticRecordRef::element(&artifact.semantic_element_id)];
    if state
        .records
        .iter()
        .any(|row| row.record == SemanticRecordRef::artifact(&artifact.artifact_id))
    {
        records.push(SemanticRecordRef::artifact(&artifact.artifact_id));
    }
    let update = selection_update(
        state,
        id,
        &views::source_context(artifact)?,
        format!("{operation}:seed"),
        records,
        SemanticSelectionMode::FollowAccepted,
    )?;
    let plan = state.plan_owned_selection(&update, now, next(state)?)?;
    apply(state, plan).map(|_| ())
}

fn publication(
    request: &ChallengeRequest,
    state: &SemanticRevisionState,
    head: &str,
    artifact: SemanticArtifact,
) -> SemanticRevisionResult<SemanticRevisionPublication> {
    let expected = state
        .selection_members(&SemanticRevisionSelectionQuery {
            project_root: state.project_root.clone(),
            selection_id: head.into(),
            structure_coverage: SemanticRevisionStructureCoverage::None,
            artifact_ids: vec![artifact.artifact_id.clone()],
        })?
        .expect("seeded head")
        .expected;
    Ok(SemanticRevisionPublication {
        operation_id: request.operation_id.clone(),
        project_root: state.project_root.clone(),
        project_id: state.project_id.clone(),
        selection_id: head.into(),
        source_context: state
            .selection(head)
            .expect("seeded head")
            .selection
            .source_context
            .clone(),
        producer_base: None,
        expected,
        structure: None,
        artifacts: vec![SemanticArtifactRevisionWrite {
            artifact,
            media_type: "text/markdown".into(),
            checkpoint: false,
        }],
        removed_artifact_ids: vec![],
        source_anchor: None,
    })
}

fn publish_if_approved(
    request: &ChallengeRequest,
    state: &mut SemanticRevisionState,
    accepted: &SemanticRecordRef,
    artifact: &SemanticArtifact,
    now: &SemanticRevisionTimestamp,
) -> SemanticRevisionResult<()> {
    let status = artifact.metadata.get("publication").and_then(Value::as_str);
    if !matches!(status, Some("approved" | "rejected")) {
        return Ok(());
    }
    let public = state.selection("public").expect("validated public head");
    let mut records = public.records.iter().cloned().collect::<BTreeSet<_>>();
    if matches!(request.purpose, ChallengePurpose::Vote) {
        records.remove(&SemanticRecordRef::artifact(&request.report_id));
    }
    if status == Some("approved") {
        records.insert(accepted.clone());
    }
    if let Some(changes) = artifact.metadata.get("changes").and_then(Value::as_str) {
        records.remove(&SemanticRecordRef::artifact(changes));
    }
    let context = public.selection.source_context.clone();
    let update = selection_update(
        state,
        "public",
        &context,
        format!("{}:public", request.operation_id),
        records.into_iter().collect(),
        SemanticSelectionMode::Pinned,
    )?;
    let plan = state.plan_owned_selection(&update, now, next(state)?)?;
    apply(state, plan)?;
    // A decided report is retained by public membership; pending protection can be released.
    let id = selection(logical(artifact)?)?;
    let context = state
        .selection(&id)
        .expect("contribution head")
        .selection
        .source_context
        .clone();
    let release = selection_update(
        state,
        &id,
        &context,
        format!("{}:release", request.operation_id),
        vec![],
        SemanticSelectionMode::FollowAccepted,
    )?;
    let plan = state.plan_owned_selection(&release, now, next(state)?)?;
    apply(state, plan)?;
    state.snapshot("public", &now.rfc3339)?;
    Ok(())
}

fn selection_update(
    state: &SemanticRevisionState,
    id: &str,
    context: &str,
    operation_id: String,
    records: Vec<SemanticRecordRef>,
    mode: SemanticSelectionMode,
) -> SemanticRevisionResult<SemanticRevisionSelectionUpdate> {
    let head = state.selection(id);
    let mut edits = std::collections::BTreeMap::new();
    for row in &state.records {
        if records.contains(&row.record) {
            if let Some(token) = &row.artifact_state {
                edits.insert(row.record.record_id.clone(), token.edit_version);
            }
        }
    }
    Ok(SemanticRevisionSelectionUpdate {
        operation_id,
        project_root: state.project_root.clone(),
        project_id: state.project_id.clone(),
        selection_id: id.into(),
        source_context: context.into(),
        expected_generation: head.map_or(0, |head| head.selection.generation),
        artifact_edits: edits,
        mode,
        records,
        connection_ids: head.map_or_else(Vec::new, |head| head.connection_ids.clone()),
        source_anchor: head.and_then(|head| head.selection.source_anchor.clone()),
    })
}

fn apply(
    state: &mut SemanticRevisionState,
    plan: SemanticRevisionPublicationPlan,
) -> SemanticRevisionResult<SemanticRevisionPublicationReceipt> {
    match state.apply_publication(plan)? {
        SemanticRevisionPublicationOutcome::Accepted(receipt) => Ok(receipt),
        SemanticRevisionPublicationOutcome::Conflict(conflict) => Err(invalid(
            format!("{conflict:?}"),
            "unchanged challenge scope",
        )),
    }
}

fn next(state: &SemanticRevisionState) -> SemanticRevisionResult<i64> {
    state.commit_version.checked_add(1).ok_or_else(|| {
        invalid(
            state.commit_version.to_string(),
            "an incrementable challenge commit",
        )
    })
}

fn outcome(
    request: &ChallengeRequest,
    prepared: &Value,
    state: &SemanticRevisionState,
    record: &SemanticRecordRef,
) -> SemanticRevisionResult<Value> {
    if matches!(request.purpose, ChallengePurpose::Vote) {
        let mut outcome = prepared["receipt"].clone();
        outcome["reportId"] = json!(record.record_id);
        return Ok(outcome);
    }
    let token = state
        .records
        .iter()
        .find(|stored| stored.record == *record)
        .and_then(|stored| stored.artifact_state.as_ref())
        .ok_or_else(|| invalid(&record.record_id, "an accepted artifact edit token"))?;
    Ok(
        json!({"reportId":record.record_id,"revision":token.edit_version.to_string(),"status":prepared["status"]}),
    )
}
