use crate::KnowledgeArtifact;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(super) fn copy_referenced_attachments(
    source: &KnowledgeArtifact,
    copy: &mut KnowledgeArtifact,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let mut documents = vec![copy.metadata.clone()];
    let content = serde_json::from_str::<Value>(&copy.content).ok();
    if let Some(content) = content {
        documents.push(content);
    }
    let mut references = HashMap::new();
    for document in &mut documents {
        replace_attachment_refs(
            document,
            source,
            &copy.artifact_id,
            context,
            &mut references,
        )?;
    }
    copy.metadata = documents.remove(0);
    if let Some(content) = documents.pop() {
        copy.content = serde_json::to_string(&content).map_err(encoding_error)?;
    }
    Ok(())
}

fn replace_attachment_refs(
    value: &mut Value,
    source: &KnowledgeArtifact,
    target_id: &str,
    context: &mut PluginContext<'_>,
    references: &mut HashMap<String, String>,
) -> Result<(), PluginError> {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "contentRef" | "content_ref") {
                    if let Some(original) = value.as_str().filter(|reference| {
                        reference.starts_with("canvas-file:")
                            || reference.starts_with("artifact-transfer:")
                    }) {
                        *value = Value::String(copy_reference(
                            original, source, target_id, context, references,
                        )?);
                    }
                }
                replace_attachment_refs(value, source, target_id, context, references)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                replace_attachment_refs(value, source, target_id, context, references)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn copy_reference(
    original: &str,
    source: &KnowledgeArtifact,
    target_id: &str,
    context: &mut PluginContext<'_>,
    references: &mut HashMap<String, String>,
) -> Result<String, PluginError> {
    if let Some(copied) = references.get(original) {
        return Ok(copied.clone());
    }
    let response = context.host_call("storage.semantic", json!({"operation":"copy_artifact_attachment", "source_artifact_id":source.artifact_id, "target_artifact_id":target_id, "content_ref":original}))?;
    let reference = response["content_ref"]
        .as_str()
        .ok_or_else(|| {
            PluginError::new(
                "invalid_attachment_copy",
                format!("invalid response `{response}`; expected copied content_ref string"),
                false,
            )
        })?
        .to_string();
    references.insert(original.to_string(), reference.clone());
    Ok(reference)
}

pub(super) fn encoding_error(error: serde_json::Error) -> PluginError {
    PluginError::new(
        "invalid_transfer_content",
        format!("invalid transfer content `{error}`; expected serializable artifact document"),
        false,
    )
}
