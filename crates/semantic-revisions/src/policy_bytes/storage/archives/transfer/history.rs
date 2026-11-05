//! Historical membership is reconstructed in indexed scratch rows, not in an in-memory selection.
use super::*;
use engine::{ArchiveImportPlan, key};

pub(super) fn selection(
    plan: &mut ArchiveImportPlan,
    head: &SemanticRevisionSelection,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    match plan.checkpoint.section {
        0 => context(plan, head, catalog),
        1..=4 => generation(plan, head, catalog),
        5 => compare(plan, head, catalog, false),
        _ => compare(plan, head, catalog, true),
    }
}

fn context(
    plan: &mut ArchiveImportPlan,
    head: &SemanticRevisionSelection,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    if head.mode == SemanticSelectionMode::FollowAccepted {
        let Some(found) = plan.index(
            catalog,
            "follow_context",
            &head.source_context,
            plan.checkpoint.cursor.clone(),
        )?
        else {
            return Ok(false);
        };
        let selections = found
            .rows
            .iter()
            .map(|row| {
                merge::selection(Some(row)).map(|selection| SemanticRevisionSelectionState {
                    selection: selection.expect("queried selection"),
                    records: vec![],
                    connection_ids: vec![],
                })
            })
            .collect::<SemanticRevisionResult<Vec<_>>>()?;
        let state = SemanticRevisionState {
            selections,
            ..Default::default()
        };
        crate::selection::require_context_selection(
            &state,
            &head.selection_id,
            &head.source_context,
            head.mode,
        )?;
        if found.next.is_some() {
            plan.checkpoint.cursor = found.next;
            return Ok(false);
        }
    }
    if head.generation < 1 {
        return Err(invalid(
            head.generation.to_string(),
            "a positive acknowledged selection generation",
        ));
    }
    plan.checkpoint.section = 1;
    plan.checkpoint.generation = 1;
    plan.checkpoint.cursor = None;
    Ok(false)
}

fn generation(
    plan: &mut ArchiveImportPlan,
    head: &SemanticRevisionSelection,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    let generation = plan.checkpoint.generation;
    let identity = serde_json::to_string(&(&head.selection_id, generation))?;
    let Some(row) = plan.key(catalog, key("history", &identity))? else {
        return Ok(false);
    };
    let row = row.ok_or_else(|| {
        invalid(
            &identity,
            "complete contiguous selection generation history",
        )
    })?;
    let SemanticRevisionMetadata::History(delta) = canonical::metadata(&row)? else {
        unreachable!()
    };
    crate::selection_history::validate_generation(generation - 1, &delta)?;
    if generation == head.generation && delta.selection != *head {
        return Err(invalid(
            &head.selection_id,
            "the exact final historical selection header",
        ));
    }
    let members = members(&delta, plan.checkpoint.section);
    let end = reference_end(&members, plan.checkpoint.offset)?;
    let start = plan.checkpoint.offset;
    let mut waiting = false;
    for (index, member) in members.iter().enumerate().take(end).skip(start) {
        if waiting {
            plan.key(
                Catalog::Scratch,
                member_key(catalog, &head.selection_id, member)?,
            )?;
            continue;
        }
        change(
            plan,
            head,
            catalog,
            member,
            matches!(plan.checkpoint.section, 2 | 4),
        )?;
        if plan.waiting() {
            if index > start {
                plan.commit_prefix();
                plan.checkpoint.offset = index;
                return Ok(false);
            }
            waiting = true;
        }
    }
    if waiting {
        return Ok(false);
    }
    plan.checkpoint.offset = end;
    if end == members.len() {
        next_collection(plan, generation == head.generation);
    }
    Ok(false)
}

fn members(delta: &SemanticRevisionSelectionDelta, section: usize) -> Vec<SemanticRevisionMember> {
    match section {
        1 => delta
            .removed_records
            .iter()
            .cloned()
            .map(SemanticRevisionMember::Record)
            .collect(),
        2 => delta
            .added_records
            .iter()
            .cloned()
            .map(SemanticRevisionMember::Record)
            .collect(),
        3 => delta
            .removed_connection_ids
            .iter()
            .cloned()
            .map(SemanticRevisionMember::Connection)
            .collect(),
        _ => delta
            .added_connection_ids
            .iter()
            .cloned()
            .map(SemanticRevisionMember::Connection)
            .collect(),
    }
}

