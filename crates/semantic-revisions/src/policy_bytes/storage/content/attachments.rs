//! Non-primary binary attachments retain their exact artifact owner through export and collection.
use super::*;
use lumvise_contracts::ArtifactBlob;
use std::collections::BTreeSet;

pub(in crate::policy_bytes::storage) fn is_attachment(row: &StorageRow) -> bool {
    row.key.kind == "artifact_blob"
}

pub(in crate::policy_bytes::storage) fn decode(
    row: &StorageRow,
) -> SemanticRevisionResult<ArtifactBlob> {
    let body = row.body_json.as_deref().ok_or_else(|| {
        SemanticRevisionError::invalid_value(&row.key.identity, "available attachment bytes")
    })?;
    let blob: ArtifactBlob = serde_json::from_str(body)?;
    if !is_attachment(row)
        || row.key.identity != blob.content_ref
        || row.metadata_json != serde_json::to_string(&blob.artifact_id)?
    {
        return Err(SemanticRevisionError::invalid_value(
            &row.key.identity,
            "exact attachment owner and reference",
        ));
    }
    Ok(blob)
}

pub(in crate::policy_bytes::storage) fn row(
    blob: &ArtifactBlob,
) -> SemanticRevisionResult<StorageRow> {
    Ok(StorageRow {
        key: StorageRowKey {
            kind: "artifact_blob".into(),
            identity: blob.content_ref.clone(),
        },
        metadata_json: serde_json::to_string(&blob.artifact_id)?,
        body_json: Some(serde_json::to_string(blob)?),
        indexes: vec![super::super::indexes::StorageIndex {
            name: "blob_owner".into(),
            value: blob.artifact_id.clone(),
        }],
    })
}

pub(super) fn append(
    rows: &[StorageRow],
    contents: &mut BTreeMap<String, StoredContent>,
) -> SemanticRevisionResult<()> {
    for row in rows.iter().filter(|row| is_attachment(row)) {
        let blob = decode(row)?;
        insert(
            contents.entry(blob.content_ref.clone()),
            StoredContent {
                content_ref: blob.content_ref,
                media_type: blob.media_type,
                bytes: blob.content,
                updated_at: Some(blob.updated_at),
            },
        )?;
    }
    Ok(())
}

pub(in crate::policy_bytes::storage) fn export(
    rows: &[StorageRow],
    owner: &str,
) -> SemanticRevisionResult<Vec<ArtifactBlob>> {
    rows.iter()
        .filter(|row| is_attachment(row))
        .map(decode)
        .filter_map(|result| match result {
            Ok(blob) if blob.artifact_id == owner => Some(Ok(blob)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub(super) fn retained_blob(
    state: &SemanticRevisionState,
    rows: &[StorageRow],
    reference: &str,
) -> SemanticRevisionResult<Option<ArtifactBlob>> {
    let Some(row) = rows
        .iter()
        .find(|row| is_attachment(row) && row.key.identity == reference)
    else {
        return Ok(None);
    };
    let blob = decode(row)?;
    let owner = SemanticRecordRef::artifact(&blob.artifact_id);
    Ok(state
        .records
        .iter()
        .any(|record| record.record == owner)
        .then_some(blob))
}

pub(in crate::policy_bytes::storage) fn import(
    blobs: &[ArtifactBlob],
    state: &SemanticRevisionState,
    loaded: &[StorageRow],
) -> SemanticRevisionResult<Vec<StorageRow>> {
    let mut result = Vec::new();
    for blob in blobs {
        let primary = state.records.iter().any(|record|matches!(&record.body,SemanticRevisionBody::Artifact(artifact) if artifact.artifact_id == blob.artifact_id && artifact.content_ref.as_ref()==Some(&blob.content_ref)));
        if primary {
            continue;
        }
        let incoming = row(blob)?;
        if let Some(old) = loaded.iter().find(|row| row.key == incoming.key) {
            let old = decode(old)?;
            if old.artifact_id != blob.artifact_id
                || old.media_type != blob.media_type
                || old.content != blob.content
            {
                return Err(SemanticRevisionError::invalid_value(
                    &blob.content_ref,
                    "immutable exact attachment owner, media type and bytes",
                ));
            }
            continue;
        }
        result.push(incoming);
    }
    Ok(result)
}

pub(super) fn finish(
    delta: &mut StorageResult,
    rows: &[StorageRow],
    request: Option<&str>,
    actor_blobs: Vec<ArtifactBlob>,
) -> SemanticRevisionResult<()> {
    let StorageResult::Delta {
        rows: changes,
        removed,
        commit_version,
        expected_commit,
        ..
    } = delta
    else {
        unreachable!()
    };
    if commit_version == expected_commit {
        return Ok(());
    }
    for blob in actor_blobs {
        let primary = changes
            .iter()
            .filter_map(|row| row.body_json.as_ref())
            .filter_map(|body| decode_primary(body).ok())
            .any(|artifact| artifact.content_ref.as_ref() == Some(&blob.content_ref));
        if !primary {
            changes.push(row(&blob)?);
        }
        let key = StorageRowKey {
            kind: "staged_blob".into(),
            identity: blob.content_ref,
        };
        if !removed.contains(&key) {
            removed.push(key);
        }
    }
    let input = request
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()?;
    if let Some(input) = input.filter(|input| input["operation"] == "collect") {
        let plan: SemanticRevisionRetentionPlan = serde_json::from_value(input["plan"].clone())?;
        let retained = collected_owners(rows, changes, removed, &plan, commit_version)?;
        for row in rows.iter().filter(|row| is_attachment(row)) {
            let blob = decode(row)?;
            if !retained.contains(&blob.artifact_id) {
                removed.push(row.key.clone());
            }
        }
    }
    Ok(())
}

fn decode_primary(body: &str) -> SemanticRevisionResult<SemanticArtifact> {
    match super::decode(body)?.record {
        SemanticRevisionBody::Artifact(artifact) => Ok(artifact),
        _ => Err(SemanticRevisionError::invalid_value("element", "artifact")),
    }
}

fn collected_owners(
    previous: &[StorageRow],
    changes: &[StorageRow],
    removed: &[StorageRowKey],
    plan: &SemanticRevisionRetentionPlan,
    commit: &str,
) -> SemanticRevisionResult<BTreeSet<String>> {
    let mut current = super::super::rows::index(previous.to_vec());
    for key in removed {
        current.remove(key);
    }
    for row in changes {
        current.insert(row.key.clone(), row.clone());
    }
    let after = super::super::rows::decode(
        &plan.project_root,
        &plan.project_id,
        super::super::integer(commit)?,
        current.into_values().collect(),
    )?;
    Ok(after.retained_artifact_owner_ids())
}
