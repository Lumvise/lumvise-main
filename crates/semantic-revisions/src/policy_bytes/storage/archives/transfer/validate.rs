//! Scoped facts discharge the same archive integrity obligations without assembling a complete state.
use super::*;
use engine::{ArchiveImportPlan, key, record_key};

pub(super) fn row(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    if content::attachments::is_attachment(row) {
        return attachment(plan, row, catalog);
    }
    if vectors::is_vector(row) {
        return vector(plan, row, catalog);
    }
    if origin::is_origin(row) {
        origin::read(std::slice::from_ref(row))?;
        return Ok(true);
    }
    if catalog == Catalog::Candidate && auxiliary(row)? {
        return Ok(true);
    }
    let metadata = canonical::metadata(row)?;
    match &metadata {
        SemanticRevisionMetadata::Record(_) => record(plan, row, catalog),
        SemanticRevisionMetadata::Selection(head) => history::selection(plan, head, catalog),
        SemanticRevisionMetadata::Membership(member) => membership(plan, member, catalog),
        SemanticRevisionMetadata::Recovery(accepted) => recovery(plan, accepted, catalog),
        _ => {
            validate_metadata(plan, &metadata, catalog)?;
            Ok(true)
        }
    }
}

fn auxiliary(row: &StorageRow) -> SemanticRevisionResult<bool> {
    if super::super::is_receipt(row) {
        super::super::validate_receipt(row)?;
        return Ok(true);
    }
    if content::staged::is_staged(row) {
        content::staged::validate(row)?;
        return Ok(true);
    }
    if super::super::super::contributions::is_receipt(row) {
        super::super::super::contributions::validate_receipt(row)?;
        return Ok(true);
    }
    Ok(false)
}

pub(super) fn require(
    plan: &mut ArchiveImportPlan,
    catalog: Catalog,
    reference: &SemanticRecordRef,
    strict: bool,
) -> SemanticRevisionResult<Option<SemanticStoredRevision>> {
    let Some(row) = plan.key(catalog, record_key(reference)?)? else {
        return Ok(None);
    };
    let row =
        row.ok_or_else(|| invalid(&reference.record_id, "an existing exact required revision"))?;
    let stored = canonical::record(&row, root(plan, catalog))?;
    let state = SemanticRevisionState {
        records: vec![stored.clone()],
        ..Default::default()
    };
    let index = crate::required_revisions::RevisionDependencyIndex::new(&state);
    if strict {
        index.require(reference)?;
    } else {
        index.require_retained(reference)?;
    }
    Ok(Some(stored))
}

fn record(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    let stored = canonical::record(row, root(plan, catalog))?;
    if let SemanticRevisionBody::Unavailable(marker) = &stored.body {
        let Some(audit) = plan.key(catalog, key("recovery", &marker.recovery_operation_id))? else {
            return Ok(true);
        };
        let audit = audit
            .ok_or_else(|| invalid(&stored.record.record_id, "matching exact recovery audit"))?;
        let SemanticRevisionMetadata::Recovery(accepted) = canonical::metadata(&audit)? else {
            unreachable!()
        };
        crate::integrity::history::validate_receipt_shape(
            &plan.header.project_id,
            commit(plan, catalog)?,
            &accepted,
        )?;
        if !accepted
            .receipt
            .unavailable_records
            .contains(&stored.record)
        {
            return Err(invalid(
                &stored.record.record_id,
                "matching exact recovery audit",
            ));
        }
        crate::integrity::history::validate_audited_record(
            &accepted.receipt,
            &stored.record,
            Some(&stored),
        )?;
    }
    if !stored.body.is_available() {
        return Ok(true);
    }
    let dependencies = stored.body.intrinsic_dependencies();
    let end = history::reference_end(&dependencies, plan.checkpoint.offset)?
        .min(plan.checkpoint.offset + 1);
    for reference in &dependencies[plan.checkpoint.offset..end] {
        require(plan, catalog, reference, false)?;
    }
    if end < dependencies.len() {
        plan.checkpoint.offset = end;
        return Ok(false);
    }
    artifact_requirements(plan, row, &stored, catalog)
}

