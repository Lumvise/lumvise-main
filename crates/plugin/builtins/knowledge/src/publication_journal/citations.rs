//! Source acknowledgements live in the same conditional journal row as their destination.
use super::*;
use lumvise_semantic_revisions::{
    SemanticCitationRetentionAcquire, SemanticRevisionPublicationOutcome,
    SemanticRevisionPublicationReceipt,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenCitationClaim {
    pub(super) acquisition: SemanticCitationRetentionAcquire,
    pub(super) acknowledgement: Option<SemanticRevisionPublicationReceipt>,
    pub(super) released: Option<SemanticRevisionPublicationReceipt>,
}

impl FrozenKnowledgePublication {
    pub(super) fn citation_records(
        &self,
        receipt: &Value,
    ) -> Result<Vec<lumvise_contracts::SemanticRecordRef>, PluginError> {
        if self.citations().is_empty() {
            return Ok(vec![]);
        }
        let Self::DocumentBatch { batch, .. } = self else {
            unreachable!()
        };
        let locator = receipt["document_revision"]["semantic_element_id"]
            .as_str()
            .ok_or_else(|| {
                invalid_frozen_publication(receipt, "the accepted citing document locator")
            })?;
        let mut records = vec![lumvise_contracts::SemanticRecordRef::element(locator)];
        for artifact in batch["semantic_artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|artifact| artifact["artifact_kind"] == "knowledge_document")
        {
            let remap = receipt["remaps"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|remap| {
                    remap["incoming"]["kind"] == "artifact"
                        && remap["incoming"]["record_id"] == artifact["artifact_id"]
                })
                .ok_or_else(|| {
                    invalid_frozen_publication(receipt, "the exact accepted document payload")
                })?;
            records.push(crate::parse(
                remap["accepted"].clone(),
                "an accepted document payload reference",
            )?);
        }
        if records.len() != 2 {
            return Err(invalid_frozen_publication(
                receipt,
                "one exact locator and one payload for retained citations",
            ));
        }
        Ok(records)
    }
    pub(crate) fn with_citations(mut self, claims: Vec<SemanticCitationRetentionAcquire>) -> Self {
        if let Self::DocumentBatch { citations, .. } = &mut self {
            *citations = claims
                .into_iter()
                .map(|acquisition| FrozenCitationClaim {
                    acquisition,
                    acknowledgement: None,
                    released: None,
                })
                .collect();
        }
        self
    }

    pub(super) fn citations(&self) -> &[FrozenCitationClaim] {
        match self {
            Self::DocumentBatch { citations, .. } => citations,
            _ => &[],
        }
    }
}

impl KnowledgePublicationJournal {
    pub(super) fn acquire_citations(
        &self,
        context: &mut PluginContext<'_>,
        mut saved: KnowledgeWriteRecord,
    ) -> Result<KnowledgeWriteRecord, PluginError> {
        loop {
            let KnowledgeWriteRecord::Pending { publication, .. } = &saved else {
                return Ok(saved);
            };
            let Some(index) = publication
                .citations()
                .iter()
                .position(|claim| claim.acknowledgement.is_none())
            else {
                return Ok(saved);
            };
            let claim = &publication.citations()[index];
            let scope = publication.scope()?;
            let output = context.host_call("runtime.knowledge_scope", json!({"operation":"citation", "request":{
                "operation":"acquire", "claim":claim.acquisition,
                "observation":{"project_root":scope.project_root, "table":TABLE, "expected":saved}}}))?;
            let receipt = accepted_claim(&output, &claim.acquisition)?;
            let mut acknowledged = saved.clone();
            if let KnowledgeWriteRecord::Pending {
                publication: FrozenKnowledgePublication::DocumentBatch { citations, .. },
                ..
            } = &mut acknowledged
            {
                citations[index].acknowledgement = Some(receipt);
            }
            saved = match self.replace(context, Some(&saved), &acknowledged) {
                Ok(()) => acknowledged,
                Err(error) => self.read(context)?.ok_or(error)?,
            };
        }
    }
}

fn accepted_claim(
    output: &Value,
    claim: &SemanticCitationRetentionAcquire,
) -> Result<SemanticRevisionPublicationReceipt, PluginError> {
    let outcome: SemanticRevisionPublicationOutcome = crate::parse(
        output["publication"].clone(),
        "a source-owned citation acknowledgement",
    )?;
    match outcome {
        SemanticRevisionPublicationOutcome::Accepted(receipt)
            if receipt.selection.generation == 1
                && receipt.selection.source_context == claim.source_context
                && receipt.accepted_scope.records == claim.records =>
        {
            Ok(receipt)
        }
        other => Err(invalid_frozen_publication(
            &json!(other),
            "accepted retention of the original exact evidence before destination publication",
        )),
    }
}
