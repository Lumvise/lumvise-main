//! Decodes retired records separately from the current artifact taxonomy.

use std::collections::BTreeSet;

use lumvise_contracts::{
    KnowledgeArtifactLifecycleV2, KnowledgeDocument, KnowledgeDocumentKind,
    KnowledgeDocumentReference,
};
use lumvise_plugin_sdk::PluginError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{super::records, rewrite};

pub(super) fn kind(artifact: &Value) -> Option<KnowledgeDocumentKind> {
    let tags = artifact["metadata"]["knowledge"]["tags"].as_array();
    let tagged = |tag: &str| tags.is_some_and(|tags| tags.iter().any(|value| value == tag));
    if tagged("canvas") {
        return Some(KnowledgeDocumentKind::Canvas);
    }
    if tagged("ruleset") && tagged("agents") {
        return Some(KnowledgeDocumentKind::Ruleset);
    }
    match artifact["artifact_kind"].as_str()? {
        "specification" | "requirements" => None,
        "report" | "derived_summary" if !tagged("cultivation-functional") => {
            Some(KnowledgeDocumentKind::Report)
        }
        _ => Some(KnowledgeDocumentKind::Context),
    }
}

pub(super) fn document(
    artifact: &Value,
    root: &str,
    converted: &BTreeSet<String>,
) -> Result<KnowledgeDocument, PluginError> {
    let knowledge = &artifact["metadata"]["knowledge"];
    validate_project(artifact, root)?;
    let id = text(artifact, "artifact_id")?;
    Ok(KnowledgeDocument {
        document_id: id.into(),
        semantic_element_id: records::element_id(id),
        attachment_owner_id: Some(id.into()),
        project_root: root.into(),
        document_kind: kind(artifact).expect("selected legacy document"),
        title: text(artifact, "title")?.into(),
        content: rewrite::content_text(text(artifact, "content")?, converted),
        path: knowledge["path"].as_str().map(str::to_owned),
        tags: tags(knowledge)?,
        references: references(artifact, converted)?,
        metadata: metadata(artifact, converted),
        lifecycle: lifecycle(knowledge)?,
    })
}

fn validate_project(artifact: &Value, root: &str) -> Result<(), PluginError> {
    let declared = artifact["metadata"]["knowledge"]["project_root"].as_str();
    if declared.is_some_and(|declared| declared != root) {
        return Err(super::super::invalid(
            "legacy_document_project_mismatch",
            &artifact.to_string(),
            root,
        ));
    }
    Ok(())
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, PluginError> {
    value[field].as_str().ok_or_else(|| {
        super::super::invalid(
            "invalid_legacy_document",
            &value[field].to_string(),
            &format!("string {field}"),
        )
    })
}

fn tags(knowledge: &Value) -> Result<Vec<String>, PluginError> {
    if knowledge["tags"].is_null() {
        return Ok(vec![]);
    }
    crate::parse(knowledge["tags"].clone(), "legacy Knowledge tags array")
}

fn lifecycle(knowledge: &Value) -> Result<KnowledgeArtifactLifecycleV2, PluginError> {
    if knowledge["lifecycle"].is_null() {
        return Ok(KnowledgeArtifactLifecycleV2::default());
    }
    crate::parse(knowledge["lifecycle"].clone(), "legacy Knowledge lifecycle")
}

fn references(
    artifact: &Value,
    converted: &BTreeSet<String>,
) -> Result<Vec<KnowledgeDocumentReference>, PluginError> {
    let mut references = vec![
        json!({"target_kind": "semantic_element", "semantic_element_id": text(artifact, "semantic_element_id")?}),
    ];
    if let Some(dependencies) = artifact["dependencies"].as_array() {
        references.extend(
            dependencies
                .iter()
                .map(|dependency| dependency["target"].clone()),
        );
    }
    let mut value = json!(references);
    rewrite::value(&mut value, converted);
    let mut references: Vec<KnowledgeDocumentReference> =
        crate::parse(value, "legacy document references")?;
    let mut seen = std::collections::HashSet::new();
    references.retain(|reference| seen.insert(reference.clone()));
    records::validate_references(&references)?;
    Ok(references)
}

fn metadata(artifact: &Value, converted: &BTreeSet<String>) -> Value {
    let original = &artifact["metadata"]["knowledge"]["metadata"];
    let mut rewritten = original.clone();
    rewrite::value(&mut rewritten, converted);
    let mut metadata = rewritten.as_object().cloned().unwrap_or_default();
    metadata.insert("legacy_knowledge".into(), json!({"schema": 1, "artifact_id": artifact["artifact_id"],
        "knowledge_type": artifact["artifact_kind"], "semantic_element_id": artifact["semantic_element_id"],
        "snapshot_sha256": signature(artifact), "graph_metadata": artifact["metadata"]}));
    Value::Object(metadata)
}

pub(super) fn signature(artifact: &Value) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(artifact).expect("JSON Value is serializable"),
    ))
}

pub(super) fn matches(document: &KnowledgeDocument, artifact: &Value, root: &str) -> bool {
    let provenance = &document.metadata["legacy_knowledge"];
    document.project_root == root
        && provenance["schema"] == 1
        && provenance["artifact_id"] == artifact["artifact_id"]
        && provenance["snapshot_sha256"] == signature(artifact)
        && document.attachment_owner_id.as_deref() == artifact["artifact_id"].as_str()
}
