//! Owns frozen Knowledge write intents and acknowledgement through existing relational rows.
//! Artifact and document producers call `execute`; row formats and retry routing remain internal.
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod acknowledgement;
mod citation_cleanup;
mod citations;
mod receipts;
mod schema;

use citations::FrozenCitationClaim;
pub(crate) use schema::{EXPORTS, description, input_schema, output_schema};

/// Dispatches only passive producer acknowledgements; publishing remains with `execute`.
/// Example: `invoke("get_artifact_write_receipt", input, context)` never sends a write.
pub(crate) fn invoke(
    export: &str,
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    match export {
        "get_document_citation_retention" | "reconcile_document_citations" => {
            citation_cleanup::invoke(export, input, context)
        }
        "get_document_write_receipt" => {
            let request = crate::parse(
                input,
                "a project-scoped document write acknowledgement request",
            )?;
            receipts::read_document_write_receipt(context, &request).map(|receipt| json!(receipt))
        }
        "get_artifact_write_receipt" => {
            let request = crate::parse(
                input,
                "a project-scoped artifact write acknowledgement request",
            )?;
            receipts::read_artifact_write_receipt(context, &request).map(|receipt| json!(receipt))
        }
        _ => Err(PluginError::unknown_capability(export)),
    }
}

const TABLE: &str = "knowledge_publications";

