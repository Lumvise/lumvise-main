//! One scoped acknowledgement reader for the existing document and artifact journals.
use lumvise_contracts::{
    KnowledgeArtifactRevisionReference, KnowledgeArtifactWriteReceipt,
    KnowledgeArtifactWriteReceiptRequest, KnowledgeDocumentRevisionReference,
    KnowledgeDocumentWriteReceipt, KnowledgeDocumentWriteReceiptRequest, KnowledgeWriteAction,
    KnowledgeWriteState,
};

use super::*;

#[derive(Clone, Copy)]
pub(super) enum ReceiptTarget {
    Document,
    Artifact,
}

struct KnowledgeWriteReceiptRead<'a> {
    project_root: &'a str,
    operation_id: &'a str,
    action: KnowledgeWriteAction,
    target: ReceiptTarget,
    publication_id: String,
}

/// Reads original document acceptance without resuming or loading its body.
/// Example: `read_document_write_receipt(context, &request)` survives payload collection.
pub(crate) fn read_document_write_receipt(
    context: &mut PluginContext<'_>,
    request: &KnowledgeDocumentWriteReceiptRequest,
) -> Result<KnowledgeDocumentWriteReceipt, PluginError> {
    let read = KnowledgeWriteReceiptRead::new(
        &request.project_root,
        &request.operation_id,
        request.action,
        ReceiptTarget::Document,
    )?;
    crate::parse(
        read.read(context)?,
        "an original document write acknowledgement",
    )
}

/// Reads original artifact acceptance without granting access to historical bodies.
/// Example: `read_artifact_write_receipt(context, &request)` recovers a collected write.
pub(crate) fn read_artifact_write_receipt(
    context: &mut PluginContext<'_>,
    request: &KnowledgeArtifactWriteReceiptRequest,
) -> Result<KnowledgeArtifactWriteReceipt, PluginError> {
    let read = KnowledgeWriteReceiptRead::new(
        &request.project_root,
        &request.operation_id,
        request.action,
        ReceiptTarget::Artifact,
    )?;
    crate::parse(
        read.read(context)?,
        "an original artifact write acknowledgement",
    )
}

impl<'a> KnowledgeWriteReceiptRead<'a> {
    fn new(
        project_root: &'a str,
        operation_id: &'a str,
        action: KnowledgeWriteAction,
        target: ReceiptTarget,
    ) -> Result<Self, PluginError> {
        crate::artifact::require(project_root, "project_root")?;
        let publication_id = requested_operation_id(target.action_name(action), operation_id)?;
        Ok(Self {
            project_root,
            operation_id,
            action,
            target,
            publication_id,
        })
    }

    fn read(&self, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
        match crate::storage::get(context, TABLE, &self.publication_id)? {
            Some(KnowledgeWriteRecord::Pending { publication, .. }) => {
                self.pending(context, publication)
            }
            Some(KnowledgeWriteRecord::Accepted { scope, receipt, .. }) => {
                self.accepted(context, scope, receipt)
            }
            Some(KnowledgeWriteRecord::Conflicted {
                scope,
                operation_id,
                ..
            }) => {
                self.require_scope(context, &scope)?;
                self.require_identity(Some(&operation_id))?;
                Ok(self.reply(KnowledgeWriteState::Conflicted))
            }
            Some(KnowledgeWriteRecord::Absent { operation_id, .. }) => {
                self.require_identity(Some(&operation_id))?;
                Ok(self.reply(KnowledgeWriteState::NotFound))
            }
            None => Ok(self.reply(KnowledgeWriteState::NotFound)),
        }
    }

    fn pending(
        &self,
        context: &mut PluginContext<'_>,
        publication: FrozenKnowledgePublication,
    ) -> Result<Value, PluginError> {
        self.require_identity(publication.operation_id())?;
        self.require_scope(context, &publication.scope()?)?;
        Ok(self.reply(KnowledgeWriteState::Pending))
    }

