//! Releases only claims whose exact destination locator and payload are both pruned.
//! A pending/conflicted destination remains visibly retained until its original write is resolved.
use super::*;
use lumvise_contracts::KnowledgeDocumentWriteReceiptRequest;
use lumvise_semantic_revisions::{
    SemanticCitationRetentionRelease, SemanticRevisionBody, SemanticRevisionPublicationOutcome,
};

pub(super) fn invoke(
    export: &str,
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: KnowledgeDocumentWriteReceiptRequest =
        crate::parse(input, "a project-scoped document citation receipt")?;
    let publication_id = requested_operation_id(
        receipts::ReceiptTarget::Document.action_name(request.action),
        &request.operation_id,
    )?;
    let Some(mut saved) =
        crate::storage::get::<KnowledgeWriteRecord>(context, TABLE, &publication_id)?
    else {
        return Ok(json!({"publication_id":publication_id, "state":"not_found", "claims":[]}));
    };
    require_scope(context, &request.project_root, &publication_id, &saved)?;
    if export == "reconcile_document_citations" {
        saved = reconcile(context, &publication_id, saved)?;
    }
    Ok(summary(&publication_id, &saved))
}

fn require_scope(
    context: &mut PluginContext<'_>,
    root: &str,
    id: &str,
    saved: &KnowledgeWriteRecord,
) -> Result<(), PluginError> {
    let scope = match saved {
        KnowledgeWriteRecord::Pending { publication, .. } => publication.scope()?,
        KnowledgeWriteRecord::Accepted { scope, .. }
        | KnowledgeWriteRecord::Conflicted { scope, .. } => scope.clone(),
        KnowledgeWriteRecord::Absent { .. } => return Ok(()),
    };
    KnowledgePublicationScope {
        project_root: root.into(),
        project_id: scope.project_id,
    }
    .validate_lineage(context, id)?;
    for claim in claims(saved) {
        let output = context.host_call("runtime.knowledge_scope", json!({"operation":"citation", "request":{
            "operation":"intent", "project_root":root, "document_id":claim.acquisition.intent.document_id, "publication_id":id}}))?;
        if output["intent"] != json!(claim.acquisition.intent) {
            return Err(invalid_frozen_publication(
                &output,
                "the original owned citation publication",
            ));
        }
    }
    Ok(())
}

fn reconcile(
    context: &mut PluginContext<'_>,
    id: &str,
    mut saved: KnowledgeWriteRecord,
) -> Result<KnowledgeWriteRecord, PluginError> {
    loop {
        let KnowledgeWriteRecord::Accepted {
            scope,
            citation_records,
            request_digest,
            citations,
            ..
        } = &saved
        else {
            return Ok(saved);
        };
        let Some(index) = citations.iter().position(|claim| claim.released.is_none()) else {
            return Ok(saved);
        };
        if !destination_pruned(context, scope, citation_records)? {
            return Ok(saved);
        }
        let journal = KnowledgePublicationJournal {
            operation_id: id.into(),
            request_digest: request_digest.clone(),
        };
        let receipt = release(context, &citations[index], scope, &saved)?;
        let mut released = saved.clone();
        if let KnowledgeWriteRecord::Accepted { citations, .. } = &mut released {
            citations[index].released = Some(receipt);
        }
        saved = match journal.replace(context, Some(&saved), &released) {
            Ok(()) => released,
            Err(error) => journal.read(context)?.ok_or(error)?,
        };
    }
}

fn destination_pruned(
    context: &mut PluginContext<'_>,
    scope: &KnowledgePublicationScope,
    records: &[lumvise_contracts::SemanticRecordRef],
) -> Result<bool, PluginError> {
    if records.is_empty() {
        return Ok(false);
    }
    for record in records {
        let revision = crate::storage::read_revision(context, &scope.project_root, record.clone())?;
        if revision.is_none_or(|revision| revision.body != SemanticRevisionBody::Pruned) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn release(
    context: &mut PluginContext<'_>,
    claim: &FrozenCitationClaim,
    scope: &KnowledgePublicationScope,
    saved: &KnowledgeWriteRecord,
) -> Result<lumvise_semantic_revisions::SemanticRevisionPublicationReceipt, PluginError> {
    let acquired = claim.acknowledgement.as_ref().ok_or_else(|| {
        invalid_frozen_publication(&json!(claim), "the original source acquisition receipt")
    })?;
    let request = SemanticCitationRetentionRelease {
        project_root: claim.acquisition.project_root.clone(),
        project_id: claim.acquisition.project_id.clone(),
        intent: claim.acquisition.intent.clone(),
        expected_generation: acquired.selection.generation,
    };
    let output = context.host_call("runtime.knowledge_scope", json!({"operation":"citation", "request":{
        "operation":"release", "claim":request, "observation":{"project_root":scope.project_root, "table":TABLE, "expected":saved}}}))?;
    let outcome: SemanticRevisionPublicationOutcome = crate::parse(
        output["publication"].clone(),
        "a conditional source citation release",
    )?;
    match outcome {
        SemanticRevisionPublicationOutcome::Accepted(receipt)
            if receipt.selection.selection_id == acquired.selection.selection_id
                && receipt.selection.generation == acquired.selection.generation + 1
                && receipt.accepted_scope.records.is_empty() =>
        {
            Ok(receipt)
        }
        other => Err(invalid_frozen_publication(
            &json!(other),
            "the original claim's accepted empty release receipt",
        )),
    }
}

fn claims(saved: &KnowledgeWriteRecord) -> &[FrozenCitationClaim] {
    match saved {
        KnowledgeWriteRecord::Pending { publication, .. } => publication.citations(),
        KnowledgeWriteRecord::Accepted { citations, .. }
        | KnowledgeWriteRecord::Conflicted { citations, .. } => citations,
        KnowledgeWriteRecord::Absent { .. } => &[],
    }
}

fn summary(id: &str, saved: &KnowledgeWriteRecord) -> Value {
    let state = match saved {
        KnowledgeWriteRecord::Absent { .. } => "not_found",
        KnowledgeWriteRecord::Pending { .. } => "pending",
        KnowledgeWriteRecord::Accepted { .. } => "accepted",
        KnowledgeWriteRecord::Conflicted { .. } => "conflicted",
    };
    let claims = claims(saved).iter().map(|claim| json!({"project_root":claim.acquisition.project_root,
        "state":if claim.released.is_some() { "released" } else if claim.acknowledgement.is_some() { "retained" } else { "pending" }})).collect::<Vec<_>>();
    json!({"publication_id":id, "state":state, "claims":claims})
}