/// Frozen producer input for the existing artifact or Semantic publication boundary.
/// Example: `FrozenKnowledgePublication::ArtifactRevision(proposal)` from an artifact edit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum FrozenKnowledgePublication {
    ArtifactRevision(Value),
    DocumentBatch {
        batch: Value,
        citations: Vec<FrozenCitationClaim>,
    },
    ArtifactTransfer {
        publication: Value,
        completion: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KnowledgePublicationScope {
    project_root: String,
    project_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum KnowledgeWriteRecord {
    Absent {
        request_digest: String,
        operation_id: String,
    },
    Pending {
        request_digest: String,
        publication: FrozenKnowledgePublication,
    },
    Accepted {
        request_digest: String,
        receipt: Value,
        scope: KnowledgePublicationScope,
        #[serde(default)]
        citations: Vec<FrozenCitationClaim>,
        #[serde(default)]
        citation_records: Vec<lumvise_contracts::SemanticRecordRef>,
    },
    Conflicted {
        request_digest: String,
        operation_id: String,
        conflict: Value,
        scope: KnowledgePublicationScope,
        #[serde(default)]
        citations: Vec<FrozenCitationClaim>,
    },
}

struct KnowledgePublicationJournal {
    operation_id: String,
    request_digest: String,
}

/// Replays an existing intent before preparing mutable state, then freezes and submits it once.
/// Example: `execute(context, "create", &request, request.operation_id.as_deref(), prepare)`.
pub(crate) fn execute(
    context: &mut PluginContext<'_>,
    action: &str,
    request: &impl Serialize,
    requested_operation_id: Option<&str>,
    prepare: impl FnOnce(
        &mut PluginContext<'_>,
        &str,
    ) -> Result<Option<FrozenKnowledgePublication>, PluginError>,
) -> Result<Option<Value>, PluginError> {
    let journal = KnowledgePublicationJournal::new(action, request, requested_operation_id)?;
    let saved = match journal.read(context)? {
        Some(saved) => saved,
        None => {
            let publication = prepare(context, &journal.operation_id)?;
            journal.freeze(context, publication)?
        }
    };
    journal.publish(context, saved)
}

impl KnowledgePublicationJournal {
    fn new(
        action: &str,
        request: &impl Serialize,
        requested_id: Option<&str>,
    ) -> Result<Self, PluginError> {
        let request_digest = digest(&(action, request))?;
        let operation_id = match requested_id {
            Some(id) => requested_operation_id(action, id)?,
            None => format!("knowledge-write:{request_digest}"),
        };
        Ok(Self {
            operation_id,
            request_digest,
        })
    }

    fn read(
        &self,
        context: &mut PluginContext<'_>,
    ) -> Result<Option<KnowledgeWriteRecord>, PluginError> {
        let saved: Option<KnowledgeWriteRecord> =
            crate::storage::get(context, TABLE, &self.operation_id)?;
        if let Some(saved) = &saved {
            self.validate(saved)?;
        }
        Ok(saved)
    }

    fn validate(&self, saved: &KnowledgeWriteRecord) -> Result<(), PluginError> {
        let (request_digest, operation_id) = match saved {
            KnowledgeWriteRecord::Pending {
                request_digest,
                publication,
            } => (request_digest, publication.operation_id()),
            KnowledgeWriteRecord::Accepted {
                request_digest,
                receipt,
                ..
            } => (request_digest, receipt["operation_id"].as_str()),
            KnowledgeWriteRecord::Conflicted {
                request_digest,
                operation_id,
                ..
            } => (request_digest, Some(operation_id.as_str())),
            KnowledgeWriteRecord::Absent {
                request_digest,
                operation_id,
            } => (request_digest, Some(operation_id.as_str())),
        };
        if request_digest != &self.request_digest
            || operation_id != Some(self.operation_id.as_str())
        {
            return Err(invalid_frozen_publication(
                &json!(self.operation_id),
                "the identical frozen request for this operation ID",
            ));
        }
        Ok(())
    }

    fn freeze(
        &self,
        context: &mut PluginContext<'_>,
        publication: Option<FrozenKnowledgePublication>,
    ) -> Result<KnowledgeWriteRecord, PluginError> {
        let saved = match publication {
            Some(publication) => KnowledgeWriteRecord::Pending {
                request_digest: self.request_digest.clone(),
                publication,
            },
            None => KnowledgeWriteRecord::Absent {
                request_digest: self.request_digest.clone(),
                operation_id: self.operation_id.clone(),
            },
        };
        self.validate(&saved)?;
        if let KnowledgeWriteRecord::Pending { publication, .. } = &saved {
            publication.scope()?;
        }
        match self.replace(context, None, &saved) {
            Ok(()) => Ok(saved),
            Err(error) => self.read(context)?.ok_or(error),
        }
    }

    fn publish(
        &self,
        context: &mut PluginContext<'_>,
        saved: KnowledgeWriteRecord,
    ) -> Result<Option<Value>, PluginError> {
        let saved = self.acquire_citations(context, saved)?;
        let KnowledgeWriteRecord::Pending {
            ref publication, ..
        } = saved
        else {
            return self.result(context, saved);
        };
        let scope = publication.scope()?;
        let output = publication.submit(context)?;
        let completed = self.terminal_record(&output, scope, publication)?;
        if let Err(error) = self.replace(context, Some(&saved), &completed) {
            if self.read(context)? != Some(completed.clone()) {
                return Err(error);
            }
        }
        self.result(context, completed)
    }

    fn terminal_record(
        &self,
        output: &Value,
        scope: KnowledgePublicationScope,
        publication: &FrozenKnowledgePublication,
    ) -> Result<KnowledgeWriteRecord, PluginError> {
        let citations = publication.citations().to_vec();
        let outcome = &output["publication"];
        if outcome["outcome"] == "conflict" && outcome["value"].is_object() {
            return Ok(KnowledgeWriteRecord::Conflicted {
                request_digest: self.request_digest.clone(),
                operation_id: self.operation_id.clone(),
                conflict: outcome["value"].clone(),
                scope,
                citations,
            });
        }
        if outcome["outcome"] != "accepted" || outcome["value"]["operation_id"] != self.operation_id
        {
            return Err(invalid_frozen_publication(
                output,
                "an accepted receipt or conflict for the frozen operation identity",
            ));
        }
        Ok(KnowledgeWriteRecord::Accepted {
            request_digest: self.request_digest.clone(),
            receipt: outcome["value"].clone(),
            scope,
            citations,
            citation_records: publication.citation_records(&outcome["value"])?,
        })
    }

    fn result(
        &self,
        context: &mut PluginContext<'_>,
        saved: KnowledgeWriteRecord,
    ) -> Result<Option<Value>, PluginError> {
        match saved {
            KnowledgeWriteRecord::Accepted { receipt, scope, .. } => {
                scope.validate_lineage(context, &self.operation_id)?;
                Ok(Some(receipt))
            }
            KnowledgeWriteRecord::Absent { .. } => Ok(None),
            KnowledgeWriteRecord::Conflicted {
                conflict, scope, ..
            } => {
                scope.validate_lineage(context, &self.operation_id)?;
                Err(PluginError::new(
                    "knowledge_revision_conflict",
                    format!(
                        "Knowledge publication `{}` conflicts with `{conflict}`; expected its frozen observed revision membership",
                        self.operation_id
                    ),
                    false,
                ))
            }
            KnowledgeWriteRecord::Pending { publication, .. } => Err(invalid_frozen_publication(
                &json!(publication),
                "a terminal accepted or conflicted publication",
            )),
        }
    }

    fn replace(
        &self,
        context: &mut PluginContext<'_>,
        expected: Option<&KnowledgeWriteRecord>,
        saved: &KnowledgeWriteRecord,
    ) -> Result<(), PluginError> {
        let output = crate::storage::mutate_rows_if_unchanged(
            context,
            TABLE,
            &self.operation_id,
            expected,
            vec![json!({"operation":"put", "table_name":TABLE,
                "row_key":self.operation_id, "value":saved})],
        )?;
        if output["rows_put"] != 1 {
            return Err(invalid_frozen_publication(
                &output,
                "one conditionally committed Knowledge publication row",
            ));
        }
        Ok(())
    }
}

impl KnowledgePublicationScope {
    fn from_parts(project_root: &Value, project_id: &Value) -> Result<Self, PluginError> {
        let value = json!({"project_root":project_root, "project_id":project_id});
        let scope: Self = crate::parse(
            value.clone(),
            "the frozen publication's durable project scope",
        )?;
        if scope.project_root.trim().is_empty() || scope.project_id.trim().is_empty() {
            return Err(invalid_frozen_publication(
                &value,
                "non-empty original project root and lineage",
            ));
        }
        Ok(scope)
    }

    fn validate_lineage(
        &self,
        context: &mut PluginContext<'_>,
        operation_id: &str,
    ) -> Result<(), PluginError> {
        let current = context.host_call(
            "storage.semantic",
            json!({
                "operation":"project_identity", "project_root":self.project_root
            }),
        )?;
        if current["identity"]["project_id"] != self.project_id {
            return Err(PluginError::new(
                "knowledge_project_mismatch",
                format!(
                    "Knowledge publication `{operation_id}` belongs to lineage `{}`; project route `{}` returned `{}`; expected the original durable project identity",
                    self.project_id, self.project_root, current["identity"]["project_id"]
                ),
                false,
            ));
        }
        Ok(())
    }
}

fn digest(value: &impl Serialize) -> Result<String, PluginError> {
    let encoded = serde_json::to_vec(value).map_err(|error| {
        invalid_frozen_publication(
            &json!(error.to_string()),
            "a serializable Knowledge write intent",
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn requested_operation_id(action: &str, requested_id: &str) -> Result<String, PluginError> {
    crate::artifact::require(requested_id, "operation_id")?;
    Ok(format!(
        "knowledge-write:{}",
        digest(&(action, requested_id))?
    ))
}

impl FrozenKnowledgePublication {
    fn operation_id(&self) -> Option<&str> {
        match self {
            Self::ArtifactRevision(publication) | Self::ArtifactTransfer { publication, .. } => {
                publication["operation_id"].as_str()
            }
            Self::DocumentBatch { batch, .. } => batch["ingestion_job_id"].as_str(),
        }
    }

    fn scope(&self) -> Result<KnowledgePublicationScope, PluginError> {
        match self {
            Self::ArtifactRevision(publication) | Self::ArtifactTransfer { publication, .. } => {
                KnowledgePublicationScope::from_parts(
                    &publication["project_root"],
                    &publication["project_id"],
                )
            }
            Self::DocumentBatch { batch, .. } => KnowledgePublicationScope::from_parts(
                &batch["project_root"],
                &batch["revision_observation"]["project_id"],
            ),
        }
    }

    fn submit(&self, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
        match self {
            Self::ArtifactRevision(publication) => {
                let mut output = context.host_call(
                    "storage.semantic",
                    json!({"operation":"publish_revisions", "publication":publication}),
                )?;
                acknowledgement::capture_artifact(publication, &mut output)?;
                Ok(output)
            }
            Self::DocumentBatch { batch, .. } => {
                crate::documents::authorize_publication(context, batch)?;
                let mut output =
                    crate::semantic::invoke_public(context, "ingest_index_batch", batch.clone())?;
                if output["publication"]["outcome"] == "accepted"
                    && let Some(reference) = crate::documents::publication_reference(
                        batch,
                        &output["publication"]["value"],
                    )?
                {
                    output["publication"]["value"]["document_revision"] = json!(reference);
                    output["publication"]["value"]["lifecycle_revision"] =
                        json!(crate::documents::publication_lifecycle_revision(
                            batch,
                            &output["publication"]["value"]
                        )?);
                }
                Ok(output)
            }
            Self::ArtifactTransfer {
                publication,
                completion,
            } => {
                let mut output = context.host_call(
                    "storage.semantic",
                    json!({"operation":"publish_revisions", "publication":publication}),
                )?;
                if output["publication"]["outcome"] == "accepted" {
                    output["publication"]["value"]["transfer"] = completion.clone();
                }
                Ok(output)
            }
        }
    }
}

fn invalid_frozen_publication(value: &Value, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_knowledge_publication",
        format!("invalid Knowledge publication `{value}`; expected {expected}"),
        false,
    )
}
