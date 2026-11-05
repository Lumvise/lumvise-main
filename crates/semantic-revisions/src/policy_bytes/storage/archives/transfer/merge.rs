//! Candidate construction calls the same immutable-record and selection merge rules as whole archives.
use super::*;
use engine::{ArchiveImportPlan, key};

pub(super) fn copy(plan: &mut ArchiveImportPlan, row: &StorageRow) -> SemanticRevisionResult<bool> {
    if integer(&plan.context.expected_commit)? == 0 {
        return Err(invalid(
            &row.key.identity,
            "an empty current catalog before first adoption",
        ));
    }
    if row.key.kind == "membership" {
        let SemanticRevisionMetadata::Membership(member) = canonical::metadata(row)? else {
            unreachable!()
        };
        let Some(keep) = keep_membership(plan, &member, false)? else {
            return Ok(true);
        };
        if !keep {
            plan.remove(Catalog::Candidate, row.key.clone());
            return Ok(true);
        }
    }
    if row.key.kind == "project" && integer(&plan.context.expected_commit)? > 0 {
        if let SemanticRevisionMetadata::Project(id) = canonical::metadata(row)? {
            if id != plan.header.project_id {
                let kinds = ["record", "history", "operation", "retention", "recovery"];
                if let Some(kind) = kinds.get(plan.checkpoint.offset) {
                    if let Some(found) = plan.has_kind(Catalog::Current, kind)? {
                        if found {
                            return Err(invalid(id, "the existing project lineage"));
                        }
                        plan.checkpoint.offset += 1;
                    }
                    return Ok(false);
                }
            }
        }
    }
    let row = if [
        "record",
        "connection",
        "selection",
        "membership",
        "history",
        "link",
        "association",
        "operation",
        "retention",
        "recovery",
        "project",
    ]
    .contains(&row.key.kind.as_str())
    {
        canonical::source(row.clone(), &plan.context.project_root)?
    } else {
        row.clone()
    };
    plan.put(Catalog::Candidate, row);
    Ok(true)
}

pub(super) fn incoming(
    plan: &mut ArchiveImportPlan,
    source: &StorageRow,
) -> SemanticRevisionResult<bool> {
    let row = canonical::rebind(source, &plan.context.project_root)?;
    let previous = if integer(&plan.context.expected_commit)? == 0 {
        None
    } else {
        let Some(previous) = plan.key(Catalog::Current, row.key.clone())? else {
            return Ok(true);
        };
        previous
    };
    if row.key.kind == "membership" {
        let SemanticRevisionMetadata::Membership(member) = canonical::metadata(&row)? else {
            unreachable!()
        };
        if integer(&plan.context.expected_commit)? > 0
            && keep_membership(plan, &member, true)? != Some(true)
        {
            return Ok(true);
        }
    }
    let merged = merge_row(plan, &row, previous.as_ref())?;
    if let Some(merged) = merged {
        plan.put(Catalog::Candidate, merged);
    }
    Ok(true)
}

fn keep_membership(
    plan: &mut ArchiveImportPlan,
    member: &SemanticRevisionMembership,
    incoming: bool,
) -> SemanticRevisionResult<Option<bool>> {
    let head_key = key("selection", &member.selection_id);
    let Some(old) = plan.key(Catalog::Current, head_key.clone())? else {
        return Ok(None);
    };
    let Some(new) = plan.key(Catalog::Incoming, head_key)? else {
        return Ok(None);
    };
    let (old, new) = (selection(old.as_ref())?, selection(new.as_ref())?);
    let same = old
        .as_ref()
        .zip(new.as_ref())
        .is_some_and(|(old, new)| old.generation == new.generation);
    if same {
        let other = if incoming {
            Catalog::Current
        } else {
            Catalog::Incoming
        };
        let Some(found) = plan.key(other, membership_key(member)?)? else {
            return Ok(None);
        };
        if found.is_none() {
            return Err(invalid(
                &member.selection_id,
                "the same selection membership at an acknowledged generation",
            ));
        }
    }
    let use_incoming = new.as_ref().is_some_and(|new| {
        old.as_ref()
            .is_none_or(|old| new.generation >= old.generation)
    });
    Ok(Some(use_incoming == incoming || same))
}

