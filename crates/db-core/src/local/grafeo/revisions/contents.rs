//! Revision content preparation stays side-effect free; publication stages it in the graph/SQL fence.
use crate::local::sql::artifact_blobs::ArtifactBlobRepository;
use crate::local::sql::artifact_blobs::{put_archived_blob, require_stored_blob_references};
use crate::*;
use grafeo::{GrafeoDB, Value};
use std::collections::BTreeMap;

impl super::super::semantic_storage::SemanticStorage<'_> {
    pub(super) fn released_attachment_contents(
        &self,
        graph: &GrafeoDB,
        before: &SemanticRevisionState,
        after: &SemanticRevisionState,
    ) -> Result<Vec<String>> {
        let previous = before.retained_artifact_owner_ids();
        let retained = after.retained_artifact_owner_ids();
        let owners = previous
            .difference(&retained)
            .map(String::as_str)
            .collect::<Vec<_>>();
        let repository = ArtifactBlobRepository::with_clock(self.conn, self.clock.clone());
        let kept = artifact_content_refs(&after.records);
        let replaced = artifact_content_refs(&before.records);
        Ok(repository
            .blobs_for_artifacts(&owners)?
            .into_iter()
            .filter(|blob| {
                !kept
                    .values()
                    .any(|reference| *reference == Some(blob.content_ref.as_str()))
                    && !has_retained_content_reference(graph, &blob.content_ref, &replaced)
            })
            .map(|blob| blob.content_ref)
            .collect())
    }
}

pub(super) fn released_revision_contents(
    graph: &GrafeoDB,
    before: &SemanticRevisionState,
    plan: &SemanticRevisionPublicationPlan,
) -> Vec<String> {
    let next = artifact_content_refs(&plan.records);
    let candidates = artifact_content_refs(&before.records)
        .into_iter()
        .filter(|(id, reference)| next.get(id).is_some_and(|new| new != reference))
        .filter_map(|(_, reference)| reference)
        .collect::<std::collections::BTreeSet<_>>();
    candidates
        .into_iter()
        .filter(|reference| {
            reference.starts_with("revision-blob:")
                && !next.values().any(|new| *new == Some(*reference))
                && !has_retained_content_reference(graph, reference, &next)
        })
        .map(str::to_owned)
        .collect()
}

fn artifact_content_refs(records: &[SemanticStoredRevision]) -> BTreeMap<&str, Option<&str>> {
    records
        .iter()
        .filter_map(|stored| match &stored.body {
            SemanticRevisionBody::Artifact(artifact) => Some((
                artifact.artifact_id.as_str(),
                artifact.content_ref.as_deref(),
            )),
            _ => None,
        })
        .collect()
}

fn has_retained_content_reference(
    graph: &GrafeoDB,
    reference: &str,
    replaced: &BTreeMap<&str, Option<&str>>,
) -> bool {
    graph
        .find_nodes_by_property("content_ref", &Value::from(reference))
        .into_iter()
        .filter_map(|id| graph.get_node(id))
        .filter(|node| node.has_label("SemanticArtifact"))
        .any(|node| match node.get_property("artifact_id") {
            Some(Value::String(id)) => !replaced.contains_key::<str>(id.as_ref()),
            _ => true,
        })
}

pub(super) fn remove_revision_contents(
    conn: &rusqlite::Connection,
    references: &[String],
) -> Result<()> {
    for reference in references {
        conn.execute(
            "DELETE FROM artifact_blobs WHERE content_ref = ?1",
            [reference],
        )?;
    }
    Ok(())
}

pub(super) fn prepare_revision_contents(
    request: &SemanticRevisionPublication,
    plan: &SemanticRevisionPublicationPlan,
    timestamp: &str,
) -> Result<Vec<ArtifactBlob>> {
    let Some(operation) = &plan.accepted_operation else {
        return Ok(vec![]);
    };
    let accepted = accepted_artifact_ids(&operation.receipt);
    let mut contents = vec![];
    for write in &request.artifacts {
        let Some(body) = write.artifact.content.as_deref() else {
            continue;
        };
        let id = accepted_artifact_id(&accepted, &write.artifact.artifact_id)?;
        let blob = inline_revision_blob(write, body, &request.project_id, id, timestamp)?;
        contents.push(blob);
    }
    Ok(contents)
}

fn accepted_artifact_ids(receipt: &SemanticRevisionPublicationReceipt) -> BTreeMap<&str, &str> {
    receipt
        .remaps
        .iter()
        .filter(|remap| remap.incoming.kind == SemanticRecordKind::Artifact)
        .map(|remap| {
            (
                remap.incoming.record_id.as_str(),
                remap.accepted.record_id.as_str(),
            )
        })
        .collect()
}

fn accepted_artifact_id<'a>(accepted: &BTreeMap<&'a str, &'a str>, id: &str) -> Result<&'a str> {
    accepted.get(id).copied().ok_or_else(|| {
        DbError::invalid_value(
            id,
            "an accepted artifact revision in the publication receipt",
        )
    })
}

fn inline_revision_blob(
    write: &SemanticArtifactRevisionWrite,
    body: &str,
    project_id: &str,
    accepted_id: &str,
    timestamp: &str,
) -> Result<ArtifactBlob> {
    let stored = write.stored_artifact(project_id)?;
    Ok(ArtifactBlob {
        content_ref: stored.content_ref.ok_or_else(|| {
            DbError::invalid_value(
                &stored.artifact_id,
                "the deterministic inline revision content reference",
            )
        })?,
        artifact_id: accepted_id.into(),
        media_type: write.media_type.clone(),
        content: body.as_bytes().to_vec(),
        updated_at: timestamp.into(),
    })
}

pub(super) fn stage_revision_contents(
    conn: &rusqlite::Connection,
    contents: &[ArtifactBlob],
    records: &[SemanticStoredRevision],
) -> Result<()> {
    for blob in contents {
        put_archived_blob(conn, blob)?;
    }
    let references = records.iter().filter_map(|stored| match &stored.body {
        SemanticRevisionBody::Artifact(artifact) => artifact.content_ref.as_deref(),
        _ => None,
    });
    require_stored_blob_references(conn, references)
}
