//! Frozen import requests replay before consulting mutable revision bodies.
use super::*;
use crate::revision_index::digest;
use crate::validation::require_non_empty;
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportReceipt {
    pub identity: String,
    request_digest: String,
    outcome_json: String,
}

pub(super) fn decode(input: &str) -> SemanticRevisionResult<(ImportRequest, ArchivePayload)> {
    let request: ImportRequest = serde_json::from_str(input)?;
    if request.operation != "import_archive" || request.format_release != 1 {
        return Err(SemanticRevisionError::invalid_value(
            input,
            "import_archive with format_release 1",
        ));
    }
    require_non_empty(&request.operation_id, "a frozen import operation ID")?;
    let archive: ArchivePayload = serde_json::from_str(&request.archive_json)?;
    if archive.selection_id != "working" || archive.revision_state.is_none() {
        return Err(SemanticRevisionError::invalid_value(
            &archive.selection_id,
            "working archive with complete revision metadata",
        ));
    }
    Ok((request, archive))
}

pub(in crate::policy_bytes::storage) fn initial(
    input: &str,
    root: &str,
) -> SemanticRevisionResult<StorageResult> {
    let (_, archive) = decode(input)?;
    crate::revision_index::require_project_identity(&archive.project_id)?;
    let state = SemanticRevisionState {
        project_root: root.into(),
        project_id: archive.project_id.clone(),
        ..Default::default()
    };
    encoded(
        "archive_context",
        &serde_json::json!({
            "context":{"project_root":root,"project_id":archive.project_id,"commit_version":"0"},
            "state_json":serde_json::to_string(&state)?,
        }),
    )
}

pub(super) fn apply(
    before: &SemanticRevisionState,
    input: &str,
    rows: &[StorageRow],
    actor: Option<&str>,
    now: i64,
    next: i64,
) -> SemanticRevisionResult<StorageResult> {
    let (request, archive) = decode(input)?;
    let actor = actor.filter(|actor| !actor.is_empty()).ok_or_else(|| {
        SemanticRevisionError::invalid_value("absent actor", "an authenticated archive writer")
    })?;
    let identity = digest(&(actor, &request.operation_id))?;
    let request_digest = format!("{:x}", Sha256::digest(request.archive_json.as_bytes()));
    if let Some(replayed) = replay(before, rows, &identity, &request_digest)? {
        return Ok(replayed);
    }
    if next <= before.commit_version {
        return Err(SemanticRevisionError::invalid_value(
            next.to_string(),
            format!("next commit above {}", before.commit_version),
        ));
    }
    let incoming = validated_state(&archive, &before.project_root)?;
    let mut after = before.merge_archive(&incoming, now)?;
    after.snapshot("public", "")?;
    after.commit_version = next.max(after.commit_version.checked_add(1).ok_or_else(|| {
        SemanticRevisionError::invalid_value(
            after.commit_version.to_string(),
            "an incrementable archive commit",
        )
    })?);
    let outcome = outcome(&archive, before, &after);
    let receipt = ImportReceipt {
        identity,
        request_digest,
        outcome_json: outcome.to_string(),
    };
    import_delta(before, &after, &archive, receipt, rows)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptRequest {
    operation: String,
    operation_id: String,
    archive_digest: String,
}

pub(in crate::policy_bytes::storage) fn receipt_intent(
    input: &str,
    actor: Option<&str>,
) -> SemanticRevisionResult<StorageResult> {
    let (identity, _) = receipt_request(input, actor)?;
    encoded("intent", &read::ReadIntent::ArchiveReceipt { identity })
}

pub(in crate::policy_bytes::storage) fn receipt_read(
    input: &str,
    actor: Option<&str>,
    rows: &[StorageRow],
) -> SemanticRevisionResult<StorageResult> {
    let (identity, request) = receipt_request(input, actor)?;
    let receipt = rows
        .iter()
        .find(|row| is_receipt(row) && row.key.identity == identity)
        .map(|row| serde_json::from_str::<ImportReceipt>(&row.metadata_json))
        .transpose()?;
    if receipt
        .as_ref()
        .is_some_and(|receipt| receipt.request_digest != request.archive_digest)
    {
        return Err(SemanticRevisionError::invalid_value(
            identity,
            "the accepted archive digest",
        ));
    }
    encoded(
        "archive_receipt",
        &receipt
            .map(|receipt| serde_json::from_str::<serde_json::Value>(&receipt.outcome_json))
            .transpose()?,
    )
}

fn receipt_request(
    input: &str,
    actor: Option<&str>,
) -> SemanticRevisionResult<(String, ReceiptRequest)> {
    let request: ReceiptRequest = serde_json::from_str(input)?;
    let actor = actor.filter(|actor| !actor.is_empty()).ok_or_else(|| {
        SemanticRevisionError::invalid_value("absent actor", "an authenticated import owner")
    })?;
    if request.operation != "archive_receipt"
        || request.archive_digest.len() != 64
        || !request
            .archive_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(SemanticRevisionError::invalid_value(
            &request.archive_digest,
            "archive_receipt with a SHA-256 digest",
        ));
    }
    Ok((digest(&(actor, &request.operation_id))?, request))
}

