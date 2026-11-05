//! Native-only projection: complete archive validation precedes lossless, opaque storage pages.
use super::*;

pub(super) fn encode(input: &str) -> SemanticRevisionResult<Vec<String>> {
    let archive: ArchivePayload = serde_json::from_str(input)?;
    let state = imports::validated_state(&archive, &archive.snapshot.project_root)?;
    let header = header(&archive, &state);
    TransferHeader::parse(&serde_json::to_string(&header)?)?;
    let mut rows = super::super::super::rows::encode(&state)?;
    complete_bodies(&mut rows, &archive)?;
    rows.extend(vectors::archive::import(
        &state,
        &archive.element_name_vectors,
        &archive.artifact_text_vectors,
        &[],
    )?);
    rows.extend(content::attachments::import(
        &archive.artifact_blobs,
        &state,
        &[],
    )?);
    rows.push(origin::row(&archive)?);
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    pages(header, rows)
}

fn header(archive: &ArchivePayload, state: &SemanticRevisionState) -> TransferHeader {
    TransferHeader {
        format_release: 2,
        project_id: archive.project_id.clone(),
        source_root: state.project_root.clone(),
        archive_commit: state.commit_version.to_string(),
        selection_id: archive.selection_id.clone(),
        snapshot_id: archive.snapshot_id.clone(),
        display_name: archive.display_name.clone(),
        source_revision: archive.source_revision.clone(),
        canonical_remote: archive.canonical_remote.clone(),
    }
}

fn complete_bodies(
    rows: &mut [StorageRow],
    archive: &ArchivePayload,
) -> SemanticRevisionResult<()> {
    let contents = archive
        .artifact_blobs
        .iter()
        .map(|blob| {
            (
                blob.content_ref.clone(),
                content::StoredContent {
                    content_ref: blob.content_ref.clone(),
                    media_type: blob.media_type.clone(),
                    bytes: blob.content.clone(),
                    updated_at: Some(blob.updated_at.clone()),
                },
            )
        })
        .collect();
    for row in rows.iter_mut().filter(|row| row.key.kind == "record") {
        content::complete_record(row, &contents)?;
    }
    Ok(())
}

fn pages(header: TransferHeader, rows: Vec<StorageRow>) -> SemanticRevisionResult<Vec<String>> {
    let mut result = Vec::new();
    let mut page = TransferPage {
        format_release: 2,
        header: Some(header),
        rows: vec![],
    };
    let mut bytes = serde_json::to_vec(&page)?.len();
    for row in rows {
        let size = serde_json::to_vec(&row)?.len() + 1;
        if !page.rows.is_empty() && bytes.saturating_add(size) > PAGE_BYTES {
            result.push(serde_json::to_string(&page)?);
            page = TransferPage {
                format_release: 2,
                header: None,
                rows: vec![],
            };
            bytes = serde_json::to_vec(&page)?.len();
        }
        bytes = bytes.saturating_add(size);
        page.rows.push(row);
    }
    result.push(serde_json::to_string(&page)?);
    Ok(result)
}
