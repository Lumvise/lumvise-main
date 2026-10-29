//! Private HTTP transfer projection; PZ encoding stays in DB Core and merge policy stays in the state.
use super::*;
use lumvise_contracts::{ArtifactBlob, StoredArtifactTextVector, StoredSemanticElementNameVector};
use std::collections::{BTreeMap, BTreeSet};
mod imports;
mod origin;
pub(super) use imports::{initial, receipt_intent, receipt_read};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArchivePayload {
    display_name: Option<String>,
    source_revision: Option<String>,
    selection_id: String,
    revision_state: Option<SemanticRevisionState>,
    project_id: String,
    snapshot_id: String,
    canonical_remote: Option<String>,
    snapshot: ArchiveSnapshot,
    artifact_blobs: Vec<ArtifactBlob>,
    artifact_text_vectors: Vec<StoredArtifactTextVector>,
    element_name_vectors: Vec<StoredSemanticElementNameVector>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArchiveSnapshot {
    project_root: String,
    commit_version: i64,
    published_at: String,
    elements: Vec<SemanticElement>,
    artifacts: Vec<SemanticArtifact>,
    relationships: Vec<SemanticRelationship>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRequest {
    operation: String,
    format_release: u32,
    operation_id: String,
    archive_json: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportRequest {
    operation: String,
    selection_id: String,
    snapshot_id: String,
}

pub(super) fn prepare(input: &str) -> SemanticRevisionResult<StorageResult> {
    let request: serde_json::Value = serde_json::from_str(input)?;
    if request["operation"] == "import_archive" {
        imports::decode(input)?;
        return encoded("intent", &read::ReadIntent::All);
    }
    let request: ExportRequest = serde_json::from_str(input)?;
    if request.operation != "export_archive" {
        return Err(SemanticRevisionError::invalid_value(
            &request.operation,
            "export_archive",
        ));
    }
    encoded(
        "intent",
        &read::ReadIntent::Archive {
            selection_id: request.selection_id,
        },
    )
}

pub(super) fn execute(
    state: &SemanticRevisionState,
    input: &str,
    rows: &[StorageRow],
    actor: Option<&str>,
    timestamp: &SemanticRevisionTimestamp,
    next: i64,
) -> SemanticRevisionResult<StorageResult> {
    if serde_json::from_str::<serde_json::Value>(input)?["operation"] == "import_archive" {
        return imports::apply(state, input, rows, actor, timestamp.unix_ms, next);
    }
    let request: ExportRequest = serde_json::from_str(input)?;
    let archive = capture(state, &request, rows, &timestamp.rfc3339)?;
    encoded(
        "archive",
        &serde_json::json!({"format_release":1,"archive_json":serde_json::to_string(&archive)?}),
    )
}

fn capture(
    state: &SemanticRevisionState,
    request: &ExportRequest,
    rows: &[StorageRow],
    published: &str,
) -> SemanticRevisionResult<ArchivePayload> {
    let selected = state
        .snapshot(&request.selection_id, published)?
        .ok_or_else(|| {
            SemanticRevisionError::invalid_value(
                &request.selection_id,
                "an available archive selection",
            )
        })?;
    let public = request.selection_id == "public";
    let hints = if public {
        origin::Origin::default()
    } else {
        origin::read(rows)?
    };
    let anchor = selected.selection.source_anchor.clone();
    if !public {
        state.validate_archive_state()?;
    }
    let available = if public {
        selected_bodies(state, &selected)
    } else {
        state.records.clone()
    };
    let (artifact_text_vectors, element_name_vectors) = vectors::archive::export(rows, &available)?;
    let attachment_owners = if public {
        available
            .iter()
            .filter(|row| row.record.kind == SemanticRecordKind::Artifact)
            .map(|row| row.record.record_id.clone())
            .collect()
    } else {
        state.retained_artifact_owner_ids()
    };
    Ok(ArchivePayload {
        display_name: hints.display_name,
        source_revision: anchor
            .as_ref()
            .filter(|anchor| !anchor.dirty)
            .map(|anchor| anchor.revision.clone())
            .or(hints.source_revision),
        selection_id: request.selection_id.clone(),
        revision_state: (!public).then(|| state.clone()),
        project_id: state.project_id.clone(),
        snapshot_id: request.snapshot_id.clone(),
        canonical_remote: anchor
            .and_then(|anchor| anchor.canonical_remote)
            .or(hints.canonical_remote),
        snapshot: snapshot_records(selected),
        artifact_blobs: blobs(&available, rows, published, &attachment_owners)?,
        artifact_text_vectors,
        element_name_vectors,
    })
}

fn selected_bodies(
    state: &SemanticRevisionState,
    selected: &SemanticRevisionSelectionSnapshot,
) -> Vec<SemanticStoredRevision> {
    let mut ids = selected.expectation.records.iter().collect::<BTreeSet<_>>();
    ids.extend(selected.required_records.iter().map(|row| &row.record));
    state
        .records
        .iter()
        .filter(|row| ids.contains(&row.record))
        .cloned()
        .collect()
}

fn snapshot_records(selected: SemanticRevisionSelectionSnapshot) -> ArchiveSnapshot {
    let mut snapshot = ArchiveSnapshot {
        project_root: selected.selection.project_root,
        commit_version: selected.commit_version,
        published_at: selected.published_at,
        elements: selected.elements,
        artifacts: selected.artifacts,
        relationships: selected
            .connections
            .into_iter()
            .map(|edge| edge.relationship)
            .collect(),
    };
    for required in selected.required_records {
        match required.body {
            SemanticRevisionBody::Element(element) => snapshot.elements.push(element),
            SemanticRevisionBody::Artifact(artifact) => snapshot.artifacts.push(artifact),
            SemanticRevisionBody::Pruned => {}
        }
    }
    snapshot
}

fn blobs(
    records: &[SemanticStoredRevision],
    rows: &[StorageRow],
    published: &str,
    attachment_owners: &BTreeSet<String>,
) -> SemanticRevisionResult<Vec<ArtifactBlob>> {
    let contents = content::available(rows)?;
    let mut blobs = BTreeMap::new();
    for owner in attachment_owners {
        for attachment in content::attachments::export(rows, owner)? {
            blobs.insert(attachment.content_ref.clone(), attachment);
        }
    }
    for stored in records {
        if let SemanticRevisionBody::Artifact(artifact) = &stored.body {
            let Some(reference) = &artifact.content_ref else {
                continue;
            };
            let content = contents.get(reference).ok_or_else(|| {
                SemanticRevisionError::invalid_value(reference, "available archived bytes")
            })?;
            blobs.insert(
                reference.clone(),
                ArtifactBlob {
                    content_ref: reference.clone(),
                    artifact_id: artifact.artifact_id.clone(),
                    media_type: content.media_type.clone(),
                    content: content.bytes.clone(),
                    updated_at: content.updated_at.as_deref().unwrap_or(published).into(),
                },
            );
        }
    }
    Ok(blobs.into_values().collect())
}

pub(super) fn contents(
    input: &str,
    contents: &mut BTreeMap<String, content::StoredContent>,
) -> SemanticRevisionResult<()> {
    let (_, archive) = imports::decode(input)?;
    for blob in archive.artifact_blobs {
        content::insert(
            contents.entry(blob.content_ref.clone()),
            content::StoredContent {
                content_ref: blob.content_ref,
                media_type: blob.media_type,
                bytes: blob.content,
                updated_at: Some(blob.updated_at),
            },
        )?;
    }
    Ok(())
}

pub(super) fn is_receipt(row: &StorageRow) -> bool {
    row.key.kind == "archive_receipt"
}

pub(super) fn is_metadata(row: &StorageRow) -> bool {
    is_receipt(row) || origin::is_origin(row)
}

pub(super) fn validate_metadata(row: &StorageRow) -> SemanticRevisionResult<()> {
    if is_receipt(row) {
        return validate_receipt(row);
    }
    origin::read(std::slice::from_ref(row)).map(|_| ())
}

pub(super) fn validate_receipt(row: &StorageRow) -> SemanticRevisionResult<()> {
    let receipt: imports::ImportReceipt = serde_json::from_str(&row.metadata_json)?;
    if row.key.identity != receipt.identity || row.body_json.is_some() {
        return Err(SemanticRevisionError::invalid_value(
            &row.key.identity,
            "exact archive acceptance receipt",
        ));
    }
    Ok(())
}
