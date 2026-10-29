//! Knowledge owns memory-space resolution and document disclosure. Authority comes
//! from the host; callers cannot establish ownership through a root or metadata.
use lumvise_contracts::{
    KnowledgeAccessContext, KnowledgeDocument, KnowledgeDocumentAccess, KnowledgePrincipalKind,
    KnowledgeSpaceDescriptor, KnowledgeSpacePurpose,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

pub(super) use lumvise_contracts::is_knowledge_space_root as is_private_root;

fn current(context: &mut PluginContext<'_>) -> Result<KnowledgeAccessContext, PluginError> {
    let scope = context.host_call("runtime.knowledge_scope", json!({"operation":"current"}))?;
    crate::parse(scope, "current host-issued memory authority")
}

pub(super) fn resolve(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let purpose: KnowledgeSpacePurpose = crate::parse(
        input["purpose"].clone(),
        "self or current_project space purpose",
    )?;
    let access = current(context)?;
    let allowed = matches!(
        (access.principal_kind, purpose),
        (
            KnowledgePrincipalKind::Assistant | KnowledgePrincipalKind::Agent,
            KnowledgeSpacePurpose::Own
        ) | (
            KnowledgePrincipalKind::Mcp,
            KnowledgeSpacePurpose::CurrentProject
        )
    );
    if !allowed {
        return Err(denied(
            &input.to_string(),
            "space purpose allowed for this principal",
        ));
    }
    let descriptor = access
        .space
        .ok_or_else(|| denied("unbound", "host-resolved owned space"))?;
    super::publication::ensure_space(context, &descriptor)?;
    Ok(json!({"space":descriptor}))
}

pub(super) fn require_root(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<Option<KnowledgeAccessContext>, PluginError> {
    if !is_private_root(root) {
        return Ok(None);
    }
    let access = current(context)?;
    if access
        .space
        .as_ref()
        .is_none_or(|space| space.project_root != root)
    {
        return Err(denied(root, "the caller's host-bound document space"));
    }
    Ok(Some(access))
}

pub(super) fn protect_write(
    context: &mut PluginContext<'_>,
    document: &mut KnowledgeDocument,
) -> Result<(), PluginError> {
    let Some(scope) = require_root(context, &document.project_root)? else {
        return Ok(());
    };
    if scope.principal_kind != KnowledgePrincipalKind::Agent
        && !matches!(
            document.document_kind,
            lumvise_contracts::KnowledgeDocumentKind::Context
                | lumvise_contracts::KnowledgeDocumentKind::Report
        )
    {
        return Err(denied(
            &document.document_id,
            "context or report memory document",
        ));
    }
    let descriptor = scope.space.as_ref().expect("checked owned space");
    super::publication::ensure_space(context, descriptor)?;
    let mut access = document
        .access
        .clone()
        .unwrap_or_else(|| KnowledgeDocumentAccess {
            space_id: descriptor.space_id.clone(),
            required_projects: Vec::new(),
        });
    for project in &scope.source_projects {
        if !access
            .required_projects
            .iter()
            .any(|required| required.same_project(project))
        {
            access.required_projects.push(project.clone());
        }
    }
    require_access(&scope, &access)?;
    document.access = Some(access);
    Ok(())
}

pub(super) fn require_element(
    context: &mut PluginContext<'_>,
    element: &Value,
) -> Result<(), PluginError> {
    let root = element["project_root"].as_str().unwrap_or_default();
    let Some(scope) = require_root(context, root)? else {
        return Ok(());
    };
    element_visible(&scope, element)
        .then_some(())
        .ok_or_else(|| denied(root, "authorized document provenance"))
}

pub(super) fn filter_elements(
    context: &mut PluginContext<'_>,
    root: &str,
    elements: &mut Vec<Value>,
) -> Result<(), PluginError> {
    let Some(scope) = require_root(context, root)? else {
        return Ok(());
    };
    elements.retain(|element| element_visible(&scope, element));
    Ok(())
}

fn element_visible(scope: &KnowledgeAccessContext, element: &Value) -> bool {
    if super::record_format::is_root(element) || is_descriptor(element) {
        return true;
    }
    let metadata = &element["metadata"]["indexer_metadata"];
    let value = metadata["knowledge_document"]
        .get("access")
        .or_else(|| metadata["knowledge_document_deleted"]["document"].get("access"));
    // Established Agent documents retain their owner; subsequent writes add provenance.
    if value.is_none()
        && scope
            .space
            .as_ref()
            .is_some_and(|space| space.space_kind == lumvise_contracts::KnowledgeSpaceKind::Agent)
    {
        return true;
    }
    value
        .cloned()
        .and_then(|value| serde_json::from_value::<KnowledgeDocumentAccess>(value).ok())
        .is_some_and(|access| require_access(scope, &access).is_ok())
}

fn require_access(
    scope: &KnowledgeAccessContext,
    access: &KnowledgeDocumentAccess,
) -> Result<(), PluginError> {
    let owns = scope
        .space
        .as_ref()
        .is_some_and(|space| space.space_id == access.space_id);
    let granted = access.required_projects.iter().all(|project| {
        scope
            .project_grants
            .iter()
            .any(|grant| grant.project.same_project(project))
    });
    if owns && granted {
        return Ok(());
    }
    Err(denied(
        &access.space_id,
        "owned space and current grants for every required project",
    ))
}

pub(super) fn authorize_publication(
    context: &mut PluginContext<'_>,
    batch: &Value,
) -> Result<(), PluginError> {
    let root = batch["project_root"].as_str().unwrap_or_default();
    let Some(scope) = require_root(context, root)? else {
        return Ok(());
    };
    for artifact in batch["semantic_artifacts"].as_array().into_iter().flatten() {
        if artifact["artifact_kind"] != "knowledge_document" {
            continue;
        }
        let access: KnowledgeDocumentAccess = crate::parse(
            artifact["metadata"]["knowledge_document"]["access"].clone(),
            "frozen document access scope",
        )?;
        require_access(&scope, &access)?;
        if scope.source_projects.iter().any(|project| {
            !access
                .required_projects
                .iter()
                .any(|required| required.same_project(project))
        }) {
            return Err(denied(
                root,
                "frozen document provenance covering all supplied project context",
            ));
        }
    }
    Ok(())
}

pub(super) const DESCRIPTOR_PATH: &str = "@documents/space";

pub(super) fn is_descriptor(element: &Value) -> bool {
    element["element_kind"] == "virtual_document_space"
        && element["path"] == DESCRIPTOR_PATH
        && element["metadata"]["provider_instance_id"] == super::record_format::PROVIDER
}

pub(super) fn stored_descriptor(element: &Value) -> Result<KnowledgeSpaceDescriptor, PluginError> {
    if !is_descriptor(element) {
        return Err(denied(
            &element.to_string(),
            "Knowledge-owned space descriptor",
        ));
    }
    crate::parse(
        element["metadata"]["indexer_metadata"]["knowledge_space"].clone(),
        "immutable memory-space descriptor",
    )
}

fn denied(value: &str, expected: &str) -> PluginError {
    super::invalid("knowledge_space_denied", value, expected)
}
