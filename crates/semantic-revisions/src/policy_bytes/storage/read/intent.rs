//! Operation-specific facts use existing typed inputs; the gateway never guesses revision ancestry.
use super::*;

#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::policy_bytes::storage) enum ReadIntent {
    ChallengeView {
        element_ids: Vec<String>,
    },
    Challenge {
        request: super::super::contributions::ChallengeRequest,
    },
    ArchiveReceipt {
        identity: String,
    },
    Archive {
        selection_id: String,
    },
    BlobWrite {
        content_ref: String,
        artifact_id: String,
    },
    VectorWrite {
        records: Vec<SemanticRecordRef>,
    },
    Vectors {
        selection_id: String,
        artifact: bool,
        engine_id: String,
        model: Option<String>,
    },
    CitationAcquisition {
        request: SemanticCitationRetentionAcquire,
        authenticated_actor: String,
    },
    CitationRelease {
        request: SemanticCitationRetentionRelease,
        authenticated_actor: String,
    },
    Publication {
        request: SemanticRevisionPublication,
    },
    Selection {
        request: SemanticRevisionSelectionUpdate,
    },
    Membership {
        query: SemanticRevisionSelectionQuery,
    },
    Snapshot {
        selection_id: String,
    },
    Selected {
        selection_id: String,
        query: super::selected::SelectedQuery,
    },
    Resolve {
        record: SemanticRecordRef,
    },
    Content {
        content_ref: String,
    },
    Families {
        query: SemanticRevisionFamilyQuery,
    },
    Connections {
        query: SemanticRevisionConnectionQuery,
    },
    SelectionAt {
        selection_id: String,
    },
    SourceAnchor {
        selection_id: String,
    },
    All,
}

impl ReadIntent {
    pub fn request(&self, plan: &mut ReadPlan) -> SemanticRevisionResult<()> {
        if let Self::Challenge { request } = self {
            return super::super::contributions::context::request(plan, request);
        }
        if let Some(id) = self.operation_id() {
            plan.key("operation", id);
            plan.key("retention", id);
            if self.replay_available(plan)? {
                return Ok(());
            }
            if !["operation", "retention"].iter().all(|kind| {
                plan.completed.contains(&StorageQuery::Key {
                    key: StorageRowKey {
                        kind: (*kind).into(),
                        identity: id.into(),
                    },
                })
            }) {
                return Ok(());
            }
        }
        match self {
            Self::ChallengeView { element_ids } => {
                super::super::contributions::views::request(plan, element_ids)?
            }
            Self::Challenge { .. } => unreachable!(),
            Self::BlobWrite {
                content_ref,
                artifact_id,
            } => {
                plan.key("staged_blob", content_ref);
                plan.key("artifact_blob", content_ref);
                plan.index("content_ref", content_ref, None);
                plan.records([SemanticRecordRef::artifact(artifact_id)])?;
            }
            Self::ArchiveReceipt { identity } => plan.key("archive_receipt", identity),
            Self::Archive { selection_id } => {
                if selection_id == "public" {
                    plan.selection(selection_id, true);
                } else {
                    plan.requested.insert(StorageQuery::All);
                }
            }
            Self::VectorWrite { records } => {
                super::selected::SelectedQuery::Records {
                    records: records.clone(),
                }
                .request(plan, "working")?;
                for record in records {
                    let key = super::super::vectors::key(record)?;
                    plan.key(&key.kind, &key.identity);
                }
            }
            Self::Vectors {
                selection_id,
                artifact,
                engine_id,
                model,
            } => {
                plan.selection(selection_id, false);
                plan.index(
                    if *artifact {
                        "artifact_vector"
                    } else {
                        "element_vector"
                    },
                    &super::super::vectors::engine_key(engine_id, model.as_deref())?,
                    Some(selection_id),
                );
                for row in plan.rows.values().cloned().collect::<Vec<_>>() {
                    if super::super::vectors::is_vector(&row) {
                        plan.membership(
                            selection_id,
                            SemanticRevisionMember::Record(super::super::vectors::owner(&row)?),
                        )?;
                    }
                }
            }
            Self::CitationAcquisition {
                request,
                authenticated_actor,
            } => {
                let keys = citation_retention_keys(
                    &request.project_id,
                    authenticated_actor,
                    &request.intent,
                )?;
                if citation_receipts(plan, &keys) && !self.replay_available(plan)? {
                    plan.selection(&keys.selection_id, true);
                    plan.records(request.records.clone())?;
                }
            }
            Self::CitationRelease {
                request,
                authenticated_actor,
            } => {
                let keys = citation_retention_keys(
                    &request.project_id,
                    authenticated_actor,
                    &request.intent,
                )?;
                if citation_receipts(plan, &keys) && !self.replay_available(plan)? {
                    plan.selection(&keys.selection_id, true);
                }
            }
            Self::Publication { request } => publication(plan, request)?,
            Self::Selection { request } => selection(plan, request)?,
            Self::Membership { query } => membership(plan, query)?,
            Self::Snapshot { selection_id } => plan.selection(selection_id, true),
            Self::Selected {
                selection_id,
                query,
            } => query.request(plan, selection_id)?,
            Self::Resolve { record } => {
                plan.records([record.clone()])?;
                plan.index("link_record", &serde_json::to_string(record)?, None);
            }
            Self::Content { content_ref } => {
                plan.key("staged_blob", content_ref);
                plan.key("artifact_blob", content_ref);
                plan.index("content_ref", content_ref, None);
                if let Some(row) = plan.rows.values().find(|row| {
                    super::super::content::attachments::is_attachment(row)
                        && &row.key.identity == content_ref
                }) {
                    let owner = super::super::content::attachments::decode(row)?.artifact_id;
                    plan.records([SemanticRecordRef::artifact(owner)])?;
                }
            }
            Self::Families { query } => families(plan, query)?,
            Self::Connections { query } => {
                for id in &query.connection_ids {
                    plan.key("connection", id);
                }
            }
            Self::SelectionAt { selection_id } | Self::SourceAnchor { selection_id } => {
                plan.selection(selection_id, false);
                plan.index("history_selection", selection_id, None);
            }
            Self::All => {
                plan.requested.insert(StorageQuery::All);
            }
        }
        Ok(())
    }

