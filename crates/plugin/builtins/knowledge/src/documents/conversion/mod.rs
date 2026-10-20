//! Explicit, restart-safe conversion of retired Knowledge records to documents.
//! Publish and verify content before retiring any legacy owner; never invent missions.

mod legacy;
mod rewrite;

use std::collections::BTreeSet;

use lumvise_contracts::KnowledgeDocument;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{assets, records};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversionRequest {
    project_root: String,
    #[serde(default = "preview_default")]
    preview: bool,
}

fn preview_default() -> bool {
    true
}

struct PreparedDocument {
    original: Value,
    document: KnowledgeDocument,
    staged: bool,
}

pub(super) fn is_staged_owner(document: &KnowledgeDocument, artifact: &Value) -> bool {
    legacy::matches(document, artifact, &document.project_root)
}

pub(super) fn required_for_source(
    context: &mut PluginContext<'_>,
    root: &str,
    element_id: &str,
    workflow_tag: &str,
) -> Result<bool, PluginError> {
    Ok(artifacts(context, root)?.iter().any(|artifact| {
        artifact["semantic_element_id"] == element_id
            && artifact["metadata"]["knowledge"]["tags"]
                .as_array()
                .is_some_and(|tags| tags.iter().any(|tag| tag == workflow_tag))
    }))
}

pub(super) fn convert(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: ConversionRequest = crate::parse(input, "project-scoped document conversion")?;
    crate::artifact::require(&request.project_root, "project_root")?;
    let artifacts = artifacts(context, &request.project_root)?;
    let converted = conversion_ids(context, &request.project_root, &artifacts)?;
    let prepared = prepare(context, &request.project_root, &artifacts, &converted)?;
    if !request.preview {
        publish(context, &prepared)?;
        rewrite_retained(context, &artifacts, &converted)?;
        rewrite_documents(context, &request.project_root, &converted)?;
        retire(context, &prepared)?;
    }
    Ok(summary(&request, &prepared, &artifacts))
}

fn summary(
    request: &ConversionRequest,
    prepared: &[PreparedDocument],
    artifacts: &[Value],
) -> Value {
    let conversions: Vec<Value> = prepared
        .iter()
        .map(|item| {
            json!({"artifact_id": item.document.document_id,
        "document_id": item.document.document_id, "document_kind": item.document.document_kind,
        "source_semantic_element_id": item.original["semantic_element_id"],
        "source_content_sha256": legacy::content_signature(item.original["content"].as_str().unwrap_or_default()),
        "status": item.document.lifecycle.status,
        "already_staged": item.staged})
        })
        .collect();
    let retained = artifacts.iter().filter(|artifact| legacy::kind(artifact).is_none())
        .map(|artifact| json!({"artifact_id": artifact["artifact_id"], "semantic_element_id": artifact["semantic_element_id"]})).collect::<Vec<_>>();
    json!({"project_root": request.project_root, "preview": request.preview,
        "total": conversions.len(), "conversions": conversions, "retained": retained})
}

fn artifacts(context: &mut PluginContext<'_>, root: &str) -> Result<Vec<Value>, PluginError> {
    let response = context.host_call(
        "storage.semantic",
        json!({"operation": "project_artifacts",
        "project_root": root, "artifact_namespace": "knowledge"}),
    )?;
    let artifacts = response["artifacts"].as_array().ok_or_else(|| {
        super::invalid(
            "invalid_conversion_storage",
            &response.to_string(),
            "project artifacts array",
        )
    })?;
    Ok(artifacts
        .iter()
        .filter(|artifact| artifact["metadata"]["knowledge"].is_object())
        .cloned()
        .collect())
}

fn conversion_ids(
    context: &mut PluginContext<'_>,
    root: &str,
    artifacts: &[Value],
) -> Result<BTreeSet<String>, PluginError> {
    let documents = project_documents(context, root)?;
    let mut ids: BTreeSet<String> = documents
        .into_iter()
        .filter(|doc| doc.metadata["legacy_knowledge"]["schema"] == 1)
        .map(|doc| doc.document_id)
        .collect();
    for artifact in artifacts
        .iter()
        .filter(|artifact| legacy::kind(artifact).is_some())
    {
        ids.insert(required_id(artifact)?.into());
    }
    Ok(ids)
}

fn project_documents(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    records::list(
        context,
        &lumvise_contracts::ListKnowledgeDocumentsRequest {
            project_root: Some(root.into()),
            include_inactive: true,
            ..Default::default()
        },
    )
}

fn required_id(artifact: &Value) -> Result<&str, PluginError> {
    artifact["artifact_id"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            super::invalid(
                "invalid_legacy_document",
                &artifact.to_string(),
                "nonempty artifact_id",
            )
        })
}