fn artifact_requirements(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
    stored: &SemanticStoredRevision,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    let SemanticRevisionBody::Artifact(artifact) = &stored.body else {
        return Ok(true);
    };
    if artifact.content_ref.is_some()
        && row
            .body_json
            .as_deref()
            .map(content::decode)
            .transpose()?
            .is_none_or(|body| body.content.is_none())
    {
        return Err(invalid(
            &artifact.artifact_id,
            "available exact primary blob bytes",
        ));
    }
    let Some(associations) = plan.index(
        catalog,
        "association_artifact",
        &artifact.artifact_id,
        plan.checkpoint.cursor.clone(),
    )?
    else {
        return Ok(false);
    };
    if plan.checkpoint.cursor.is_none() && associations.rows.is_empty() {
        require(
            plan,
            catalog,
            &SemanticRecordRef::element(&artifact.semantic_element_id),
            false,
        )?;
    }
    for row in associations.rows.iter().take(1) {
        let SemanticRevisionMetadata::Association(association) = canonical::metadata(row)? else {
            unreachable!()
        };
        require(
            plan,
            catalog,
            &SemanticRecordRef::element(&association.semantic_element_id),
            false,
        )?;
    }
    plan.checkpoint.cursor = if associations.rows.len() > 1 {
        Some(associations.rows[0].key.clone())
    } else {
        associations.next
    };
    Ok(plan.checkpoint.cursor.is_none())
}

fn membership(
    plan: &mut ArchiveImportPlan,
    member: &SemanticRevisionMembership,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    let Some(head) = plan.key(catalog, key("selection", &member.selection_id))? else {
        return Ok(true);
    };
    if head.is_none() {
        return Err(invalid(
            &member.selection_id,
            "a head for every selection membership",
        ));
    }
    match &member.member {
        SemanticRevisionMember::Record(reference) => {
            if let Some(stored) = require(
                plan,
                catalog,
                reference,
                member.selection_id == "public" && catalog == Catalog::Candidate,
            )? {
                if member.selection_id == "public" && catalog == Catalog::Candidate {
                    return public_dependencies(plan, &stored, catalog);
                }
            }
        }
        SemanticRevisionMember::Connection(identity) => {
            let Some(edge) = plan.key(catalog, key("connection", identity))? else {
                return Ok(true);
            };
            let edge = edge.ok_or_else(|| invalid(identity, "an existing selected connection"))?;
            let SemanticRevisionMetadata::Connection(edge) = canonical::metadata(&edge)? else {
                unreachable!()
            };
            for id in [
                &edge.relationship.source_element_id,
                &edge.relationship.target_element_id,
            ] {
                let reference = SemanticRecordRef::element(id);
                if member.selection_id == "public" && catalog == Catalog::Candidate {
                    public_reference(plan, &reference, catalog)?;
                } else {
                    require(plan, catalog, &reference, false)?;
                }
            }
            if catalog == Catalog::Candidate && member.selection_id == "working" {
                plan.checkpoint.counts.connections += 1;
            }
        }
    }
    Ok(true)
}

fn public_dependencies(
    plan: &mut ArchiveImportPlan,
    stored: &SemanticStoredRevision,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    let mut dependencies = stored.body.intrinsic_dependencies();
    if let SemanticRevisionBody::Artifact(artifact) = &stored.body {
        let Some(associations) =
            plan.index(catalog, "association_artifact", &artifact.artifact_id, None)?
        else {
            return Ok(false);
        };
        if associations.rows.is_empty() {
            dependencies.push(SemanticRecordRef::element(&artifact.semantic_element_id));
        }
    }
    let end = history::reference_end(&dependencies, plan.checkpoint.offset)?
        .min(plan.checkpoint.offset + 1);
    for reference in &dependencies[plan.checkpoint.offset..end] {
        public_reference(plan, reference, catalog)?;
    }
    plan.checkpoint.offset = end;
    Ok(end == dependencies.len())
}

fn public_reference(
    plan: &mut ArchiveImportPlan,
    reference: &SemanticRecordRef,
    catalog: Catalog,
) -> SemanticRevisionResult<()> {
    let member = SemanticRevisionMembership {
        selection_id: "public".into(),
        member: SemanticRevisionMember::Record(reference.clone()),
    };
    if let Some(found) = plan.key(catalog, merge::membership_key(&member)?)? {
        crate::selection_snapshot::require_published_reference(reference, found.is_some())?;
    }
    Ok(())
}

fn recovery(
    plan: &mut ArchiveImportPlan,
    accepted: &SemanticRevisionAcceptedRecovery,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    crate::integrity::history::validate_receipt_shape(
        &plan.header.project_id,
        commit(plan, catalog)?,
        accepted,
    )?;
    operation_identity(plan, catalog, "recovery", &accepted.receipt.operation_id)?;
    let references = &accepted.receipt.unavailable_records;
    let end =
        history::reference_end(references, plan.checkpoint.offset)?.min(plan.checkpoint.offset + 1);
    for reference in &references[plan.checkpoint.offset..end] {
        if let Some(stored) = require(plan, catalog, reference, false)? {
            crate::integrity::history::validate_audited_record(
                &accepted.receipt,
                reference,
                Some(&stored),
            )?;
        }
    }
    plan.checkpoint.offset = end;
    Ok(end == references.len())
}