    fn operation_id(&self) -> Option<&str> {
        match self {
            Self::Publication { request } => Some(&request.operation_id),
            Self::Selection { request } => Some(&request.operation_id),
            _ => None,
        }
    }

    pub fn replay_available(&self, plan: &ReadPlan) -> SemanticRevisionResult<bool> {
        if let Self::Challenge { request } = self {
            return Ok(has_receipt(plan, &request.operation_id)
                || plan.rows.values().any(|row| {
                    super::super::contributions::is_receipt(row)
                        && row.key.identity == request.operation_id
                }));
        }
        if let Self::CitationAcquisition {
            request,
            authenticated_actor,
        } = self
        {
            let keys =
                citation_retention_keys(&request.project_id, authenticated_actor, &request.intent)?;
            return Ok(
                has_receipt(plan, &keys.acquisition_id) || has_receipt(plan, &keys.release_id)
            );
        }
        if let Self::CitationRelease {
            request,
            authenticated_actor,
        } = self
        {
            let keys =
                citation_retention_keys(&request.project_id, authenticated_actor, &request.intent)?;
            return Ok(
                !has_receipt(plan, &keys.acquisition_id) || has_receipt(plan, &keys.release_id)
            );
        }
        Ok(self.operation_id().is_some_and(|id| {
            plan.metadata.iter().any(|row| match row {
                SemanticRevisionMetadata::Operation(operation) => {
                    operation.receipt.operation_id == id
                }
                SemanticRevisionMetadata::Retention(operation) => {
                    operation.receipt.operation_id == id
                }
                _ => false,
            })
        }))
    }
}

