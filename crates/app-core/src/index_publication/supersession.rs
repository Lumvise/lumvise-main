//! Owns audited replacement of frozen source intent made impossible by approved loss recovery.
//! Source checkout may call `supersede_recovered_pending`; descriptors and pages remain immutable.
use super::*;
use lumvise_db_core::{SemanticRecordRef, SemanticRevisionRecordHeader};
use std::collections::{BTreeMap, BTreeSet};

impl IndexPublicationJournal {
    pub(super) fn require_not_superseded(
        &self,
        operation: &str,
        control: &InvocationControl,
    ) -> Result<()> {
        if self
            .rows
            .get::<RecoveredSourceSupersession>(&audit_key(operation), control)?
            .is_some()
        {
            return Err(invalid_checkpoint(
                operation,
                "a new operation ID after audited source supersession",
            ));
        }
        Ok(())
    }

    /// Clears only impossible unaccepted intent; accepted work always retains its ordinary replay path.
    /// Example: after recovery, preserve a frozen request's pages and let checkout scan a new operation.
    pub(super) fn supersede_recovered_pending(
        &self,
        publication: &FrozenIndexPublication,
        unavailable: &[SemanticRevisionRecordHeader],
        control: &InvocationControl,
    ) -> Result<bool> {
        let blocked_headers = self.blocked_recovery_headers(publication, unavailable)?;
        if blocked_headers.is_empty() {
            return Ok(false);
        }
        let Some(commit) = self.unaccepted_recovery_commit(&publication.operation_id, control)?
        else {
            return Ok(false);
        };
        let audit = RecoveredSourceSupersession {
            operation_id: publication.operation_id.clone(),
            frozen_digest: pending::publication_digest(publication)?,
            inspected_commit_version: commit,
            blocked_headers,
        };
        self.commit_source_supersession(publication, audit, control)?;
        Ok(true)
    }

    fn blocked_recovery_headers(
        &self,
        publication: &FrozenIndexPublication,
        unavailable: &[SemanticRevisionRecordHeader],
    ) -> Result<Vec<SemanticRevisionRecordHeader>> {
        let references = external_frozen_references(publication);
        let scope = publication.partition();
        let mut blocked = BTreeMap::new();
        for header in unavailable {
            if !references.contains(&header.record) || !owned_external_header(header, &scope) {
                continue;
            }
            retain_recovered_header(&mut blocked, header)?;
        }
        Ok(blocked.into_values().collect())
    }

    fn unaccepted_recovery_commit(
        &self,
        operation: &str,
        control: &InvocationControl,
    ) -> Result<Option<i64>> {
        let Some(report) = self.inspect_recovery_for_supersession(operation, control)? else {
            return Ok(None);
        };
        validate_recovered_observation(
            &report,
            &self.binding.project_id,
            &self.project_root,
            operation,
        )?;
        unaccepted_commit(&report, operation)
    }

    fn inspect_recovery_for_supersession(
        &self,
        operation: &str,
        control: &InvocationControl,
    ) -> Result<Option<lumvise_db_core::SemanticRevisionIntegrityReport>> {
        let result = match self.semantic(
            SemanticOperation::InspectRevisionIntegrity {
                project_root: self.project_root.clone(),
                publication_operation_id: Some(operation.into()),
            },
            control,
        ) {
            Ok(result) => result,
            Err(error) => {
                // Inspection must not prevent the ordinary accepted-receipt replay fast path.
                tracing::warn!(event = "source_supersession_inspection_failed", operation_id = operation, error = %error);
                return Ok(None);
            }
        };
        let SemanticResult::RevisionIntegrity(report) = result else {
            return Err(invalid_checkpoint(
                operation,
                "a guarded recovery and publication observation",
            ));
        };
        Ok(Some(report))
    }

    fn commit_source_supersession(
        &self,
        publication: &FrozenIndexPublication,
        audit: RecoveredSourceSupersession,
        control: &InvocationControl,
    ) -> Result<()> {
        let original = self.header(control)?;
        let key = audit_key(&audit.operation_id);
        let previous: Option<RecoveredSourceSupersession> = self.rows.get(&key, control)?;
        if let Some(previous) = previous {
            return verify_existing_supersession(&original, &previous, &audit);
        }
        self.require_unchanged_frozen(publication, &original, &audit, control)?;
        self.write_source_supersession(&original, &key, &audit, control)
    }

    fn require_unchanged_frozen(
        &self,
        publication: &FrozenIndexPublication,
        original: &CheckpointHeader,
        audit: &RecoveredSourceSupersession,
        control: &InvocationControl,
    ) -> Result<()> {
        require_exact_pending(original, audit)?;
        if self.pending(control)?.as_ref() != Some(publication) {
            return Err(invalid_checkpoint(
                &audit.operation_id,
                "the unchanged complete frozen request before supersession",
            ));
        }
        Ok(())
    }

