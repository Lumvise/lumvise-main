//! Archive body/metadata projection belongs to the existing PZ codec, independently of storage engines.
use super::*;
use crate::{
    SemanticArtifact, SemanticRevisionBody, SemanticRevisionMetadata, SemanticRevisionState,
};
use std::borrow::Cow;

pub(super) fn archive_revision_state(
    archive: &SemanticArchive,
) -> Result<Cow<'_, SemanticRevisionState>> {
    if let Some(state) = &archive.revision_state {
        validate_archive_project_identity(&archive.project_id, state)?;
        return Ok(Cow::Borrowed(state));
    }
    let timestamp =
        chrono::DateTime::parse_from_rfc3339(&archive.snapshot.published_at).map_err(|_| {
            DbError::invalid_value(
                &archive.snapshot.published_at,
                "an RFC3339 archive publication time",
            )
        })?;
    Ok(Cow::Owned(SemanticRevisionState::adopt(
        &archive.snapshot.project_root,
        &archive.project_id,
        &archive.selection_id,
        "local",
        archive.snapshot.elements.clone(),
        archive.snapshot.artifacts.clone(),
        internal_relationships(&archive.snapshot),
        archive.snapshot.commit_version,
        timestamp.timestamp_millis(),
    )?))
}

pub(super) fn validate_archive_project_identity(
    project_id: &str,
    state: &SemanticRevisionState,
) -> Result<()> {
    if state.project_id != project_id {
        return Err(DbError::invalid_value(
            &state.project_id,
            format!("archive project identity {project_id}"),
        ));
    }
    Ok(())
}

pub(super) fn validate_archive_blobs(
    state: &SemanticRevisionState,
    blobs: &[crate::ArtifactBlob],
) -> Result<()> {
    let mut references = BTreeSet::new();
    for blob in blobs {
        if blob.content_ref.trim().is_empty()
            || blob.artifact_id.trim().is_empty()
            || blob.media_type.trim().is_empty()
            || !references.insert(blob.content_ref.as_str())
        {
            return Err(DbError::invalid_value(
                &blob.content_ref,
                "one archived blob per non-empty reference with an owner and media type",
            ));
        }
    }
    for stored in &state.records {
        if let SemanticRevisionBody::Artifact(body) = &stored.body {
            if let Some(reference) = &body.content_ref {
                if !references.contains(reference.as_str()) {
                    return Err(DbError::invalid_value(
                        reference,
                        "exact archived bytes for every retained artifact body",
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn archive_body_snapshot(
    archive: &SemanticArchive,
    state: &SemanticRevisionState,
) -> Result<SemanticProjectSnapshot> {
    let selected = state
        .snapshot(&archive.selection_id, &archive.snapshot.published_at)?
        .ok_or_else(|| {
            DbError::invalid_value(&archive.selection_id, "an available selected archive head")
        })?;
    let mut elements = vec![];
    let mut artifacts = vec![];
    for stored in &state.records {
        match &stored.body {
            SemanticRevisionBody::Element(element) => elements.push(element.clone()),
            SemanticRevisionBody::Artifact(artifact) => artifacts.push(artifact.clone()),
            SemanticRevisionBody::Pruned => {}
        }
    }
    let ids = elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut relationships = selected
        .connections
        .into_iter()
        .map(|connection| connection.relationship)
        .collect::<Vec<_>>();
    relationships.extend(
        archive
            .snapshot
            .relationships
            .iter()
            .filter(|relationship| !ids.contains(relationship.target_element_id.as_str()))
            .cloned(),
    );
    Ok(SemanticProjectSnapshot {
        project_root: state.project_root.clone(),
        commit_version: state.commit_version,
        published_at: archive.snapshot.published_at.clone(),
        elements,
        artifacts,
        relationships,
    })
}

fn internal_relationships(snapshot: &SemanticProjectSnapshot) -> Vec<crate::SemanticRelationship> {
    let ids = snapshot
        .elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<BTreeSet<_>>();
    snapshot
        .relationships
        .iter()
        .filter(|relationship| {
            ids.contains(relationship.source_element_id.as_str())
                && ids.contains(relationship.target_element_id.as_str())
        })
        .cloned()
        .collect()
}

pub(super) fn revision_metadata_table(state: &SemanticRevisionState) -> Result<TableBytes> {
    let mut rows = state
        .archive_metadata_rows()
        .into_iter()
        .map(|mut row| {
            if let SemanticRevisionMetadata::Record(header) = &mut row {
                if let Some(scope) = &mut header.element_scope {
                    scope.path = root_relative_path(&scope.path, &state.project_root)?;
                }
            }
            Ok(vec![
                Some(row.kind().into()),
                Some(row.identity()?),
                Some(serde_json::to_string(&row)?),
            ])
        })
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|left, right| left[0].cmp(&right[0]).then(left[1].cmp(&right[1])));
    let (_, schema_id, columns) = TABLE_SCHEMAS
        .iter()
        .find(|(name, _, _)| *name == "revision_metadata.parquet")
        .expect("the versioned archive schema owns its metadata table");
    Ok(TableBytes {
        name: "revision_metadata.parquet",
        schema_id,
        rows: rows.len() as u64,
        bytes: encode_table(columns, rows.len(), &rows)?,
    })
}

pub(super) fn restore_revision_state(
    root: &str,
    commit: i64,
    metadata: Vec<SemanticRevisionMetadata>,
    elements: Vec<SemanticElement>,
    artifacts: Vec<SemanticArtifact>,
) -> Result<SemanticRevisionState> {
    Ok(SemanticRevisionState::from_metadata_rows(
        root, commit, metadata, elements, artifacts,
    )?)
}
