//! Adopts or conditionally creates the one immutable project document anchor.
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use crate::documents::{record_format as format, records};
use crate::publication_journal as journal;

pub(super) fn ensure(context: &mut PluginContext<'_>, root: &str) -> Result<Value, PluginError> {
    if let Some(anchor) = current(context, root)? {
        return Ok(anchor);
    }
    let result = journal::execute(
        context,
        "document.root",
        &json!({"project_root":root}),
        None,
        |context, operation_id| {
            let observation =
                super::observe(context, root, &[format::ROOT_PATH.into()], &[], None)?;
            let element = root_element(&observation.project_id);
            Ok(Some(super::batch(
                root,
                &element["semantic_source_id"],
                observation,
                operation_id,
                json!({
                    "semantic_elements":[element], "semantic_relationships":[], "replace_paths":[format::ROOT_PATH], "semantic_artifacts":[]
                }),
            )))
        },
    );
    // Independent first documents can race only on this fixed, content-free anchor.
    if result
        .as_ref()
        .is_err_and(|error| error.code != "knowledge_revision_conflict")
    {
        result?;
    }
    current(context, root)?.ok_or_else(|| {
        super::super::invalid(
            "invalid_document_root",
            root,
            "one accepted Knowledge-owned root",
        )
    })
}

fn current(context: &mut PluginContext<'_>, root: &str) -> Result<Option<Value>, PluginError> {
    let roots = records::scoped_elements(
        context,
        root,
        json!({"Location":{
        "project_root":root, "path":format::ROOT_PATH, "line":1, "include_inactive":false}}),
    )?;
    match roots.as_slice() {
        [] => Ok(None),
        [element] if format::is_root(element) => Ok(Some(element.clone())),
        _ => Err(super::super::invalid(
            "invalid_document_root",
            &json!(roots).to_string(),
            "one Knowledge-owned project document root",
        )),
    }
}

fn root_element(project_id: &str) -> Value {
    json!({"semantic_element_id":format::root_id(project_id), "semantic_source_id":format::source_id(project_id),
        "semantic_element_type":"virtual_document_root", "semantic_element_name":"Project documents",
        "path":format::ROOT_PATH, "metadata":{"knowledge_document_root":{"schema":1}}})
}