fn merge_row(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
    previous: Option<&StorageRow>,
) -> SemanticRevisionResult<Option<StorageRow>> {
    if row.key.kind == "record" {
        return merge_record(plan, row, previous).map(Some);
    }
    if vectors::is_vector(row) {
        return vectors::archive::merge_row(row, previous).map(Some);
    }
    if row.key.kind == "selection" {
        return merge_selection(plan, row, previous).map(Some);
    }
    if origin::is_origin(row) {
        return Ok(if previous.is_some() {
            None
        } else {
            Some(row.clone())
        });
    }
    if row.key.kind == "membership" || row.key.kind == "project" {
        return Ok(Some(row.clone()));
    }
    if content::attachments::is_attachment(row) {
        return merge_attachment(row, previous).map(Some);
    }
    let metadata = canonical::metadata(row)?;
    if let Some(previous) = previous {
        if serde_json::to_vec(&canonical::metadata(previous)?)? != serde_json::to_vec(&metadata)? {
            return Err(crate::archive_merge::history_conflict(&metadata)?);
        }
    }
    require_reserved(plan, &metadata, previous.is_some())?;
    Ok(Some(row.clone()))
}

fn merge_record(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
    previous: Option<&StorageRow>,
) -> SemanticRevisionResult<StorageRow> {
    let mut incoming = canonical::record(row, &plan.context.project_root)?;
    let old = previous
        .map(|old| canonical::record(old, &plan.context.project_root))
        .transpose()?;
    count_record(plan, &incoming, old.as_ref());
    if let Some(mut stored) = old {
        let previous_body = stored.body.clone();
        crate::archive_merge::merge_record(&mut stored, &incoming, integer(&plan.context.now_ms)?)?;
        if stored.body != previous_body {
            let vector_key = vectors::key(&stored.record)?;
            if let Some(None) = plan.key(Catalog::Incoming, vector_key.clone())? {
                plan.remove(Catalog::Candidate, vector_key);
            }
        }
        let body = if previous.is_some_and(|row| row.body_json.is_some()) {
            previous.unwrap()
        } else {
            row
        };
        incoming = stored;
        let mut merged = canonical::encode_record(&incoming, body)?;
        let contents = content::available(&[previous.unwrap().clone(), row.clone()])?;
        content::complete_record(&mut merged, &contents)?;
        return Ok(merged);
    }
    canonical::encode_record(&incoming, row)
}

fn count_record(
    plan: &mut ArchiveImportPlan,
    incoming: &SemanticStoredRevision,
    old: Option<&SemanticStoredRevision>,
) {
    let added = incoming.body.is_available() && old.is_none_or(|old| !old.body.is_available());
    let counts = &mut plan.checkpoint.counts;
    match (incoming.record.kind, added, incoming.body.is_available()) {
        (SemanticRecordKind::Element, true, _) => counts.added_elements += 1,
        (SemanticRecordKind::Artifact, true, _) => counts.added_artifacts += 1,
        (SemanticRecordKind::Artifact, false, true) => counts.kept_artifacts += 1,
        _ => {}
    }
    if !added {
        counts.reused += 1;
    }
}

fn merge_selection(
    plan: &ArchiveImportPlan,
    row: &StorageRow,
    previous: Option<&StorageRow>,
) -> SemanticRevisionResult<StorageRow> {
    let new = selection(Some(row))?.expect("selection row");
    let old = selection(previous)?;
    let wrap = |selection| SemanticRevisionSelectionState {
        selection,
        records: vec![],
        connection_ids: vec![],
    };
    let old_heads = old.clone().map(wrap).into_iter().collect::<Vec<_>>();
    let heads = crate::archive_merge::merge_heads(&old_heads, &[wrap(new.clone())])?;
    require_reserved(
        plan,
        &SemanticRevisionMetadata::Selection(new),
        old_heads == heads,
    )?;
    let metadata = SemanticRevisionMetadata::Selection(heads[0].selection.clone());
    Ok(StorageRow {
        metadata_json: serde_json::to_string(&metadata)?,
        indexes: super::super::super::indexes::entries(&metadata, None)?,
        ..row.clone()
    })
}

