//! Pure document provenance for Agents' causal policy. This command reads no storage and grants
//! no authority; document writes still pass through the original revision-checked publication.
pub(super) mod schema;
pub(crate) use schema::{input_schema, output_schema};

use lumvise_contracts::KnowledgeDocument;
use lumvise_plugin_sdk::PluginError;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const EXPORT_ID: &str = "prepare_document_maintenance";
pub(super) const MARKER: &str = "lumvise_maintenance";

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum MaintenanceRequest {
    Inspect {
        document: KnowledgeDocument,
    },
    Prepare {
        document: KnowledgeDocument,
        content: String,
        causal_paths: Vec<Vec<String>>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentMaintenanceMarker {
    owner: String,
    causal_paths: Vec<Vec<String>>,
    content_sha256: String,
}

pub(crate) fn invoke(input: Value) -> Result<Value, PluginError> {
    match crate::parse(input, "a captured maintenance document and causal paths")? {
        MaintenanceRequest::Inspect { document } => Ok(json!({"causal_paths":inspect(&document)})),
        MaintenanceRequest::Prepare {
            mut document,
            content,
            causal_paths,
        } => {
            document.content = content;
            advance_proposal(&mut document)?;
            stamp(&mut document, &causal_paths)?;
            Ok(json!({"metadata":document.metadata}))
        }
    }
}

fn advance_proposal(document: &mut KnowledgeDocument) -> Result<(), PluginError> {
    let previous = document.lifecycle.clone();
    if previous.revision == u64::MAX {
        return Err(super::invalid(
            "invalid_maintenance_revision",
            &previous.revision.to_string(),
            "a document revision below u64::MAX",
        ));
    }
    crate::lifecycle::stamp(&mut document.lifecycle, Some(&previous), chrono::Utc::now());
    Ok(())
}

pub(super) fn stamp(
    document: &mut KnowledgeDocument,
    paths: &[Vec<String>],
) -> Result<(), PluginError> {
    validate_paths(paths)?;
    if document.metadata.is_null() {
        document.metadata = json!({});
    }
    if !document.metadata.is_object() {
        return Err(super::invalid(
            "invalid_maintenance_metadata",
            &document.metadata.to_string(),
            "object metadata",
        ));
    }
    require_owned_marker(document)?;
    document.metadata[MARKER] = json!({"owner":"builtin.agents","causal_paths":paths,"content_sha256":signature(document, paths)});
    Ok(())
}

fn require_owned_marker(document: &KnowledgeDocument) -> Result<(), PluginError> {
    if let Some(marker) = document.metadata.get(MARKER) {
        if read_marker(document).is_none() {
            return Err(super::invalid(
                "maintenance_marker_conflict",
                &marker.to_string(),
                "an unused lumvise_maintenance field or this workflow's owned marker; preserve human metadata",
            ));
        }
    }
    Ok(())
}

fn inspect(document: &KnowledgeDocument) -> Vec<Vec<String>> {
    let Some(marker) = read_marker(document) else {
        return vec![vec![]];
    };
    let paths = marker.causal_paths;
    if marker.content_sha256 != signature(document, &paths) {
        return vec![vec![]];
    }
    if paths.is_empty() {
        return vec![vec![]];
    }
    paths
}

fn read_marker(document: &KnowledgeDocument) -> Option<DocumentMaintenanceMarker> {
    let marker: DocumentMaintenanceMarker =
        serde_json::from_value(document.metadata.get(MARKER)?.clone()).ok()?;
    (marker.owner == "builtin.agents" && validate_paths(&marker.causal_paths).is_ok())
        .then_some(marker)
}

fn validate_paths(paths: &[Vec<String>]) -> Result<(), PluginError> {
    for path in paths {
        let mut seen = std::collections::BTreeSet::new();
        if path
            .iter()
            .any(|id| id.trim().is_empty() || !seen.insert(id))
        {
            return Err(super::invalid(
                "invalid_maintenance_paths",
                &json!(paths).to_string(),
                "causal paths of distinct nonempty Agent identities",
            ));
        }
    }
    Ok(())
}

fn signature(document: &KnowledgeDocument, paths: &[Vec<String>]) -> String {
    let fields = super::generated::managed_fields(document);
    let content = super::generated::signature(document, &fields);
    let projection = crate::projection_identity::canonical_projection_json(&json!([
        document.project_root,
        document.document_id,
        document.document_kind,
        document.lifecycle.revision,
        document.path,
        document.tags,
        document.lifecycle.status,
        document.lifecycle.expires_at,
        document.lifecycle.superseded_by,
        content,
        paths
    ]));
    format!("{:x}", Sha256::digest(projection.as_bytes()))
}