pub(super) fn reference_end<T: Serialize>(
    values: &[T],
    start: usize,
) -> SemanticRevisionResult<usize> {
    if start > values.len() {
        return Err(invalid(
            start.to_string(),
            "an offset within the current reference collection",
        ));
    }
    let mut bytes = 0;
    let mut end = start;
    for value in &values[start..] {
        let size = serde_json::to_vec(value)?.len();
        if end > start && (bytes + size > PAGE_BYTES || end - start >= 128) {
            break;
        }
        bytes += size;
        end += 1;
    }
    Ok(end)
}

fn next_collection(plan: &mut ArchiveImportPlan, final_generation: bool) {
    plan.checkpoint.offset = 0;
    plan.checkpoint.section += 1;
    if plan.checkpoint.section == 5 && !final_generation {
        plan.checkpoint.generation += 1;
        plan.checkpoint.section = 1;
    }
}

fn change(
    plan: &mut ArchiveImportPlan,
    head: &SemanticRevisionSelection,
    catalog: Catalog,
    member: &SemanticRevisionMember,
    adding: bool,
) -> SemanticRevisionResult<()> {
    let membership = SemanticRevisionMembership {
        selection_id: head.selection_id.clone(),
        member: member.clone(),
    };
    let row_key = member_key(catalog, &head.selection_id, member)?;
    let Some(old) = plan.key(Catalog::Scratch, row_key.clone())? else {
        return Ok(());
    };
    crate::selection_history::validate_membership_change(member, old.is_some(), adding)?;
    if !adding {
        plan.remove(Catalog::Scratch, row_key);
        return Ok(());
    }
    let owner = serde_json::to_string(&(catalog, &head.selection_id))?;
    plan.put(
        Catalog::Scratch,
        StorageRow {
            key: row_key,
            metadata_json: serde_json::to_string(&membership)?,
            body_json: None,
            indexes: vec![super::super::super::indexes::StorageIndex {
                name: "archive_history_member".into(),
                value: owner,
            }],
        },
    );
    Ok(())
}

fn member_key(
    catalog: Catalog,
    selection: &str,
    member: &SemanticRevisionMember,
) -> SemanticRevisionResult<StorageRowKey> {
    let membership = SemanticRevisionMembership {
        selection_id: selection.into(),
        member: member.clone(),
    };
    Ok(key(
        "archive_history_member",
        &serde_json::to_string(&(catalog, membership))?,
    ))
}

fn compare(
    plan: &mut ArchiveImportPlan,
    head: &SemanticRevisionSelection,
    catalog: Catalog,
    reverse: bool,
) -> SemanticRevisionResult<bool> {
    let owner = serde_json::to_string(&(catalog, &head.selection_id))?;
    let (scan, name, value) = if reverse {
        (Catalog::Scratch, "archive_history_member", owner.as_str())
    } else {
        (catalog, "selection_member", head.selection_id.as_str())
    };
    let Some(page) = plan.index(scan, name, value, plan.checkpoint.cursor.clone())? else {
        return Ok(false);
    };
    let mut processed = 0;
    let mut last = None;
    let mut waiting = false;
    for row in page.rows.iter().take(128) {
        let membership = if reverse {
            serde_json::from_str(&row.metadata_json)?
        } else {
            let SemanticRevisionMetadata::Membership(member) = canonical::metadata(row)? else {
                unreachable!()
            };
            member
        };
        let target = if reverse {
            merge::membership_key(&membership)?
        } else {
            key(
                "archive_history_member",
                &serde_json::to_string(&(catalog, &membership))?,
            )
        };
        let target_catalog = if reverse { catalog } else { Catalog::Scratch };
        if let Some(found) = plan.key(target_catalog, target)? {
            if found.is_none() {
                return Err(invalid(
                    &head.selection_id,
                    "current membership matching complete generation history",
                ));
            }
            if !waiting {
                processed += 1;
                last = Some(row.key.clone());
            }
        } else {
            if processed > 0 {
                plan.commit_prefix();
                plan.checkpoint.cursor = last;
                return Ok(false);
            }
            waiting = true;
        }
    }
    if waiting {
        return Ok(false);
    }
    plan.checkpoint.cursor = if page.rows.len() > 128 {
        Some(page.rows[127].key.clone())
    } else {
        page.next
    };
    if plan.checkpoint.cursor.is_some() {
        return Ok(false);
    }
    if reverse {
        return Ok(true);
    }
    plan.checkpoint.section = 6;
    Ok(false)
}