fn merge_attachment(
    row: &StorageRow,
    previous: Option<&StorageRow>,
) -> SemanticRevisionResult<StorageRow> {
    if let Some(previous) = previous {
        let (old, new) = (
            content::attachments::decode(previous)?,
            content::attachments::decode(row)?,
        );
        if old.artifact_id != new.artifact_id
            || old.media_type != new.media_type
            || old.content != new.content
        {
            return Err(invalid(
                &row.key.identity,
                "immutable exact attachment owner, media type and bytes",
            ));
        }
        return Ok(previous.clone());
    }
    Ok(row.clone())
}

fn require_reserved(
    plan: &ArchiveImportPlan,
    metadata: &SemanticRevisionMetadata,
    identical: bool,
) -> SemanticRevisionResult<()> {
    use crate::archive_merge::reserved::{protected_receipt, reserved};
    let protected = match metadata {
        SemanticRevisionMetadata::Selection(head) => {
            reserved(&head.selection_id) || reserved(&head.source_context)
        }
        SemanticRevisionMetadata::History(delta) => {
            reserved(&delta.selection.selection_id) || reserved(&delta.selection.source_context)
        }
        SemanticRevisionMetadata::Operation(operation) => protected_receipt(&operation.receipt),
        _ => false,
    };
    if protected && !identical && integer(&plan.context.expected_commit)? > 0 {
        return Err(invalid(
            metadata.identity()?,
            "an identical existing source-owned selection/history/receipt",
        ));
    }
    Ok(())
}

pub(super) fn reconcile(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
) -> SemanticRevisionResult<bool> {
    if row.key.kind != "record" {
        return Ok(true);
    }
    let mut stored = canonical::record(row, &plan.context.project_root)?;
    let Some(membership) = plan.index(
        Catalog::Candidate,
        "member_record",
        &serde_json::to_string(&stored.record)?,
        None,
    )?
    else {
        return Ok(true);
    };
    if !membership.rows.is_empty() {
        stored.unselected_at_ms = None;
    } else if stored.unselected_at_ms.is_none() {
        stored.unselected_at_ms = Some(integer(&plan.context.now_ms)?);
    }
    plan.put(Catalog::Candidate, canonical::encode_record(&stored, row)?);
    Ok(true)
}

pub(super) fn count_removed(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
) -> SemanticRevisionResult<bool> {
    if row.key.kind != "membership" {
        return Ok(true);
    }
    let SemanticRevisionMetadata::Membership(member) = canonical::metadata(row)? else {
        unreachable!()
    };
    if member.selection_id != "working"
        || !matches!(&member.member, SemanticRevisionMember::Record(record) if record.kind == SemanticRecordKind::Element)
    {
        return Ok(true);
    }
    let Some(current) = plan.key(Catalog::Candidate, row.key.clone())? else {
        return Ok(true);
    };
    if current.is_none() {
        plan.checkpoint.counts.inactive += 1;
    }
    Ok(true)
}

pub(super) fn membership_key(
    member: &SemanticRevisionMembership,
) -> SemanticRevisionResult<StorageRowKey> {
    Ok(key("membership", &serde_json::to_string(member)?))
}
pub(super) fn selection(
    row: Option<&StorageRow>,
) -> SemanticRevisionResult<Option<SemanticRevisionSelection>> {
    row.map(|row| match canonical::metadata(row)? {
        SemanticRevisionMetadata::Selection(head) => Ok(head),
        _ => Err(invalid(&row.key.kind, "a selection header")),
    })
    .transpose()
}
