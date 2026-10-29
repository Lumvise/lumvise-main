//! Lossless byte boundary for the identical native/WebAssembly policy; JavaScript never decodes record numbers.
use crate::*;
use serde::{Deserialize, Serialize};
mod storage;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum RevisionPolicyRequest {
    Resolve {
        state: SemanticRevisionState,
        record: SemanticRecordRef,
    },
    History {
        state: SemanticRevisionState,
    },
    CitationAcquisition {
        state: SemanticRevisionState,
        request: SemanticCitationRetentionAcquire,
        authenticated_actor: String,
        timestamp: SemanticRevisionTimestamp,
        next_commit: i64,
    },
    CitationRelease {
        state: SemanticRevisionState,
        request: SemanticCitationRetentionRelease,
        authenticated_actor: String,
        timestamp: SemanticRevisionTimestamp,
        next_commit: i64,
    },
    Storage {
        request: storage::StorageRequest,
    },
    Families {
        basis: SemanticRevisionFamilyBasis,
        query: SemanticRevisionFamilyQuery,
    },
    Connections {
        state: SemanticRevisionState,
        query: SemanticRevisionConnectionQuery,
    },
    ArchiveMerge {
        state: SemanticRevisionState,
        archived: SemanticRevisionState,
        now_ms: i64,
    },
    Publication {
        basis: SemanticRevisionPublicationBasis,
        request: SemanticRevisionPublication,
        timestamp: SemanticRevisionTimestamp,
        next_commit: i64,
    },
    Selection {
        state: SemanticRevisionState,
        request: SemanticRevisionSelectionUpdate,
        timestamp: SemanticRevisionTimestamp,
        next_commit: i64,
    },
    RetentionPlan {
        state: SemanticRevisionState,
        operation_id: String,
        policy: SemanticRevisionRetentionPolicy,
        now_ms: i64,
    },
    Collect {
        state: SemanticRevisionState,
        plan: SemanticRevisionRetentionPlan,
        policy: SemanticRevisionRetentionPolicy,
        now_ms: i64,
        next_commit: i64,
    },
    Membership {
        state: SemanticRevisionState,
        query: SemanticRevisionSelectionQuery,
    },
    SelectionAt {
        state: SemanticRevisionState,
        selection_id: String,
        generation: i64,
    },
    SourceAnchor {
        state: SemanticRevisionState,
        selection_id: String,
        anchor: SemanticSourceAnchor,
    },
    Snapshot {
        state: SemanticRevisionState,
        selection_id: String,
        published_at: String,
    },
    Adopt {
        project_root: String,
        project_id: String,
        selection_id: String,
        source_context: String,
        elements: Vec<SemanticElement>,
        artifacts: Vec<SemanticArtifact>,
        relationships: Vec<SemanticRelationship>,
        commit_version: i64,
        now_ms: i64,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum RevisionPolicyResult {
    Resolution(Option<SemanticRevisionResolution>),
    Storage(storage::StorageResult),
    Families(Vec<SemanticRevisionRemap>),
    Connections(Vec<SemanticRevisionConnection>),
    Publication(SemanticRevisionPublicationPlan),
    RetentionPlan(SemanticRevisionRetentionPlan),
    Collection {
        state: SemanticRevisionState,
        receipt: SemanticRevisionRetentionReceipt,
    },
    Membership(Option<SemanticRevisionSelectionState>),
    Observation(Option<SemanticRevisionSelectionObservation>),
    Snapshot(Option<SemanticRevisionSelectionSnapshot>),
    State(SemanticRevisionState),
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum RevisionPolicyResponse {
    Accepted { result: RevisionPolicyResult },
    Rejected { message: String },
}

/// Executes the shared policy on opaque UTF-8 JSON bytes, preserving all signed 64-bit values.
/// Example: `let response = evaluate_revision_policy(br#"{"operation":"unknown"}"#);` returns a rejection.
pub fn evaluate_revision_policy(input: &[u8]) -> Vec<u8> {
    let result = serde_json::from_slice::<RevisionPolicyRequest>(input)
        .map_err(SemanticRevisionError::from)
        .and_then(evaluate);
    let response = match result {
        Ok(result) => RevisionPolicyResponse::Accepted { result },
        Err(error) => RevisionPolicyResponse::Rejected {
            message: error.to_string(),
        },
    };
    serde_json::to_vec(&response)
        .expect("revision policy responses contain only JSON-serializable domain records")
}

fn evaluate(request: RevisionPolicyRequest) -> SemanticRevisionResult<RevisionPolicyResult> {
    Ok(match request {
        RevisionPolicyRequest::Resolve { state, record } => {
            RevisionPolicyResult::Resolution(state.resolve(&record))
        }
        RevisionPolicyRequest::History { state } => RevisionPolicyResult::State(state),
        RevisionPolicyRequest::CitationAcquisition {
            state,
            request,
            authenticated_actor,
            timestamp,
            next_commit,
        } => RevisionPolicyResult::Publication(state.plan_citation_acquisition(
            &request,
            &authenticated_actor,
            &timestamp,
            next_commit,
        )?),
        RevisionPolicyRequest::CitationRelease {
            state,
            request,
            authenticated_actor,
            timestamp,
            next_commit,
        } => RevisionPolicyResult::Publication(state.plan_citation_release(
            &request,
            &authenticated_actor,
            &timestamp,
            next_commit,
        )?),
        RevisionPolicyRequest::Storage { request } => {
            RevisionPolicyResult::Storage(storage::evaluate(request)?)
        }
        RevisionPolicyRequest::Families { basis, query } => {
            RevisionPolicyResult::Families(basis.resolve(&query)?)
        }
        RevisionPolicyRequest::Connections { state, query } => {
            RevisionPolicyResult::Connections(state.resolve_connections(&query)?)
        }
        RevisionPolicyRequest::ArchiveMerge {
            state,
            archived,
            now_ms,
        } => RevisionPolicyResult::State(state.merge_archive(&archived, now_ms)?),
        RevisionPolicyRequest::Publication {
            basis,
            request,
            timestamp,
            next_commit,
        } => RevisionPolicyResult::Publication(basis.plan_publication(
            &request,
            &timestamp,
            next_commit,
        )?),
        RevisionPolicyRequest::Selection {
            state,
            request,
            timestamp,
            next_commit,
        } => RevisionPolicyResult::Publication(state.plan_selection(
            &request,
            &timestamp,
            next_commit,
        )?),
        RevisionPolicyRequest::RetentionPlan {
            state,
            operation_id,
            policy,
            now_ms,
        } => RevisionPolicyResult::RetentionPlan(state.plan_retention(
            &operation_id,
            &policy,
            now_ms,
        )?),
        RevisionPolicyRequest::Collect {
            mut state,
            plan,
            policy,
            now_ms,
            next_commit,
        } => {
            let receipt = state.apply_retention(&plan, &policy, now_ms, next_commit)?;
            RevisionPolicyResult::Collection { state, receipt }
        }
        RevisionPolicyRequest::Membership { state, query } => {
            RevisionPolicyResult::Observation(state.selection_members(&query)?)
        }
        RevisionPolicyRequest::SelectionAt {
            state,
            selection_id,
            generation,
        } => RevisionPolicyResult::Membership(
            state.selection_at_generation(&selection_id, generation)?,
        ),
        RevisionPolicyRequest::SourceAnchor {
            state,
            selection_id,
            anchor,
        } => RevisionPolicyResult::Membership(
            state.selection_for_source_anchor(&selection_id, &anchor)?,
        ),
        RevisionPolicyRequest::Snapshot {
            state,
            selection_id,
            published_at,
        } => RevisionPolicyResult::Snapshot(state.snapshot(&selection_id, &published_at)?),
        RevisionPolicyRequest::Adopt {
            project_root,
            project_id,
            selection_id,
            source_context,
            elements,
            artifacts,
            relationships,
            commit_version,
            now_ms,
        } => RevisionPolicyResult::State(SemanticRevisionState::adopt(
            &project_root,
            &project_id,
            &selection_id,
            &source_context,
            elements,
            artifacts,
            relationships,
            commit_version,
            now_ms,
        )?),
    })
}