fn replay(
    state: &SemanticRevisionState,
    rows: &[StorageRow],
    identity: &str,
    request_digest: &str,
) -> SemanticRevisionResult<Option<StorageResult>> {
    let Some(row) = rows
        .iter()
        .find(|row| is_receipt(row) && row.key.identity == identity)
    else {
        return Ok(None);
    };
    validate_receipt(row)?;
    let receipt: ImportReceipt = serde_json::from_str(&row.metadata_json)?;
    if receipt.request_digest != request_digest {
        return Err(SemanticRevisionError::invalid_value(
            identity,
            "identical bytes for an accepted import operation",
        ));
    }
    Ok(Some(StorageResult::Delta {
        expected_commit: state.commit_version.to_string(),
        commit_version: state.commit_version.to_string(),
        rows: vec![],
        removed: vec![],
        outcome_json: receipt.outcome_json,
    }))
}

fn validated_state(
    archive: &ArchivePayload,
    root: &str,
) -> SemanticRevisionResult<SemanticRevisionState> {
    let state = archive
        .revision_state
        .as_ref()
        .expect("decoded revision metadata");
    if state.project_id != archive.project_id || state.project_root != archive.snapshot.project_root
    {
        return Err(SemanticRevisionError::invalid_value(
            &archive.project_id,
            "archive and revision lineage/root agreement",
        ));
    }
    state.validate_archive_state()?;
    validate_blobs(archive, state)?;
    let elements = state
        .records
        .iter()
        .filter_map(|row| match &row.body {
            SemanticRevisionBody::Element(body) => Some(body.clone()),
            _ => None,
        })
        .collect();
    let artifacts = state
        .records
        .iter()
        .filter_map(|row| match &row.body {
            SemanticRevisionBody::Artifact(body) => Some(body.clone()),
            _ => None,
        })
        .collect();
    SemanticRevisionState::from_metadata_rows(
        root,
        state.commit_version,
        state.metadata_rows(),
        elements,
        artifacts,
    )
}