fn prepare(
    context: &mut PluginContext<'_>,
    root: &str,
    artifacts: &[Value],
    converted: &BTreeSet<String>,
) -> Result<Vec<PreparedDocument>, PluginError> {
    artifacts
        .iter()
        .filter(|artifact| legacy::kind(artifact).is_some())
        .map(|original| prepare_one(context, root, original, converted))
        .collect()
}

fn prepare_one(
    context: &mut PluginContext<'_>,
    root: &str,
    original: &Value,
    converted: &BTreeSet<String>,
) -> Result<PreparedDocument, PluginError> {
    let document = legacy::document(original, root, converted)?;
    let staged = records::find_for_conversion(context, root, &document.document_id)?;
    if staged
        .as_ref()
        .is_some_and(|stored| !legacy::matches(stored, original, root))
    {
        return Err(super::invalid(
            "document_conversion_conflict",
            &document.document_id,
            "unused identity or unchanged legacy provenance; preserve both records for review",
        ));
    }
    Ok(PreparedDocument {
        original: original.clone(),
        staged: staged.is_some(),
        document: staged.unwrap_or(document),
    })
}

fn publish(
    context: &mut PluginContext<'_>,
    prepared: &[PreparedDocument],
) -> Result<(), PluginError> {
    for item in prepared.iter().filter(|item| !item.staged) {
        records::write(context, &item.document)?;
    }
    for item in prepared {
        let stored = records::find(context, &item.document.document_id)?;
        if stored.as_ref() != Some(&item.document) {
            return Err(super::invalid(
                "document_conversion_verification",
                &item.document.document_id,
                "complete read-back of staged document",
            ));
        }
    }
    Ok(())
}

fn rewrite_retained(
    context: &mut PluginContext<'_>,
    artifacts: &[Value],
    converted: &BTreeSet<String>,
) -> Result<(), PluginError> {
    for original in artifacts
        .iter()
        .filter(|artifact| legacy::kind(artifact).is_none())
    {
        let mut artifact = original.clone();
        rewrite::artifact(&mut artifact, converted);
        if artifact != *original {
            validate_unchanged(context, original)?;
            stamp_retained(&mut artifact)?;
            upsert(context, artifact)?;
        }
    }
    Ok(())
}

fn stamp_retained(artifact: &mut Value) -> Result<(), PluginError> {
    let stored = artifact["metadata"]["knowledge"]["lifecycle"].clone();
    let mut lifecycle: lumvise_contracts::KnowledgeArtifactLifecycleV2 = if stored.is_null() {
        Default::default()
    } else {
        crate::parse(stored, "retained artifact lifecycle")?
    };
    let previous = lifecycle.clone();
    crate::lifecycle::stamp(&mut lifecycle, Some(&previous), chrono::Utc::now());
    artifact["metadata"]["knowledge"]["lifecycle"] =
        serde_json::to_value(lifecycle).expect("serializable lifecycle");
    Ok(())
}

fn rewrite_documents(
    context: &mut PluginContext<'_>,
    root: &str,
    converted: &BTreeSet<String>,
) -> Result<(), PluginError> {
    for original in project_documents(context, root)? {
        let mut document = original.clone();
        rewrite::document(&mut document, converted)?;
        if document != original {
            crate::lifecycle::stamp(
                &mut document.lifecycle,
                Some(&original.lifecycle),
                chrono::Utc::now(),
            );
            records::write(context, &document)?;
        }
    }
    Ok(())
}

fn upsert(context: &mut PluginContext<'_>, artifact: Value) -> Result<(), PluginError> {
    context.host_call(
        "storage.semantic",
        json!({"operation": "upsert_artifact", "artifact": artifact,
        "media_type": "text/markdown; charset=utf-8"}),
    )?;
    Ok(())
}

fn retire(
    context: &mut PluginContext<'_>,
    prepared: &[PreparedDocument],
) -> Result<(), PluginError> {
    // Re-read immediately before removal so concurrent legacy edits are not lost.
    for item in prepared {
        validate_unchanged(context, &item.original)?;
        assets::retire_legacy(context, &item.document)?;
    }
    Ok(())
}

fn validate_unchanged(
    context: &mut PluginContext<'_>,
    original: &Value,
) -> Result<(), PluginError> {
    let id = required_id(original)?;
    let current = context.host_call(
        "storage.semantic",
        json!({"operation": "artifact", "artifact_id": id}),
    )?;
    if current["artifact"] != *original {
        return Err(super::invalid(
            "document_conversion_conflict",
            id,
            "unchanged legacy record before replacement",
        ));
    }
    Ok(())
}
