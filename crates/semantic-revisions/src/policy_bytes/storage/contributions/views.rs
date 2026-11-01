//! Indexed active contribution heads keep pending views separate from public graph reads.
use super::super::read::ReadPlan;
use super::*;
use std::collections::BTreeSet;

pub(in crate::policy_bytes::storage) fn request(
    plan: &mut ReadPlan,
    owners: &[String],
) -> SemanticRevisionResult<()> {
    let sources = anchors(plan, owners)?;
    for source in &sources {
        plan.index("challenge_source", source, None);
    }
    for head in loaded(plan, "selection")? {
        if let SemanticRevisionMetadata::Selection(head) = head {
            if source(&head.source_context)?.is_some_and(|owner| sources.contains(&owner)) {
                plan.selection(&head.selection_id, true);
            }
        }
    }
    Ok(())
}

/// The asked-about elements and every older revision they replaced in the working head: a report follows its
/// element forward, so the current revision shows what was written against its predecessors.
fn anchors(plan: &mut ReadPlan, owners: &[String]) -> SemanticRevisionResult<BTreeSet<String>> {
    plan.selection("working", false);
    let context = loaded(plan, "selection")?
        .into_iter()
        .find_map(|row| match row {
            SemanticRevisionMetadata::Selection(head) if head.selection_id == "working" => {
                Some(head.source_context)
            }
            _ => None,
        });
    let mut found = owners
        .iter()
        .map(SemanticRecordRef::element)
        .collect::<BTreeSet<_>>();
    if let Some(context) = context {
        let links = loaded(plan, "link")?;
        let mut pending = found.iter().cloned().collect::<Vec<_>>();
        while let Some(record) = pending.pop() {
            plan.index(
                "link_newer",
                &serde_json::to_string(&(&context, &record))?,
                None,
            );
            for link in &links {
                if let SemanticRevisionMetadata::Link(link) = link {
                    if link.source_context == context
                        && link.newer == record
                        && found.insert(link.older.clone())
                    {
                        pending.push(link.older.clone());
                    }
                }
            }
        }
    }
    Ok(found.into_iter().map(|record| record.record_id).collect())
}

fn loaded(plan: &ReadPlan, kind: &str) -> SemanticRevisionResult<Vec<SemanticRevisionMetadata>> {
    plan.rows
        .values()
        .filter(|row| row.key.kind == kind)
        .map(|row| Ok(serde_json::from_str(&row.metadata_json)?))
        .collect()
}

pub(in crate::policy_bytes::storage) fn source(
    context: &str,
) -> SemanticRevisionResult<Option<String>> {
    context
        .strip_prefix("challenge-contribution:source:")
        .map(serde_json::from_str::<(String, String, String)>)
        .transpose()
        .map(|pair| pair.map(|pair| pair.0))
        .map_err(Into::into)
}

pub(in crate::policy_bytes::storage) fn contributor(
    context: &str,
) -> SemanticRevisionResult<Option<String>> {
    context
        .strip_prefix("challenge-contribution:source:")
        .map(serde_json::from_str::<(String, String, String)>)
        .transpose()
        .map(|pair| pair.map(|pair| pair.2))
        .map_err(Into::into)
}

pub(super) fn source_context(artifact: &SemanticArtifact) -> SemanticRevisionResult<String> {
    let author = artifact.metadata["contributorSubjectHash"]
        .as_str()
        .ok_or_else(|| {
            invalid(
                &artifact.artifact_id,
                "an authenticated contribution author",
            )
        })?;
    Ok(format!(
        "challenge-contribution:source:{}",
        serde_json::to_string(&(&artifact.semantic_element_id, logical(artifact)?, author))?
    ))
}

pub(in crate::policy_bytes::storage) fn display(
    state: &SemanticRevisionState,
    rows: &[StorageRow],
    public_only: bool,
) -> SemanticRevisionResult<StorageResult> {
    let public = state.selection("public");
    let records = state.records.iter().filter(|stored| {
        !public_only || public.is_some_and(|head| head.records.contains(&stored.record))
    });
    encoded("challenge_nodes", &artifact_nodes(records, rows)?)
}

pub(super) fn artifact_nodes<'a>(
    records: impl IntoIterator<Item = &'a SemanticStoredRevision>,
    rows: &[StorageRow],
) -> SemanticRevisionResult<Vec<Value>> {
    let contents = content::available(rows)?;
    let mut nodes = Vec::new();
    for stored in records {
        let SemanticRevisionBody::Artifact(artifact) = &stored.body else {
            continue;
        };
        nodes.push(artifact_node(stored, artifact, &contents)?);
    }
    Ok(nodes)
}

fn artifact_node(
    stored: &SemanticStoredRevision,
    artifact: &SemanticArtifact,
    contents: &std::collections::BTreeMap<String, content::StoredContent>,
) -> SemanticRevisionResult<Value> {
    let mut displayed = artifact.clone();
    displayed.content = artifact
        .content_ref
        .as_ref()
        .and_then(|reference| contents.get(reference))
        .and_then(|body| String::from_utf8(body.bytes.clone()).ok())
        .or_else(|| artifact.content.clone());
    let token = stored
        .artifact_state
        .as_ref()
        .map(|state| state.edit_version.to_string())
        .unwrap_or_else(|| "1".into());
    context::node(
        "artifact",
        &artifact.artifact_id,
        &artifact.title,
        &json!(displayed),
        Some(&token),
    )
}