fn citation_receipts(plan: &mut ReadPlan, keys: &SemanticCitationRetentionKeys) -> bool {
    plan.selection(&keys.selection_id, false);
    let ids = [&keys.acquisition_id, &keys.release_id];
    for id in ids {
        plan.key("operation", id);
    }
    ids.iter().all(|id| {
        plan.completed.contains(&StorageQuery::Key {
            key: StorageRowKey {
                kind: "operation".into(),
                identity: (*id).clone(),
            },
        })
    })
}

fn has_receipt(plan: &ReadPlan, id: &str) -> bool {
    plan.metadata.iter().any(|row| matches!(row, SemanticRevisionMetadata::Operation(operation) if operation.receipt.operation_id == id))
}

fn publication(
    plan: &mut ReadPlan,
    request: &SemanticRevisionPublication,
) -> SemanticRevisionResult<()> {
    for write in &request.artifacts {
        plan.index("staged_blob_owner", &write.artifact.artifact_id, None);
        if let Some(reference) = &write.artifact.content_ref {
            plan.key("staged_blob", reference);
            plan.key("artifact_blob", reference);
            plan.index("content_ref", reference, None);
        }
    }
    plan.selection(&request.selection_id, false);
    plan.index("follow_context", &request.source_context, None);
    if let Some(base) = &request.producer_base {
        plan.selection(&base.selection_id, false);
    }
    plan.coverage(&request.selection_id, &structure_coverage(request));
    let mut families = requested_artifact_ids(request)
        .iter()
        .map(SemanticRecordRef::artifact)
        .collect::<Vec<_>>();
    if let Some(structure) = &request.structure {
        families.extend(
            structure
                .elements
                .iter()
                .map(|element| SemanticRecordRef::element(&element.semantic_element_id)),
        );
        plan.records(
            structure
                .elements
                .iter()
                .flat_map(SemanticElement::required_revisions),
        )?;
        plan.records(structure.relationships.iter().flat_map(|edge| {
            [
                SemanticRecordRef::element(&edge.source_element_id),
                SemanticRecordRef::element(&edge.target_element_id),
            ]
        }))?;
    }
    families.extend(
        request
            .artifacts
            .iter()
            .map(|write| SemanticRecordRef::element(&write.artifact.semantic_element_id)),
    );
    plan.families(&request.source_context, &request.selection_id, families)?;
    plan.records(
        request
            .artifacts
            .iter()
            .flat_map(|write| write.artifact.revision_dependencies()),
    )?;
    plan.records(request.expected.records.clone())?;
    for id in &request.expected.connection_ids {
        plan.key("connection", id);
    }
    plan.owned_connections(&request.selection_id, &structure_coverage(request))
}

fn selection(
    plan: &mut ReadPlan,
    request: &SemanticRevisionSelectionUpdate,
) -> SemanticRevisionResult<()> {
    plan.selection(&request.selection_id, true);
    plan.index("follow_context", &request.source_context, None);
    plan.records(request.records.clone())?;
    for id in &request.connection_ids {
        plan.key("connection", id);
    }
    Ok(())
}

fn membership(
    plan: &mut ReadPlan,
    query: &SemanticRevisionSelectionQuery,
) -> SemanticRevisionResult<()> {
    plan.selection(&query.selection_id, false);
    plan.coverage(&query.selection_id, &query.structure_coverage);
    if let Some(context) = context(plan, &query.selection_id) {
        plan.families(
            &context,
            &query.selection_id,
            query
                .artifact_ids
                .iter()
                .map(SemanticRecordRef::artifact)
                .collect(),
        )?;
    }
    plan.owned_connections(&query.selection_id, &query.structure_coverage)
}

fn families(
    plan: &mut ReadPlan,
    query: &SemanticRevisionFamilyQuery,
) -> SemanticRevisionResult<()> {
    plan.selection(&query.selection_id, false);
    if let Some(context) = context(plan, &query.selection_id) {
        plan.families(&context, &query.selection_id, query.records.clone())?;
    }
    Ok(())
}

fn context(plan: &ReadPlan, selection: &str) -> Option<String> {
    plan.metadata.iter().find_map(|row| match row {
        SemanticRevisionMetadata::Selection(head) if head.selection_id == selection => {
            Some(head.source_context.clone())
        }
        _ => None,
    })
}
