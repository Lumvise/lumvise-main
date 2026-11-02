//! Guarded metadata-only acknowledgement. Native storage supplies verified absence facts.
use crate::revision_index::{digest, ensure_root, require_project_identity};
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Fresh storage inspection; malformed or duplicate existing bodies must fail before this report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionIntegrityReport {
    pub project_root: String,
    pub project_id: String,
    pub commit_version: i64,
    pub missing_records: Vec<SemanticRevisionRecordHeader>,
    pub publication_inspection: Option<SemanticRevisionPublicationInspection>,
}

/// Exact publication lookup from the same storage observation; absence is explicit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionPublicationInspection {
    pub operation_id: String,
    pub accepted: Option<SemanticRevisionAcceptedOperation>,
}

/// Explicit owner-approved scope. Example: inspect, copy exact IDs, then submit once with an operation ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionRecoveryRequest {
    pub operation_id: String,
    pub project_root: String,
    pub project_id: String,
    pub expected_commit_version: i64,
    pub provider_instance_id: String,
    pub missing_records: Vec<SemanticRecordRef>,
}

/// Durable acknowledgement; this does not claim that lost historical bytes were reconstructed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionRecoveryReceipt {
    pub operation_id: String,
    pub project_id: String,
    pub previous_commit_version: i64,
    pub commit_version: i64,
    pub unavailable_at_ms: i64,
    pub unavailable_records: Vec<SemanticRecordRef>,
}

/// Retry identity retained with revision metadata, including through portable archives.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionAcceptedRecovery {
    pub request_root: String,
    pub request_digest: String,
    pub receipt: SemanticRevisionRecoveryReceipt,
}

/// Only header updates and one audit row may enter the adapter's existing atomic commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionRecoveryPlan {
    pub expected_commit_version: i64,
    pub headers: Vec<SemanticRevisionRecordHeader>,
    pub accepted: SemanticRevisionAcceptedRecovery,
}

impl SemanticRevisionIntegrityReport {
    /// Fences a loss acknowledgement to exact inspected state. Example: `report.plan_recovery(&request, now, next)?`.
    pub fn plan_recovery(
        &self,
        request: &SemanticRevisionRecoveryRequest,
        now_ms: i64,
        commit_version: i64,
    ) -> SemanticRevisionResult<SemanticRevisionRecoveryPlan> {
        validate_recovery_identity(self, request, commit_version)?;
        let references = validate_recovery_records(self, request)?;
        let headers = unavailable_headers(self, request, now_ms)?;
        Ok(SemanticRevisionRecoveryPlan {
            expected_commit_version: self.commit_version,
            headers,
            accepted: accepted_recovery(self, request, references, now_ms, commit_version)?,
        })
    }
}

impl SemanticRevisionAcceptedRecovery {
    /// Replays only identical intent after later commits. Example: `accepted.replay(&request)?` before inspection.
    pub fn replay(
        &self,
        request: &SemanticRevisionRecoveryRequest,
    ) -> SemanticRevisionResult<SemanticRevisionRecoveryReceipt> {
        if request.operation_id != self.receipt.operation_id
            || recovery_digest(request, &self.request_root)? != self.request_digest
        {
            return Err(SemanticRevisionError::invalid_value(
                &request.operation_id,
                "identical guarded recovery intent for the accepted operation ID",
            ));
        }
        Ok(self.receipt.clone())
    }
}

fn validate_recovery_identity(
    report: &SemanticRevisionIntegrityReport,
    request: &SemanticRevisionRecoveryRequest,
    commit_version: i64,
) -> SemanticRevisionResult<()> {
    ensure_root(&request.project_root, &report.project_root)?;
    require_project_identity(&request.project_id)?;
    if request.project_id != report.project_id
        || request.expected_commit_version != report.commit_version
        || report.commit_version < 0
        || commit_version <= report.commit_version
    {
        return Err(SemanticRevisionError::invalid_value(
            format!(
                "{}@{} -> {commit_version}",
                request.project_id, request.expected_commit_version
            ),
            format!(
                "inspected project {} at commit {} and a strictly newer commit",
                report.project_id, report.commit_version
            ),
        ));
    }
    validate_recovery_names(request)
}

