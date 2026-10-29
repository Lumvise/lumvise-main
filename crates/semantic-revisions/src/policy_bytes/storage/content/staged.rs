//! Writer-owned bytes await their exact artifact publication; public reads require an accepted record.
use super::*;
use crate::validation::require_non_empty;
use lumvise_contracts::ArtifactBlob;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::policy_bytes::storage) struct BlobPut {
    operation: String,
    pub content_ref: String,
    pub artifact_id: String,
    media_type: String,
    content: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StagedBlob {
    actor: String,
    blob: ArtifactBlob,
}

pub(in crate::policy_bytes::storage) fn is_staged(row: &StorageRow) -> bool {
    row.key.kind == "staged_blob"
}

pub(in crate::policy_bytes::storage) fn validate(row: &StorageRow) -> SemanticRevisionResult<()> {
    decode(row).map(|_| ())
}

fn decode(row: &StorageRow) -> SemanticRevisionResult<StagedBlob> {
    let stored: StagedBlob = serde_json::from_str(row.body_json.as_deref().ok_or_else(|| {
        SemanticRevisionError::invalid_value(&row.key.identity, "available staged blob")
    })?)?;
    if !is_staged(row)
        || stored.blob.content_ref != row.key.identity
        || row.metadata_json
            != serde_json::to_string(&(stored.actor.clone(), stored.blob.artifact_id.clone()))?
    {
        return Err(SemanticRevisionError::invalid_value(
            &row.key.identity,
            "exact staged blob owner/reference",
        ));
    }
    Ok(stored)
}

pub(in crate::policy_bytes::storage) fn put(
    state: &SemanticRevisionState,
    input: &str,
    rows: &[StorageRow],
    actor: Option<&str>,
    time: &SemanticRevisionTimestamp,
    next: i64,
) -> SemanticRevisionResult<StorageResult> {
    let request: BlobPut = serde_json::from_str(input)?;
    let actor = actor.ok_or_else(|| {
        SemanticRevisionError::invalid_value("anonymous", "an authenticated blob writer")
    })?;
    for value in [
        &request.content_ref,
        &request.artifact_id,
        &request.media_type,
        actor,
    ] {
        require_non_empty(
            value,
            "a nonempty blob reference, artifact, media type and actor",
        )?;
    }
    if request.operation != "blob_put" || next <= state.commit_version {
        return Err(SemanticRevisionError::invalid_value(
            input,
            "blob_put with an advancing commit",
        ));
    }
    let blob = ArtifactBlob {
        content_ref: request.content_ref,
        artifact_id: request.artifact_id,
        media_type: request.media_type,
        content: request.content,
        updated_at: time.rfc3339.clone(),
    };
    if let Some(existing) = existing(state, rows, actor, &blob)? {
        return result(state, state.commit_version, vec![], &existing);
    }
    let stored = StagedBlob {
        actor: actor.into(),
        blob: blob.clone(),
    };
    let row = if state.records.iter().any(|record|matches!(&record.body,SemanticRevisionBody::Artifact(artifact) if artifact.artifact_id == blob.artifact_id) && record.artifact_state.as_ref().is_some_and(|edit|!edit.frozen)) {
        attachments::row(&blob)?
    } else {
        StorageRow { key: StorageRowKey {kind:"staged_blob".into(),identity:blob.content_ref.clone()}, metadata_json:serde_json::to_string(&(actor,&blob.artifact_id))?, body_json:Some(serde_json::to_string(&stored)?),indexes:vec![super::super::indexes::StorageIndex {name:"staged_blob_owner".into(),value:blob.artifact_id.clone()}] }
    };
    result(state, next, vec![row], &blob)
}

fn existing(
    state: &SemanticRevisionState,
    rows: &[StorageRow],
    actor: &str,
    blob: &ArtifactBlob,
) -> SemanticRevisionResult<Option<ArtifactBlob>> {
    if let Some(row) = rows
        .iter()
        .find(|row| attachments::is_attachment(row) && row.key.identity == blob.content_ref)
    {
        let old = attachments::decode(row)?;
        if old.artifact_id != blob.artifact_id {
            return Err(SemanticRevisionError::invalid_value(
                &blob.artifact_id,
                "the exact attachment owner",
            ));
        }
        same_bytes(&old, blob)?;
        return Ok(Some(old));
    }
    if let Some(row) = rows
        .iter()
        .find(|row| is_staged(row) && row.key.identity == blob.content_ref)
    {
        let stored = decode(row)?;
        if stored.actor != actor || stored.blob.artifact_id != blob.artifact_id {
            return Err(SemanticRevisionError::invalid_value(
                &blob.content_ref,
                "the original staged actor and exact artifact",
            ));
        }
        same_bytes(&stored.blob, blob)?;
        return Ok(Some(stored.blob));
    }
    let contents = available(rows)?;
    let Some(stored) = contents.get(&blob.content_ref) else {
        return Ok(None);
    };
    if !state.records.iter().any(|row| matches!(&row.body,SemanticRevisionBody::Artifact(artifact) if artifact.artifact_id == blob.artifact_id && artifact.content_ref.as_ref() == Some(&blob.content_ref))) {
        return Err(SemanticRevisionError::invalid_value(&blob.artifact_id, "the exact committed blob owner"));
    }
    if stored.bytes != blob.content || stored.media_type != blob.media_type {
        return Err(SemanticRevisionError::invalid_value(
            &blob.content_ref,
            "immutable bytes and media type",
        ));
    }
    Ok(Some(blob.clone()))
}

fn same_bytes(stored: &ArtifactBlob, incoming: &ArtifactBlob) -> SemanticRevisionResult<()> {
    if stored.content != incoming.content || stored.media_type != incoming.media_type {
        return Err(SemanticRevisionError::invalid_value(
            &incoming.content_ref,
            "immutable bytes and media type",
        ));
    }
    Ok(())
}

fn result(
    state: &SemanticRevisionState,
    next: i64,
    rows: Vec<StorageRow>,
    blob: &ArtifactBlob,
) -> SemanticRevisionResult<StorageResult> {
    Ok(StorageResult::Delta {
        expected_commit: state.commit_version.to_string(),
        commit_version: next.to_string(),
        rows,
        removed: vec![],
        outcome_json: serde_json::to_string(blob)?,
    })
}

pub(in crate::policy_bytes::storage) fn claim(
    state: &SemanticRevisionState,
    input: &str,
    actor: Option<&str>,
    rows: &[StorageRow],
) -> SemanticRevisionResult<()> {
    let request: serde_json::Value = serde_json::from_str(input)?;
    let publication: SemanticRevisionPublication =
        serde_json::from_value(request["request"].clone())?;
    if state
        .accepted_operations
        .iter()
        .any(|operation| operation.receipt.operation_id == publication.operation_id)
    {
        return Ok(());
    }
    for write in publication.artifacts {
        let Some(reference) = &write.artifact.content_ref else {
            continue;
        };
        let Some(row) = rows
            .iter()
            .find(|row| is_staged(row) && &row.key.identity == reference)
        else {
            continue;
        };
        let stored = decode(row)?;
        if Some(stored.actor.as_str()) != actor
            || stored.blob.artifact_id != write.artifact.artifact_id
        {
            return Err(SemanticRevisionError::invalid_value(
                reference,
                "staged bytes owned by this writer and incoming artifact",
            ));
        }
    }
    for row in rows.iter().filter(|row| is_staged(row)) {
        if Some(decode(row)?.actor.as_str()) != actor {
            return Err(SemanticRevisionError::invalid_value(
                &row.key.identity,
                "pending attachments owned by this writer",
            ));
        }
    }
    Ok(())
}

pub(super) fn append(
    rows: &[StorageRow],
    contents: &mut BTreeMap<String, StoredContent>,
) -> SemanticRevisionResult<()> {
    for row in rows.iter().filter(|row| is_staged(row)) {
        let blob = decode(row)?.blob;
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

pub(super) fn claimed(
    delta: &StorageResult,
    rows: &[StorageRow],
    request: Option<&str>,
) -> SemanticRevisionResult<Vec<ArtifactBlob>> {
    let Some(input) = request else {
        return Ok(vec![]);
    };
    let input: serde_json::Value = serde_json::from_str(input)?;
    if input["operation"] != "publication" {
        return Ok(vec![]);
    }
    let publication: SemanticRevisionPublication =
        serde_json::from_value(input["request"].clone())?;
    let StorageResult::Delta {
        outcome_json,
        commit_version,
        expected_commit,
        ..
    } = delta
    else {
        unreachable!()
    };
    if commit_version == expected_commit {
        return Ok(vec![]);
    }
    let SemanticRevisionPublicationOutcome::Accepted(receipt) = serde_json::from_str(outcome_json)?
    else {
        return Ok(vec![]);
    };
    let mut claimed = Vec::new();
    for row in rows.iter().filter(|row| is_staged(row)) {
        let mut blob = decode(row)?.blob;
        if !publication
            .artifacts
            .iter()
            .any(|write| write.artifact.artifact_id == blob.artifact_id)
        {
            continue;
        }
        if let Some(remap) = receipt
            .remaps
            .iter()
            .find(|remap| remap.incoming == SemanticRecordRef::artifact(&blob.artifact_id))
        {
            blob.artifact_id = remap.accepted.record_id.clone();
        }
        claimed.push(blob);
    }
    Ok(claimed)
}

pub(super) fn read(
    rows: &[StorageRow],
    reference: &str,
    actor: Option<&str>,
) -> SemanticRevisionResult<Option<ArtifactBlob>> {
    rows.iter()
        .find(|row| is_staged(row) && row.key.identity == reference)
        .map(|row| {
            let stored = decode(row)?;
            Ok((Some(stored.actor.as_str()) == actor).then_some(stored.blob))
        })
        .transpose()
        .map(Option::flatten)
}
