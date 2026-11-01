//! Configures document-owned maintenance through the existing host permission and Agent journal.
//! Knowledge owns the document editing base; Agents alone selects inputs and executes Missions.
mod schema;
mod status;
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) use schema::{input_schema, output_schema};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigureDocumentHook {
    operation_id: String,
    project_root: String,
    document_id: String,
    expected_hook_revision: Option<u64>,
    enabled: bool,
    expected_revision: Option<u64>,
    expected_semantic_element_id: Option<String>,
    settings: Option<Value>,
}

pub(super) fn configure(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: ConfigureDocumentHook = crate::parse(input, "document hook configuration")?;
    crate::artifact::require(&request.operation_id, "operation_id")?;
    let view = permission_view(context, &request.project_root, &request.document_id)?;
    let operation = request.scoped_operation();
    if !request.enabled {
        return disable(context, &request, &operation);
    }
    let document = arm(context, &request, &view, &operation)?;
    let grant = save_document_permission(context, &request, &operation, &document)?;
    configure_executor(context, &request, &grant, &operation, &document)?;
    // Agents finishes the executor after this call, so the host's view is `pending` until its
    // accepted configuration is observed; the caller reads the hook again until it is `ready`.
    let saved =
        permission_view(context, &request.project_root, &request.document_id)?["delegation"]
            .clone();
    let settings = request.settings.as_ref().expect("enabled hook settings");
    Ok(
        json!({"hook":status::configured_hook(&saved, settings, &request.project_root),"document":document}),
    )
}

pub(super) fn get(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let identity: KnowledgeDocumentIdentity = crate::parse(input, "document hook identity")?;
    let document = records::required(context, &identity)?;
    let view = permission_view(context, &identity.project_root, &identity.document_id)?;
    let saved = &view["delegation"];
    if saved.is_null() {
        return Ok(json!({"hook":null,"document":document}));
    }
    let mut hook = hook_view(saved, status::setup_status(saved));
    if view["writable"] == true {
        status::inspect_executor(context, saved, &identity.project_root, &mut hook);
    }
    Ok(json!({"hook":hook,"document":document}))
}

fn save_document_permission(
    context: &mut PluginContext<'_>,
    request: &ConfigureDocumentHook,
    operation: &str,
    document: &KnowledgeDocument,
) -> Result<Value, PluginError> {
    let inputs = prepare_inputs(context, document, request.settings.as_ref().unwrap())?;
    let projects = document
        .access
        .as_ref()
        .map(|access| access.required_projects.clone())
        .unwrap_or_default();
    save_permission(
        context,
        request,
        operation,
        inputs["inputs"].clone(),
        json!(projects),
    )
}

fn hook_view(saved: &Value, status: &str) -> Value {
    json!({"delegation_id":saved["delegation_id"],"revision":saved["revision"],"enabled":saved["enabled"],
        "executor_agent_id":saved["executor_agent_id"],"inputs":saved["inputs"],"status":status})
}

fn permission_view(
    context: &mut PluginContext<'_>,
    root: &str,
    document: &str,
) -> Result<Value, PluginError> {
    let view = permission(context, json!({"operation":"get", "document_id":document}))?;
    if view["project_root"] != root {
        return Err(invalid(
            "memory_hook_scope",
            root,
            "the host-authorized owning memory space: pass the project_root from resolve_document_space \
             (MCP callers use purpose current_project, Assistant callers self). Reports under a source \
             project are hooked from the Hooks tab or Agents automation watches (configure_automation_watch)",
        ));
    }
    Ok(view)
}

fn permission(context: &mut PluginContext<'_>, request: Value) -> Result<Value, PluginError> {
    context.host_call(
        "runtime.knowledge_scope",
        json!({"operation":"delegation","request":request}),
    )
}

fn prepare_inputs(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    settings: &Value,
) -> Result<Value, PluginError> {
    invoke_agents(
        context,
        "prepare_memory_hook_inputs",
        json!({"document":document,"settings":settings}),
    )
}

fn invoke_agents(
    context: &mut PluginContext<'_>,
    export: &str,
    input: Value,
) -> Result<Value, PluginError> {
    let response = context.host_call(
        "plugin.invoke",
        json!({"plugin_id":"builtin.agents","export_id":export,"input":input}),
    )?;
    response.get("output").cloned().ok_or_else(|| {
        invalid(
            "memory_executor_response",
            &response.to_string(),
            "the signed Agent command output",
        )
    })
}

