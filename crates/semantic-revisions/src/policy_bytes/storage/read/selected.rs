//! Current-selection reads fetch only indexed matches and their exact dependency closure.
use super::*;

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::policy_bytes::storage) enum SelectedQuery {
    Records {
        records: Vec<SemanticRecordRef>,
    },
    Path {
        path: String,
    },
    Prefix {
        prefix: String,
    },
    Owners {
        element_ids: Vec<String>,
    },
    Connections {
        element_ids: Vec<String>,
        incoming: bool,
    },
    Dependents {
        record: SemanticRecordRef,
    },
    Content {
        content_ref: String,
    },
}

impl SelectedQuery {
    pub(in crate::policy_bytes::storage) fn request(
        &self,
        plan: &mut ReadPlan,
        selection: &str,
    ) -> SemanticRevisionResult<()> {
        plan.selection(selection, false);
        match self {
            Self::Records { records } => {
                for record in records {
                    if plan.membership(selection, SemanticRevisionMember::Record(record.clone()))? {
                        plan.records([record.clone()])?;
                    }
                }
            }
            Self::Path { path } => plan.index("record_path", path, Some(selection)),
            Self::Prefix { prefix } => {
                if prefix.is_empty() {
                    return Err(SemanticRevisionError::invalid_value(
                        prefix,
                        "a nonempty stored path prefix",
                    ));
                }
                plan.requested.insert(StorageQuery::Prefix {
                    prefix: prefix.clone(),
                    selection_id: selection.into(),
                });
            }
            Self::Owners { element_ids } => {
                for id in element_ids {
                    if plan.membership(
                        selection,
                        SemanticRevisionMember::Record(SemanticRecordRef::element(id)),
                    )? {
                        plan.index("artifact_owner", id, Some(selection));
                        plan.index("association_element", id, Some(selection));
                    }
                }
            }
            Self::Connections {
                element_ids,
                incoming,
            } => {
                for id in element_ids {
                    plan.index("connection_source", id, Some(selection));
                    if *incoming {
                        plan.index("connection_target", id, Some(selection));
                    }
                }
            }
            Self::Dependents { record } => plan.index(
                "artifact_dependency",
                &serde_json::to_string(record)?,
                Some(selection),
            ),
            Self::Content { content_ref } => {
                plan.index("content_ref", content_ref, Some(selection));
                plan.key("artifact_blob", content_ref);
                if let Some(row) = plan.rows.values().find(|row| {
                    super::super::content::attachments::is_attachment(row)
                        && &row.key.identity == content_ref
                }) {
                    let owner = super::super::content::attachments::decode(row)?.artifact_id;
                    Self::Records {
                        records: vec![SemanticRecordRef::artifact(owner)],
                    }
                    .request(plan, selection)?;
                }
            }
        }
        Ok(())
    }
}

impl ReadPlan {
    pub(super) fn location_ancestors(&mut self, selection: &str) -> SemanticRevisionResult<()> {
        for metadata in self.metadata.clone() {
            if let SemanticRevisionMetadata::Record(header) = metadata {
                if header.record.kind == SemanticRecordKind::Element
                    && self.membership(
                        selection,
                        SemanticRevisionMember::Record(header.record.clone()),
                    )?
                {
                    self.index(
                        "containment_target",
                        &header.record.record_id,
                        Some(selection),
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn expand_selected(&mut self, selection: &str) -> SemanticRevisionResult<()> {
        for metadata in self.metadata.clone() {
            match metadata {
                SemanticRevisionMetadata::Record(header) => {
                    self.selected_record(selection, &header)?
                }
                SemanticRevisionMetadata::Membership(member)
                    if member.selection_id == selection =>
                {
                    match member.member {
                        SemanticRevisionMember::Record(record) => self.records([record])?,
                        SemanticRevisionMember::Connection(id) => self.key("connection", &id),
                    }
                }
                SemanticRevisionMetadata::Connection(connection) => {
                    self.membership(
                        selection,
                        SemanticRevisionMember::Connection(connection.connection_id),
                    )?;
                    self.records([
                        SemanticRecordRef::element(connection.relationship.source_element_id),
                        SemanticRecordRef::element(connection.relationship.target_element_id),
                    ])?;
                }
                SemanticRevisionMetadata::Association(association) => {
                    self.selected_association(selection, &association)?
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn selected_record(
        &mut self,
        selection: &str,
        header: &SemanticRevisionRecordHeader,
    ) -> SemanticRevisionResult<()> {
        self.membership(
            selection,
            SemanticRevisionMember::Record(header.record.clone()),
        )?;
        let key = StorageRowKey {
            kind: "record".into(),
            identity: serde_json::to_string(&header.record)?,
        };
        let Some(body) = self.rows[&key].body_json.as_ref() else {
            return Ok(());
        };
        let body = super::super::content::decode(body)?.record;
        self.records(body.intrinsic_dependencies())?;
        if let SemanticRevisionBody::Artifact(artifact) = body {
            self.index("association_artifact", &artifact.artifact_id, None);
            // Hidden associations suppress the original owner fallback just as in the full snapshot.
            let complete = self.completed.contains(&StorageQuery::Index {
                name: "association_artifact".into(),
                value: artifact.artifact_id.clone(),
                selection_id: None,
            });
            if complete && !self.metadata.iter().any(|row| matches!(row, SemanticRevisionMetadata::Association(a) if a.artifact_id == artifact.artifact_id)) {
                self.records([SemanticRecordRef::element(artifact.semantic_element_id)])?;
            }
        }
        Ok(())
    }

    fn selected_association(
        &mut self,
        selection: &str,
        association: &SemanticRevisionAssociation,
    ) -> SemanticRevisionResult<()> {
        if !self.metadata.iter().any(|row| matches!(row, SemanticRevisionMetadata::Selection(head) if head.selection_id == selection && head.source_context == association.source_context)) {
            return Ok(());
        }
        let owner = SemanticRecordRef::element(&association.semantic_element_id);
        let artifact = SemanticRecordRef::artifact(&association.artifact_id);
        let selected_owner =
            self.membership(selection, SemanticRevisionMember::Record(owner.clone()))?;
        let selected_artifact =
            self.membership(selection, SemanticRevisionMember::Record(artifact.clone()))?;
        let loaded_artifact = self.rows.contains_key(&StorageRowKey {
            kind: "record".into(),
            identity: serde_json::to_string(&artifact)?,
        });
        if selected_owner && (selected_artifact || loaded_artifact) {
            self.records([owner, artifact])?;
        }
        Ok(())
    }
}