fn validate_metadata(
    plan: &mut ArchiveImportPlan,
    metadata: &SemanticRevisionMetadata,
    catalog: Catalog,
) -> SemanticRevisionResult<()> {
    let version = match metadata {
        SemanticRevisionMetadata::History(delta) => Some(delta.commit_version),
        SemanticRevisionMetadata::Link(link) => Some(link.commit_version),
        SemanticRevisionMetadata::Operation(operation) => {
            operation_identity(plan, catalog, "operation", &operation.receipt.operation_id)?;
            Some(operation.receipt.commit_version)
        }
        SemanticRevisionMetadata::Retention(operation) => {
            operation_identity(plan, catalog, "retention", &operation.receipt.operation_id)?;
            Some(operation.receipt.commit_version)
        }
        SemanticRevisionMetadata::Connection(connection) => {
            connection.relationship.validate()?;
            crate::revision_index::ensure_root(
                &connection.relationship.project_root,
                root(plan, catalog),
            )?;
            None
        }
        SemanticRevisionMetadata::Project(id) if id != &plan.header.project_id => {
            return Err(invalid(id, "the frozen project lineage"));
        }
        _ => None,
    };
    if let Some(version) = version {
        crate::metadata::validate_archive_commit(commit(plan, catalog)?, version)?;
    }
    Ok(())
}

fn operation_identity(
    plan: &mut ArchiveImportPlan,
    catalog: Catalog,
    kind: &str,
    id: &str,
) -> SemanticRevisionResult<()> {
    for other in ["operation", "retention", "recovery"]
        .into_iter()
        .filter(|other| *other != kind)
    {
        if let Some(found) = plan.key(catalog, key(other, id))? {
            if found.is_some() {
                return Err(invalid(id, "unique accepted operation identities"));
            }
        }
    }
    Ok(())
}

fn attachment(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    let blob = content::attachments::decode(row)?;
    let Some(owner) = plan.key(Catalog::Scratch, owner_key(catalog, &blob.artifact_id)?)? else {
        return Ok(true);
    };
    if owner.is_none() {
        return Err(invalid(
            &blob.artifact_id,
            "an exact retained artifact attachment owner",
        ));
    }
    Ok(true)
}

fn vector(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    // A committed unavailable record can retain hidden cached vectors; incoming vector admission stays strict.
    if catalog == Catalog::Candidate {
        vectors::archive::canonical(row, root(plan, catalog))?;
        return Ok(true);
    }
    let owner = vectors::owner(row)?;
    if let Some(stored) = require(plan, catalog, &owner, true)? {
        vectors::archive::validate_row(row, stored, root(plan, catalog))?;
    }
    Ok(true)
}

pub(super) fn retain(
    plan: &mut ArchiveImportPlan,
    row: &StorageRow,
    catalog: Catalog,
) -> SemanticRevisionResult<bool> {
    if row.key.kind == "record" {
        let stored = canonical::record(row, root(plan, catalog))?;
        if stored.record.kind == SemanticRecordKind::Artifact && stored.body.is_available() {
            mark_owner(plan, catalog, &stored.record.record_id)?;
        }
    }
    if row.key.kind == "link" {
        let SemanticRevisionMetadata::Link(link) = canonical::metadata(row)? else {
            unreachable!()
        };
        if link.older.kind == SemanticRecordKind::Artifact
            && link.newer.kind == SemanticRecordKind::Artifact
        {
            if let Some(Some(_)) =
                plan.key(Catalog::Scratch, owner_key(catalog, &link.newer.record_id)?)?
            {
                mark_owner(plan, catalog, &link.older.record_id)?;
            }
        }
    }
    Ok(true)
}

fn mark_owner(
    plan: &mut ArchiveImportPlan,
    catalog: Catalog,
    id: &str,
) -> SemanticRevisionResult<()> {
    let key = owner_key(catalog, id)?;
    let Some(found) = plan.key(Catalog::Scratch, key.clone())? else {
        return Ok(());
    };
    if found.is_some() {
        return Ok(());
    }
    plan.put(
        Catalog::Scratch,
        StorageRow {
            key,
            metadata_json: "null".into(),
            body_json: None,
            indexes: vec![],
        },
    );
    plan.checkpoint.dirty = true;
    Ok(())
}

fn owner_key(catalog: Catalog, id: &str) -> SemanticRevisionResult<StorageRowKey> {
    Ok(key(
        "archive_retained_owner",
        &serde_json::to_string(&(catalog, id))?,
    ))
}
fn root(plan: &ArchiveImportPlan, catalog: Catalog) -> &str {
    if catalog == Catalog::Incoming {
        &plan.header.source_root
    } else {
        &plan.context.project_root
    }
}
fn commit(plan: &ArchiveImportPlan, catalog: Catalog) -> SemanticRevisionResult<i64> {
    Ok(if catalog == Catalog::Incoming {
        integer(&plan.header.archive_commit)?
    } else {
        integer(&plan.header.archive_commit)?.max(integer(&plan.context.expected_commit)?)
    })
}
