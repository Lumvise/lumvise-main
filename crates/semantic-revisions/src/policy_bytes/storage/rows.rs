//! Exact row identity, payload and locator inputs; no policy is reconstructed by the storage adapter.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::policy_bytes) struct StorageRowKey {
    pub kind: String,
    pub identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::policy_bytes) struct StorageRow {
    pub key: StorageRowKey,
    pub metadata_json: String,
    pub body_json: Option<String>,
    pub indexes: Vec<super::indexes::StorageIndex>,
}

pub(super) fn encode(state: &SemanticRevisionState) -> SemanticRevisionResult<Vec<StorageRow>> {
    let bodies = state
        .records
        .iter()
        .map(|row| (&row.record, &row.body))
        .collect::<BTreeMap<_, _>>();
    state
        .metadata_rows()
        .into_iter()
        .map(|metadata| {
            let body = match &metadata {
                SemanticRevisionMetadata::Record(header) => bodies
                    .get(&header.record)
                    .copied()
                    .filter(|body| !matches!(body, SemanticRevisionBody::Pruned)),
                _ => None,
            };
            let body_json = if matches!(metadata, SemanticRevisionMetadata::Connection(_)) {
                Some(serde_json::to_string(&metadata)?)
            } else {
                body.map(super::content::encode).transpose()?
            };
            Ok(StorageRow {
                key: StorageRowKey {
                    kind: metadata.kind().into(),
                    identity: metadata.identity()?,
                },
                indexes: super::indexes::entries(&metadata, body)?,
                metadata_json: serde_json::to_string(&metadata)?,
                body_json,
            })
        })
        .collect()
}

pub(super) fn decode(
    root: &str,
    project: &str,
    commit: i64,
    rows: Vec<StorageRow>,
) -> SemanticRevisionResult<SemanticRevisionState> {
    crate::revision_index::require_project_identity(project)?;
    let mut metadata = Vec::new();
    let mut elements = Vec::new();
    let mut artifacts = Vec::new();
    for row in rows {
        if super::contributions::is_receipt(&row) {
            super::contributions::validate_receipt(&row)?;
            continue;
        }
        if super::content::attachments::is_attachment(&row) {
            super::content::attachments::decode(&row)?;
            continue;
        }
        if super::content::staged::is_staged(&row) {
            super::content::staged::validate(&row)?;
            continue;
        }
        if super::archives::is_metadata(&row) {
            super::archives::validate_metadata(&row)?;
            continue;
        }
        if super::vectors::is_vector(&row) {
            super::vectors::owner(&row)?;
            continue;
        }
        let decoded = decode_row(row, &mut elements, &mut artifacts)?;
        metadata.push(decoded);
    }
    let state =
        SemanticRevisionState::from_storage_facts(root, commit, metadata, elements, artifacts)?;
    if state.project_id != project || commit < 0 {
        return Err(SemanticRevisionError::invalid_value(
            format!("{} at {commit}", state.project_id),
            format!("lineage {project} at a nonnegative commit"),
        ));
    }
    Ok(state)
}

fn decode_row(
    row: StorageRow,
    elements: &mut Vec<SemanticElement>,
    artifacts: &mut Vec<SemanticArtifact>,
) -> SemanticRevisionResult<SemanticRevisionMetadata> {
    let metadata: SemanticRevisionMetadata = serde_json::from_str(&row.metadata_json)?;
    if row.key.kind != metadata.kind() || row.key.identity != metadata.identity()? {
        return Err(SemanticRevisionError::invalid_value(
            format!("{:?}", row.key),
            "metadata matching its indexed identity",
        ));
    }
    match (&metadata, row.body_json) {
        (SemanticRevisionMetadata::Record(header), body) => {
            let body = body
                .map(|body| super::content::decode(&body).map(|body| body.record))
                .transpose()?
                .unwrap_or(SemanticRevisionBody::Pruned);
            header.clone().restore(body.clone())?;
            append_body(body, elements, artifacts);
        }
        (SemanticRevisionMetadata::Connection(_), Some(body)) if body == row.metadata_json => {}
        (SemanticRevisionMetadata::Connection(_), _) => {
            return Err(SemanticRevisionError::invalid_value(
                row.key.identity,
                "an exact available connection body",
            ));
        }
        (_, Some(_)) => {
            return Err(SemanticRevisionError::invalid_value(
                row.key.kind,
                "body locators only for record or connection rows",
            ));
        }
        (_, None) => {}
    }
    Ok(metadata)
}

fn append_body(
    body: SemanticRevisionBody,
    elements: &mut Vec<SemanticElement>,
    artifacts: &mut Vec<SemanticArtifact>,
) {
    match body {
        SemanticRevisionBody::Element(element) => elements.push(element),
        SemanticRevisionBody::Artifact(artifact) => artifacts.push(artifact),
        SemanticRevisionBody::Pruned => {}
    }
}

pub(super) fn index(rows: Vec<StorageRow>) -> BTreeMap<StorageRowKey, StorageRow> {
    rows.into_iter().map(|row| (row.key.clone(), row)).collect()
}