fn arm(
    context: &mut PluginContext<'_>,
    request: &ConfigureDocumentHook,
    view: &Value,
    operation: &str,
) -> Result<KnowledgeDocument, PluginError> {
    request.require_enabled_fields()?;
    let receipt = crate::publication_journal::execute(
        context,
        "document.hook",
        request,
        Some(operation),
        |context, operation| {
            let current = permission_view(context, &request.project_root, &request.document_id)?;
            if current["delegation"]["revision"].as_u64() != request.expected_hook_revision {
                return Err(invalid(
                    "memory_hook_revision_conflict",
                    &current.to_string(),
                    "the originally read hook revision",
                ));
            }
            let mut document = records::required(context, &request.identity())?;
            super::validate_locator_revision(
                &document,
                request.expected_semantic_element_id.as_ref().unwrap(),
            )?;
            super::validate_revision(&document, request.expected_revision.unwrap())?;
            prepare_inputs(context, &document, request.settings.as_ref().unwrap())?;
            freeze_marker(
                context,
                &mut document,
                view["delegation_id"].as_str().unwrap(),
                operation,
            )
            .map(Some)
        },
    )?;
    publication::accepted(
        context,
        &request.project_root,
        &request.document_id,
        receipt.as_ref(),
    )
}

fn freeze_marker(
    context: &mut PluginContext<'_>,
    document: &mut KnowledgeDocument,
    delegation: &str,
    operation: &str,
) -> Result<crate::publication_journal::FrozenKnowledgePublication, PluginError> {
    generated::arm_memory_hook(document, delegation)?;
    spaces::protect_write(context, document)?;
    let original = document.lifecycle.clone();
    crate::lifecycle::stamp(&mut document.lifecycle, Some(&original), Utc::now());
    let resources = assets::for_update(context, document)?.into_iter().collect();
    publication::prepare(
        context,
        document,
        resources,
        Some(&document.semantic_element_id),
        operation,
    )
}

fn save_permission(
    context: &mut PluginContext<'_>,
    request: &ConfigureDocumentHook,
    operation: &str,
    inputs: Value,
    required_projects: Value,
) -> Result<Value, PluginError> {
    let saved = permission(
        context,
        json!({"operation":"save","intent":{"operation_id":operation,
        "document_id":request.document_id,"expected_revision":request.expected_hook_revision,"enabled":request.enabled,
        "inputs":inputs,"required_projects":required_projects}}),
    )?;
    Ok(saved["delegation"].clone())
}

fn disable(
    context: &mut PluginContext<'_>,
    request: &ConfigureDocumentHook,
    operation: &str,
) -> Result<Value, PluginError> {
    let saved = save_permission(context, request, operation, json!([]), json!([]))?;
    Ok(json!({"hook":hook_view(&saved, "disabled")}))
}

/// Hands Agents the owner's baseline with the setup request: Agents runs inside this call, so it
/// cannot read the destination or the inputs through Knowledge itself.
fn configure_executor(
    context: &mut PluginContext<'_>,
    request: &ConfigureDocumentHook,
    grant: &Value,
    operation: &str,
    document: &KnowledgeDocument,
) -> Result<Value, PluginError> {
    let inputs: Vec<lumvise_contracts::KnowledgeDocumentReference> =
        crate::parse(grant["inputs"].clone(), "the saved delegated inputs")?;
    let baseline = json!({"destination":document,
        "inputs":super::delegation::read_setup_inputs(context, &document.project_root, &inputs)?});
    permission(
        context,
        json!({"operation":"prepare_executor", "delegation_id":grant["delegation_id"],"revision":grant["revision"]}),
    )?;
    invoke_agents(
        context,
        "configure_memory_executor",
        json!({"operation_id":operation,"agent_id":grant["executor_agent_id"],
        "delegation":{"delegation_id":grant["delegation_id"],"revision":grant["revision"],
            "project_root":request.project_root,"document_id":request.document_id},"enabled":true,
            "settings":request.settings,"baseline":baseline}),
    )
}

impl ConfigureDocumentHook {
    fn identity(&self) -> KnowledgeDocumentIdentity {
        KnowledgeDocumentIdentity {
            project_root: self.project_root.clone(),
            document_id: self.document_id.clone(),
        }
    }

    fn scoped_operation(&self) -> String {
        let key = serde_json::to_vec(&json!([
            self.project_root,
            self.document_id,
            self.operation_id
        ]))
        .expect("string identities");
        format!("memory-hook-setup:{:x}", Sha256::digest(key))
    }

    fn require_enabled_fields(&self) -> Result<(), PluginError> {
        if self.expected_revision.is_none()
            || self
                .expected_semantic_element_id
                .as_ref()
                .is_none_or(|id| id.trim().is_empty())
            || self.settings.is_none()
        {
            return Err(invalid(
                "memory_hook_base",
                &json!(self).to_string(),
                "the original document locator/revision and Agent hook settings when enabled",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example closing the tool description is the first call an agent copies, so it must
    /// satisfy the request type and every field the signed input schema requires.
    #[test]
    fn the_described_enable_example_is_a_complete_valid_request() {
        let description = crate::documents::description("configure_document_hook").unwrap();
        let (_, example) = description.split_once("Example: ").expect("example");
        let example: Value = serde_json::from_str(example).expect("example is JSON");

        let request: ConfigureDocumentHook =
            crate::parse(example.clone(), "described example").unwrap();
        request.require_enabled_fields().unwrap();
        let schema = schema::input_schema("configure_document_hook").unwrap();
        for field in schema["required"].as_array().unwrap() {
            assert!(example.get(field.as_str().unwrap()).is_some(), "{field}");
        }
    }
}
