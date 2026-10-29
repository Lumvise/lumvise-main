//! Conditionally publishes one content-free space descriptor beside the document
//! anchor, preserving existing Agent document bodies and their immutable anchors.
use crate::documents::{records, spaces};
use lumvise_contracts::KnowledgeSpaceDescriptor;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

pub(in crate::documents) fn ensure(
    context: &mut PluginContext<'_>,
    descriptor: &KnowledgeSpaceDescriptor,
) -> Result<(), PluginError> {
    let root = &descriptor.project_root;
    spaces::require_root(context, root)?;
    if let Some(stored) = current(context, root)? {
        return require_equal(&stored, descriptor);
    }
    let anchor = super::root::ensure(context, root)?;
    let published = crate::publication_journal::execute(
        context,
        "document.space",
        descriptor,
        None,
        |context, operation_id| proposal(context, descriptor, &anchor, operation_id),
    );
    // Concurrent resolvers share only this immutable identity; adopt the winner.
    if let Some(stored) = current(context, root)? {
        return require_equal(&stored, descriptor);
    }
    published?;
    Err(super::super::invalid(
        "knowledge_space_unavailable",
        root,
        "accepted space descriptor",
    ))
}

fn proposal(
    context: &mut PluginContext<'_>,
    descriptor: &KnowledgeSpaceDescriptor,
    anchor: &Value,
    operation: &str,
) -> Result<Option<crate::publication_journal::FrozenKnowledgePublication>, PluginError> {
    let root = &descriptor.project_root;
    let observation = super::observe(context, root, &[spaces::DESCRIPTOR_PATH.into()], &[], None)?;
    let element = json!({"semantic_element_id":format!("knowledge-space:{}", descriptor.space_id),
        "semantic_source_id":anchor["semantic_source_id"], "semantic_element_type":"virtual_document_space",
        "semantic_element_name":"Document space", "path":spaces::DESCRIPTOR_PATH,
        "metadata":{"knowledge_space":descriptor}});
    Ok(Some(super::batch(
        root,
        &anchor["semantic_source_id"],
        observation,
        operation,
        json!({"semantic_elements":[element], "semantic_relationships":[],
            "replace_paths":[spaces::DESCRIPTOR_PATH], "semantic_artifacts":[]}),
    )))
}

fn current(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<Option<KnowledgeSpaceDescriptor>, PluginError> {
    let rows = records::scoped_elements(
        context,
        root,
        json!({"Location":{
        "project_root":root, "path":spaces::DESCRIPTOR_PATH, "line":1, "include_inactive":false}}),
    )?;
    match rows.as_slice() {
        [] => Ok(None),
        [element] => spaces::stored_descriptor(element).map(Some),
        _ => Err(super::super::invalid(
            "knowledge_space_conflict",
            root,
            "one immutable space descriptor",
        )),
    }
}

fn require_equal(
    stored: &KnowledgeSpaceDescriptor,
    expected: &KnowledgeSpaceDescriptor,
) -> Result<(), PluginError> {
    let mut routed = stored.clone();
    if let (Some(original), Some(current)) = (&mut routed.shared_project, &expected.shared_project)
    {
        if original.same_project(current) {
            original.project_root = current.project_root.clone();
        }
    }
    if &routed == expected {
        return Ok(());
    }
    Err(super::super::invalid(
        "knowledge_space_conflict",
        &stored.project_root,
        "the original host-bound space owner",
    ))
}
