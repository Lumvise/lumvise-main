//! One row's canonical codec reuses the ordinary header, content, index and vector owners.
use super::*;

pub(super) fn source(row: StorageRow, root: &str) -> SemanticRevisionResult<StorageRow> {
    if content::attachments::is_attachment(&row) {
        let blob = content::attachments::decode(&row)?;
        if blob.content_ref.is_empty() || blob.media_type.is_empty() {
            return Err(invalid(
                &blob.content_ref,
                "an immutable blob reference and media type",
            ));
        }
        return content::attachments::row(&blob);
    }
    if vectors::is_vector(&row) {
        return vectors::archive::canonical(&row, root);
    }
    if origin::is_origin(&row) {
        origin::read(std::slice::from_ref(&row))?;
        return Ok(StorageRow {
            indexes: vec![],
            ..row
        });
    }
    let metadata = metadata(&row)?;
    match &metadata {
        SemanticRevisionMetadata::Record(_) => record(&row, root).map(|_| ())?,
        SemanticRevisionMetadata::Connection(edge) => {
            edge.relationship.validate()?;
            crate::revision_index::ensure_root(&edge.relationship.project_root, root)?;
            if row.body_json.as_deref() != Some(&row.metadata_json) {
                return Err(invalid(&row.key.identity, "an exact connection body"));
            }
        }
        _ if row.body_json.is_some() => {
            return Err(invalid(&row.key.kind, "body-free revision metadata"));
        }
        _ => {}
    }
    let body = row
        .body_json
        .as_deref()
        .filter(|_| row.key.kind == "record")
        .map(content::decode)
        .transpose()?;
    if let Some(content) = body.as_ref().and_then(|body| body.content.as_ref()) {
        if content.content_ref.is_empty() || content.media_type.is_empty() {
            return Err(invalid(
                &content.content_ref,
                "an immutable primary blob reference and media type",
            ));
        }
    }
    let metadata_json = serde_json::to_string(&metadata)?;
    let body_json = if row.key.kind == "connection" {
        Some(metadata_json.clone())
    } else {
        body.as_ref().map(serde_json::to_string).transpose()?
    };
    Ok(StorageRow {
        indexes: super::super::super::indexes::entries(
            &metadata,
            body.as_ref().map(|b| &b.record),
        )?,
        metadata_json,
        body_json,
        ..row
    })
}

pub(super) fn metadata(row: &StorageRow) -> SemanticRevisionResult<SemanticRevisionMetadata> {
    let metadata: SemanticRevisionMetadata = serde_json::from_str(&row.metadata_json)?;
    if metadata.kind() != row.key.kind || metadata.identity()? != row.key.identity {
        return Err(invalid(
            &row.key.identity,
            "metadata matching its exact indexed identity",
        ));
    }
    Ok(metadata)
}

pub(super) fn record(
    row: &StorageRow,
    root: &str,
) -> SemanticRevisionResult<SemanticStoredRevision> {
    let SemanticRevisionMetadata::Record(header) = metadata(row)? else {
        return Err(invalid(&row.key.kind, "a revision record header"));
    };
    let body = row
        .body_json
        .as_deref()
        .map(content::decode)
        .transpose()?
        .map(|value| value.record)
        .or_else(|| header.payload_absence())
        .ok_or_else(|| {
            invalid(
                &row.key.identity,
                "an available body or explicit unavailable/pruned header",
            )
        })?;
    if let SemanticRevisionBody::Element(element) = &body {
        if header.canonical_lifecycle.as_deref() != Some(&element.lifecycle) {
            return Err(invalid(
                &row.key.identity,
                "the canonical element lifecycle matching its exact body",
            ));
        }
    }
    let stored = header.restore(body)?;
    crate::metadata::validate_archive_record(root, &stored)?;
    Ok(stored)
}

pub(super) fn rebind(row: &StorageRow, root: &str) -> SemanticRevisionResult<StorageRow> {
    if vectors::is_vector(row) {
        return vectors::archive::canonical(row, root);
    }
    if content::attachments::is_attachment(row) || origin::is_origin(row) {
        return Ok(row.clone());
    }
    let mut metadata = metadata(row)?;
    crate::metadata::set_owned_root(&mut metadata, root);
    let mut rebound = StorageRow {
        metadata_json: serde_json::to_string(&metadata)?,
        ..row.clone()
    };
    if let Some(body) = &row.body_json {
        if row.key.kind == "connection" {
            rebound.body_json = Some(rebound.metadata_json.clone());
        } else {
            let mut body = content::decode(body)?;
            if let SemanticRevisionBody::Element(element) = &mut body.record {
                element.project_root = root.into();
            }
            rebound.body_json = Some(serde_json::to_string(&body)?);
        }
    }
    source(rebound, root)
}

pub(super) fn encode_record(
    stored: &SemanticStoredRevision,
    original: &StorageRow,
) -> SemanticRevisionResult<StorageRow> {
    let metadata = SemanticRevisionMetadata::Record(SemanticRevisionRecordHeader::from(stored));
    let body_json = if stored.body.is_available() {
        let content = original
            .body_json
            .as_deref()
            .map(content::decode)
            .transpose()?
            .and_then(|body| body.content);
        Some(serde_json::to_string(&content::StoredBody {
            record: stored.body.clone(),
            content,
        })?)
    } else {
        None
    };
    Ok(StorageRow {
        key: original.key.clone(),
        metadata_json: serde_json::to_string(&metadata)?,
        body_json,
        indexes: super::super::super::indexes::entries(&metadata, Some(&stored.body))?,
    })
}