fn validate_recovery_names(
    request: &SemanticRevisionRecoveryRequest,
) -> SemanticRevisionResult<()> {
    if request.operation_id.trim().is_empty() || request.provider_instance_id.trim().is_empty() {
        return Err(SemanticRevisionError::invalid_value(
            format!(
                "operation={:?}, provider={:?}",
                request.operation_id, request.provider_instance_id
            ),
            "nonempty recovery operation and source provider IDs",
        ));
    }
    crate::citation_retention::require_general_selection(&request.operation_id)
}

fn validate_recovery_records(
    report: &SemanticRevisionIntegrityReport,
    request: &SemanticRevisionRecoveryRequest,
) -> SemanticRevisionResult<Vec<SemanticRecordRef>> {
    let requested = request
        .missing_records
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let inspected = report
        .missing_records
        .iter()
        .map(|header| header.record.clone())
        .collect::<BTreeSet<_>>();
    if requested.is_empty()
        || requested.len() != request.missing_records.len()
        || inspected.len() != report.missing_records.len()
        || requested != inspected
    {
        return Err(SemanticRevisionError::invalid_value(
            format!("{:?}", request.missing_records),
            format!("exactly the unique nonempty inspected missing record set {inspected:?}"),
        ));
    }
    for header in &report.missing_records {
        validate_recoverable_header(header, &request.provider_instance_id)?;
    }
    Ok(requested.into_iter().collect())
}

fn validate_recoverable_header(
    header: &SemanticRevisionRecordHeader,
    provider: &str,
) -> SemanticRevisionResult<()> {
    let owned = header.element_scope.as_ref().is_some_and(|scope| {
        !scope.path.trim().is_empty() && scope.provider_instance_id.as_deref() == Some(provider)
    });
    if header.record.kind != SemanticRecordKind::Element
        || header.record.record_id.trim().is_empty()
        || !owned
        || header.payload_absence().is_some()
    {
        return Err(SemanticRevisionError::invalid_value(
            format!("{header:?}"),
            format!(
                "an unpruned, unacknowledged missing source element owned by provider {provider:?}"
            ),
        ));
    }
    Ok(())
}

fn unavailable_headers(
    report: &SemanticRevisionIntegrityReport,
    request: &SemanticRevisionRecoveryRequest,
    now_ms: i64,
) -> SemanticRevisionResult<Vec<SemanticRevisionRecordHeader>> {
    report
        .missing_records
        .iter()
        .map(|header| {
            let mut next = header.clone();
            let marker = SemanticRevisionUnavailable {
                recovery_operation_id: request.operation_id.clone(),
                unavailable_at_ms: now_ms,
                canonical_lifecycle: header.canonical_lifecycle.clone(),
            };
            marker.validate(&header.record)?;
            next.unavailable = Some(marker.clone());
            next.clone()
                .restore(SemanticRevisionBody::Unavailable(marker))?;
            Ok(next)
        })
        .collect()
}

fn recovery_digest(
    request: &SemanticRevisionRecoveryRequest,
    root: &str,
) -> SemanticRevisionResult<String> {
    let mut canonical = request.clone();
    canonical.project_root = root.into();
    canonical.missing_records.sort();
    digest(&canonical)
}

fn accepted_recovery(
    report: &SemanticRevisionIntegrityReport,
    request: &SemanticRevisionRecoveryRequest,
    unavailable_records: Vec<SemanticRecordRef>,
    unavailable_at_ms: i64,
    commit_version: i64,
) -> SemanticRevisionResult<SemanticRevisionAcceptedRecovery> {
    Ok(SemanticRevisionAcceptedRecovery {
        request_root: report.project_root.clone(),
        request_digest: recovery_digest(request, &report.project_root)?,
        receipt: SemanticRevisionRecoveryReceipt {
            operation_id: request.operation_id.clone(),
            project_id: report.project_id.clone(),
            previous_commit_version: report.commit_version,
            commit_version,
            unavailable_at_ms,
            unavailable_records,
        },
    })
}