fn validate_blobs(
    archive: &ArchivePayload,
    state: &SemanticRevisionState,
) -> SemanticRevisionResult<()> {
    let mut references = BTreeSet::new();
    let retained_owners = state.retained_artifact_owner_ids();
    for blob in &archive.artifact_blobs {
        let primary = state.records.iter().find_map(|row| match &row.body {
            SemanticRevisionBody::Artifact(artifact)
                if artifact.artifact_id == blob.artifact_id =>
            {
                Some(artifact)
            }
            _ => None,
        });
        let owner = retained_owners.contains(&blob.artifact_id)
            && primary.is_none_or(|artifact| {
                artifact.content_ref.as_ref() != Some(&blob.content_ref)
                    || artifact.content_size_bytes == Some(blob.content.len())
            });
        if !owner
            || blob.content_ref.is_empty()
            || blob.media_type.is_empty()
            || !references.insert(&blob.content_ref)
        {
            return Err(SemanticRevisionError::invalid_value(
                &blob.content_ref,
                "one immutable blob with an exact retained artifact owner, media type and byte length",
            ));
        }
    }
    for row in &state.records {
        if let SemanticRevisionBody::Artifact(artifact) = &row.body {
            if artifact
                .content_ref
                .as_ref()
                .is_some_and(|reference| !references.contains(reference))
            {
                return Err(SemanticRevisionError::invalid_value(
                    &artifact.artifact_id,
                    "available exact bytes for each retained artifact",
                ));
            }
        }
    }
    Ok(())
}

fn import_delta(
    before: &SemanticRevisionState,
    after: &SemanticRevisionState,
    archive: &ArchivePayload,
    receipt: ImportReceipt,
    loaded: &[StorageRow],
) -> SemanticRevisionResult<StorageResult> {
    let mut delta = super::super::delta(
        before,
        after,
        &serde_json::from_str::<serde_json::Value>(&receipt.outcome_json)?,
    )?;
    let StorageResult::Delta { rows, removed, .. } = &mut delta else {
        unreachable!()
    };
    if before.commit_version == 0 {
        *rows = super::super::rows::encode(after)?;
    }
    let imported_vectors = vectors::archive::import(
        after,
        &archive.element_name_vectors,
        &archive.artifact_text_vectors,
        loaded,
    )?;
    removed.retain(|key| !imported_vectors.iter().any(|row| &row.key == key));
    rows.extend(imported_vectors);
    rows.extend(content::attachments::import(
        &archive.artifact_blobs,
        after,
        loaded,
    )?);
    if !loaded.iter().any(origin::is_origin) {
        rows.push(origin::row(archive)?);
    }
    rows.push(StorageRow {
        key: StorageRowKey {
            kind: "archive_receipt".into(),
            identity: receipt.identity.clone(),
        },
        metadata_json: serde_json::to_string(&receipt)?,
        body_json: None,
        indexes: vec![],
    });
    Ok(delta)
}

fn outcome(
    archive: &ArchivePayload,
    before: &SemanticRevisionState,
    after: &SemanticRevisionState,
) -> serde_json::Value {
    let incoming = archive.revision_state.as_ref().expect("validated metadata");
    let previous = before
        .records
        .iter()
        .filter(|row| !matches!(row.body, SemanticRevisionBody::Pruned))
        .map(|row| &row.record)
        .collect::<BTreeSet<_>>();
    let added = incoming
        .records
        .iter()
        .filter(|row| {
            !matches!(row.body, SemanticRevisionBody::Pruned) && !previous.contains(&row.record)
        })
        .collect::<Vec<_>>();
    let artifacts = added
        .iter()
        .filter(|row| row.record.kind == SemanticRecordKind::Artifact)
        .count();
    let selected = after
        .selection("working")
        .map(|head| head.records.iter().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let inactive = before
        .selection("working")
        .into_iter()
        .flat_map(|head| &head.records)
        .filter(|record| record.kind == SemanticRecordKind::Element && !selected.contains(record))
        .count();
    serde_json::json!({"projectId":archive.project_id,"snapshotId":archive.snapshot_id,"canonicalRemote":archive.canonical_remote,
        "artifactsImported":artifacts,"artifactsKept":incoming.records.iter().filter(|row| matches!(row.body,SemanticRevisionBody::Artifact(_))).count()-artifacts,
        "structure":{"elements_upserted":added.len()-artifacts,"relationships_upserted":after.selection("working").map_or(0,|head|head.connection_ids.len()),"identities_reused":incoming.records.len()-added.len(),"elements_marked_inactive":inactive}})
}
