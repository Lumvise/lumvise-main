//! Bounded execution over the document owner. Host authority authenticates every action;
//! scheduling and Mission progress remain in Agents, and writes use the existing journal.
mod schema;

use lumvise_contracts::{
    KnowledgeCitationTarget, KnowledgeDocumentCitationSelection, KnowledgeDocumentReference,
    KnowledgeQualifiedCitation, KnowledgeRevisionCitationSelection,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const EXPORT_ID: &str = "execute_document_delegation";
pub(crate) use schema::{input_schema, output_schema};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegationExecutionRequest {
    delegation_id: String,
    revision: u64,
    action: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundDocumentAction {
    delegation_id: String,
    revision: u64,
    document_id: String,
    project_root: String,
    action: Value,
}

pub(crate) fn invoke(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: DelegationExecutionRequest =
        crate::parse(input, "an exact delegated memory action")?;
    let bound = context.host_call("runtime.knowledge_scope", json!({"operation":"delegation", "request":{
        "operation":"bind","delegation_id":request.delegation_id,"revision":request.revision,"action":request.action}}))?;
    let bound: BoundDocumentAction =
        crate::parse(bound, "a host-bound memory destination and action")?;
    let output = bound.execute(context)?;
    context.host_call("runtime.knowledge_scope", json!({"operation":"current"}))?;
    Ok(json!({"output":output}))
}

impl BoundDocumentAction {
    fn execute(&self, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
        match self.action["action"].as_str() {
            Some("read_destination") => super::get(self.identity(), context),
            Some("read_input") => self.read_input(context),
            Some("update_destination") => self.update(context),
            Some("read_receipt") => self.receipt(context),
            _ => Err(super::invalid(
                "invalid_delegated_action",
                &self.action.to_string(),
                "a host-authorized document action",
            )),
        }
    }

    fn identity(&self) -> Value {
        json!({"project_root":self.project_root, "document_id":self.document_id})
    }

    fn read_input(&self, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
        let reference = crate::parse(
            self.action["reference"].clone(),
            "an explicitly delegated input",
        )?;
        let citation = qualify_input(context, &self.project_root, reference)?;
        super::citations::resolve(context, &citation, false).map(|read| json!(read))
    }

    fn update(&self, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
        let mut request = self.identity();
        for field in [
            "expected_semantic_element_id",
            "expected_revision",
            "content",
        ] {
            request[field] = self.action[field].clone();
        }
        request["operation_id"] = json!(self.operation_id()?);
        let paths: Vec<Vec<String>> = crate::parse(
            self.action["causal_paths"].clone(),
            "host-bound causal paths",
        )?;
        super::update_document(request, context, Some((&self.delegation_id, &paths)))
    }

    fn receipt(&self, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
        let output = crate::publication_journal::invoke(
            "get_document_write_receipt",
            json!({
            "project_root":self.project_root, "operation_id":self.operation_id()?,"action":"update"}),
            context,
        )?;
        if let Some(reference) = output
            .get("document_revision")
            .filter(|value| !value.is_null())
        {
            if reference["project_root"] != self.project_root
                || reference["document_id"] != self.document_id
            {
                return Err(super::invalid(
                    "delegated_receipt_mismatch",
                    &reference.to_string(),
                    "the original delegated destination",
                ));
            }
        }
        Ok(output)
    }

    fn operation_id(&self) -> Result<String, PluginError> {
        let operation = self.action["operation_id"].as_str().unwrap_or_default();
        crate::artifact::require(operation, "delegated operation_id")?;
        // A Mission cannot guess another document's receipt or collide with an ordinary write.
        let key = serde_json::to_vec(&json!([self.delegation_id, self.revision, operation]))
            .expect("serializable identities");
        Ok(format!("memory-hook:{:x}", Sha256::digest(key)))
    }
}

fn qualify_input(
    context: &mut PluginContext<'_>,
    root: &str,
    reference: KnowledgeDocumentReference,
) -> Result<KnowledgeQualifiedCitation, PluginError> {
    if let KnowledgeDocumentReference::Qualified(citation) = reference {
        return Ok(citation);
    }
    let project = super::citations::project_identity(context, root)?;
    let selection = KnowledgeRevisionCitationSelection::Selected {
        selection_id: "working".into(),
        source_context: "local".into(),
    };
    let target = match reference {
        KnowledgeDocumentReference::SemanticElement {
            semantic_element_id,
        } => KnowledgeCitationTarget::SemanticElement {
            semantic_element_id,
            selection,
        },
        KnowledgeDocumentReference::Artifact {
            artifact_id,
            edit_version,
        } => KnowledgeCitationTarget::Artifact {
            artifact_id,
            edit_version,
            selection,
        },
        KnowledgeDocumentReference::Document { document_id } => KnowledgeCitationTarget::Document {
            document_id,
            selection: KnowledgeDocumentCitationSelection::Current,
        },
        KnowledgeDocumentReference::Qualified(_) => unreachable!("qualified input returned above"),
    };
    Ok(KnowledgeQualifiedCitation { project, target })
}