    fn accepted(
        &self,
        context: &mut PluginContext<'_>,
        scope: KnowledgePublicationScope,
        receipt: Value,
    ) -> Result<Value, PluginError> {
        self.require_scope(context, &scope)?;
        self.require_identity(receipt["operation_id"].as_str())?;
        let field = self.target.reference_field();
        self.target
            .validate_reference(&receipt[field], &scope.project_root)?;
        validate_acceptance(&receipt, self.action)?;
        let mut result = self.reply(KnowledgeWriteState::Accepted);
        result[field] = receipt[field].clone();
        result["lifecycle_revision"] = receipt["lifecycle_revision"].clone();
        result["commit_version"] = receipt["commit_version"].clone();
        Ok(result)
    }

    fn require_scope(
        &self,
        context: &mut PluginContext<'_>,
        scope: &KnowledgePublicationScope,
    ) -> Result<(), PluginError> {
        let route = KnowledgePublicationScope {
            project_root: self.project_root.into(),
            project_id: scope.project_id.clone(),
        };
        route.validate_lineage(context, &self.publication_id).map_err(|error| {
            if route.project_root == scope.project_root || error.code != "knowledge_project_mismatch" {
                return error;
            }
            PluginError::new("knowledge_publication_scope_mismatch", format!(
                "Knowledge write `{}` was requested through `{}`; expected a route to its original durable project",
                self.operation_id, self.project_root), false)
        })
    }

    fn require_identity(&self, saved: Option<&str>) -> Result<(), PluginError> {
        if saved == Some(self.publication_id.as_str()) {
            return Ok(());
        }
        Err(invalid_frozen_publication(
            &json!(saved),
            &format!("original operation {}", self.publication_id),
        ))
    }

    fn reply(&self, state: KnowledgeWriteState) -> Value {
        let mut result = json!({"operation_id":self.operation_id,"publication_id":self.publication_id,
            "project_root":self.project_root,"state":state,"commit_version":null,"lifecycle_revision":null});
        result[self.target.reference_field()] = Value::Null;
        result
    }
}

impl ReceiptTarget {
    pub(super) fn reference_field(self) -> &'static str {
        match self {
            Self::Document => "document_revision",
            Self::Artifact => "artifact_revision",
        }
    }

    pub(super) fn action_name(self, action: KnowledgeWriteAction) -> &'static str {
        match (self, action) {
            (Self::Document, KnowledgeWriteAction::Create) => "document.create",
            (Self::Document, KnowledgeWriteAction::Update) => "document.update",
            (Self::Document, KnowledgeWriteAction::Delete) => "document.delete",
            (Self::Artifact, KnowledgeWriteAction::Create) => "create",
            (Self::Artifact, KnowledgeWriteAction::Update) => "update",
            (Self::Artifact, KnowledgeWriteAction::Delete) => "remove",
        }
    }

    fn validate_reference(self, reference: &Value, root: &str) -> Result<(), PluginError> {
        match self {
            Self::Document => {
                let exact: KnowledgeDocumentRevisionReference =
                    crate::parse(reference.clone(), "an exact document write reference")?;
                crate::artifact::require(&exact.document_id, "document write document_id")?;
                crate::artifact::require(
                    &exact.semantic_element_id,
                    "document write semantic_element_id",
                )?;
            }
            Self::Artifact => {
                let exact: KnowledgeArtifactRevisionReference =
                    crate::parse(reference.clone(), "an exact artifact write reference")?;
                crate::artifact::require(&exact.artifact_id, "artifact write artifact_id")?;
            }
        }
        if reference["project_root"].as_str() == Some(root) {
            return Ok(());
        }
        Err(invalid_frozen_publication(
            reference,
            "an exact reference in the original publication project",
        ))
    }
}

fn validate_acceptance(receipt: &Value, action: KnowledgeWriteAction) -> Result<(), PluginError> {
    if receipt["commit_version"]
        .as_i64()
        .is_none_or(|counter| counter < 0)
    {
        return Err(invalid_frozen_publication(
            receipt,
            "the original nonnegative acceptance version",
        ));
    }
    if action == KnowledgeWriteAction::Delete && receipt["lifecycle_revision"].is_null() {
        return Ok(());
    }
    if receipt["lifecycle_revision"].as_u64().is_some() {
        return Ok(());
    }
    Err(invalid_frozen_publication(
        receipt,
        "the nonnegative lifecycle counter captured from the frozen body",
    ))
}