    fn write_source_supersession(
        &self,
        original: &CheckpointHeader,
        key: &str,
        audit: &RecoveredSourceSupersession,
        control: &InvocationControl,
    ) -> Result<()> {
        let mut next = original.clone();
        next.pending = None;
        self.rows.apply(
            vec![
                self.rows.require(Some(original))?,
                self.rows
                    .require_value::<RecoveredSourceSupersession>(key, None)?,
                self.rows.put(key, audit)?,
                self.rows.put("header", &next)?,
            ],
            control,
        )
    }
}

fn retain_recovered_header(
    blocked: &mut BTreeMap<SemanticRecordRef, SemanticRevisionRecordHeader>,
    header: &SemanticRevisionRecordHeader,
) -> Result<()> {
    let absence = header.payload_absence().ok_or_else(|| {
        invalid_checkpoint(
            &header.record.record_id,
            "an acknowledged unavailable exact source revision",
        )
    })?;
    header
        .clone()
        .restore(absence)
        .map_err(lumvise_db_core::DbError::from)?;
    if blocked
        .insert(header.record.clone(), header.clone())
        .is_some_and(|old| old != *header)
    {
        return Err(invalid_checkpoint(
            &header.record.record_id,
            "identical recovery metadata across source selections",
        ));
    }
    Ok(())
}

fn unaccepted_commit(
    report: &lumvise_db_core::SemanticRevisionIntegrityReport,
    operation: &str,
) -> Result<Option<i64>> {
    if report
        .publication_inspection
        .as_ref()
        .is_some_and(|inspected| inspected.accepted.is_some())
    {
        return Ok(None);
    }
    if !report.missing_records.is_empty() {
        return Err(invalid_checkpoint(
            operation,
            "no unacknowledged missing bodies before source supersession",
        ));
    }
    Ok(Some(report.commit_version))
}

fn external_frozen_references(publication: &FrozenIndexPublication) -> BTreeSet<SemanticRecordRef> {
    let elements = &publication.batch.semantic_elements;
    let incoming: BTreeSet<_> = elements
        .iter()
        .map(|element| SemanticRecordRef::element(&element.semantic_element_id))
        .collect();
    let edges = publication
        .batch
        .semantic_relationships
        .iter()
        .flat_map(|edge| {
            std::iter::once(edge.source_element_id.as_str())
                .chain(edge.target_element_id.as_deref())
        })
        .map(SemanticRecordRef::element);
    let parents = elements
        .iter()
        .filter_map(|element| element.parent_element_id.as_deref())
        .map(SemanticRecordRef::element);
    edges
        .chain(parents)
        .chain(
            elements
                .iter()
                .flat_map(|element| element.revision_dependencies.clone()),
        )
        .filter(|record| !incoming.contains(record))
        .collect()
}

fn owned_external_header(
    header: &SemanticRevisionRecordHeader,
    scope: &lumvise_db_core::SemanticPartition,
) -> bool {
    header.unavailable.is_some()
        && header.element_scope.as_ref().is_some_and(|owner| {
            owner.provider_instance_id == scope.provider_instance_id
                && !scope.contains_path(&owner.path)
        })
}

fn validate_recovered_observation(
    report: &lumvise_db_core::SemanticRevisionIntegrityReport,
    project: &str,
    root: &str,
    operation: &str,
) -> Result<()> {
    if report.project_id != project
        || report.project_root != root
        || report
            .publication_inspection
            .as_ref()
            .is_none_or(|observed| observed.operation_id != operation)
    {
        return Err(invalid_checkpoint(
            operation,
            "the same recovered project, no unacknowledged missing bodies, and exact publication inspection",
        ));
    }
    Ok(())
}

fn require_exact_pending(
    header: &CheckpointHeader,
    audit: &RecoveredSourceSupersession,
) -> Result<()> {
    if header.last_acknowledged_operation.as_deref() == Some(&audit.operation_id)
        || header.pending.as_ref().is_none_or(|pending| {
            !pending.ready
                || pending.operation_id != audit.operation_id
                || pending.digest != audit.frozen_digest
        })
    {
        return Err(invalid_checkpoint(
            &audit.operation_id,
            "the identical ready unacknowledged source operation",
        ));
    }
    Ok(())
}

fn verify_existing_supersession(
    header: &CheckpointHeader,
    previous: &RecoveredSourceSupersession,
    requested: &RecoveredSourceSupersession,
) -> Result<()> {
    if previous.operation_id != requested.operation_id
        || previous.frozen_digest != requested.frozen_digest
        || previous.blocked_headers != requested.blocked_headers
        || header.pending.is_some()
    {
        return Err(invalid_checkpoint(
            &requested.operation_id,
            "the same completed supersession with no replacement pending operation",
        ));
    }
    Ok(())
}

fn audit_key(operation: &str) -> String {
    format!("superseded/{operation}")
}
