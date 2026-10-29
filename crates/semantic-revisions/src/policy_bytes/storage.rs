//! Private gateway storage envelopes. Only this module decodes domain numbers; SQL sees string keys.
use crate::*;
use serde::{Deserialize, Serialize};
mod archives;
mod content;
mod contributions;
mod headers;
mod indexes;
mod names;
mod read;
mod request;
mod rows;
mod vectors;
use rows::{StorageRow, StorageRowKey};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum StorageRequest {
    ChallengeIdentity {
        project_id: String,
        authenticated_actor: String,
    },
    ChallengeDisplay {
        state_json: String,
        rows: Vec<StorageRow>,
        public_only: bool,
    },
    ChallengePrepare {
        purpose: contributions::ChallengePurpose,
        project_id: String,
        authenticated_actor: String,
        owner_override: bool,
        input_json: String,
    },
    ChallengeExecute {
        request_json: String,
        state_json: String,
        rows: Vec<StorageRow>,
        prepared_json: Option<String>,
        now_ms: String,
        published_at: String,
    },
    ArchiveContext {
        request_json: String,
        project_root: String,
    },
    Prepare {
        request_json: String,
        authenticated_actor: Option<String>,
    },
    Execute {
        request_json: String,
        state_json: String,
        rows: Vec<StorageRow>,
        authenticated_actor: Option<String>,
        now_ms: String,
        published_at: String,
        next_commit: String,
    },
    Read {
        context: read::ReadContext,
        intent_json: String,
        rows: Vec<StorageRow>,
        completed: Vec<read::StorageQuery>,
    },
    Evaluate {
        request_json: String,
    },
    Rows {
        state_json: String,
    },
    ElementHeaders {
        rows: Vec<StorageRow>,
    },
    NameSearchQueries {
        request_json: String,
    },
    NameSearchSelection {
        request_json: String,
        rows: Vec<StorageRow>,
    },
    State {
        project_root: String,
        project_id: String,
        commit_version: String,
        rows: Vec<StorageRow>,
    },
    Publication {
        state_json: String,
        plan_json: String,
    },
    Contents {
        delta_json: String,
        request_json: Option<String>,
        rows: Vec<StorageRow>,
    },
    Collection {
        state_json: String,
        plan_json: String,
        policy_json: String,
        now_ms: String,
        next_commit: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(super) enum StorageResult {
    Read {
        queries: Vec<read::StorageQuery>,
        state_json: Option<String>,
    },
    Encoded {
        kind: String,
        value_json: String,
    },
    Rows(Vec<StorageRow>),
    Delta {
        expected_commit: String,
        commit_version: String,
        rows: Vec<StorageRow>,
        removed: Vec<StorageRowKey>,
        outcome_json: String,
    },
}

pub(super) fn evaluate(request: StorageRequest) -> SemanticRevisionResult<StorageResult> {
    match request {
        StorageRequest::ChallengeIdentity {
            project_id,
            authenticated_actor,
        } => encoded(
            "challenge_identity",
            &contributions::author(
                &format!("https://app.lumvise.com/{project_id}"),
                &authenticated_actor,
            ),
        ),
        StorageRequest::ChallengeDisplay {
            state_json,
            rows,
            public_only,
        } => contributions::views::display(&serde_json::from_str(&state_json)?, &rows, public_only),
        StorageRequest::ChallengePrepare {
            purpose,
            project_id,
            authenticated_actor,
            owner_override,
            input_json,
        } => contributions::prepare(
            purpose,
            &project_id,
            &authenticated_actor,
            owner_override,
            &input_json,
        ),
        StorageRequest::ChallengeExecute {
            request_json,
            state_json,
            rows,
            prepared_json,
            now_ms,
            published_at,
        } => contributions::execute(
            &request_json,
            &state_json,
            &rows,
            prepared_json.as_deref(),
            &SemanticRevisionTimestamp {
                unix_ms: integer(&now_ms)?,
                rfc3339: published_at,
            },
        ),
        StorageRequest::ArchiveContext {
            request_json,
            project_root,
        } => archives::initial(&request_json, &project_root),
        StorageRequest::Prepare {
            request_json,
            authenticated_actor,
        } => request::prepare(&request_json, authenticated_actor),
        StorageRequest::Execute {
            request_json,
            state_json,
            rows,
            authenticated_actor,
            now_ms,
            published_at,
            next_commit,
        } => request::execute(
            &request_json,
            &state_json,
            &rows,
            authenticated_actor,
            &SemanticRevisionTimestamp {
                rfc3339: published_at,
                unix_ms: integer(&now_ms)?,
            },
            integer(&next_commit)?,
        ),
        StorageRequest::Read {
            context,
            intent_json,
            rows,
            completed,
        } => read::plan(context, &intent_json, rows, completed),
        StorageRequest::Evaluate { request_json } => opaque_evaluation(&request_json),
        StorageRequest::Rows { state_json } => Ok(StorageResult::Rows(rows::encode(
            &serde_json::from_str(&state_json)?,
        )?)),
        StorageRequest::ElementHeaders { rows } => headers::project(rows),
        StorageRequest::NameSearchQueries { request_json } => names::queries(&request_json),
        StorageRequest::NameSearchSelection { request_json, rows } => {
            names::select(&request_json, rows)
        }
        StorageRequest::State {
            project_root,
            project_id,
            commit_version,
            rows,
        } => {
            let state = rows::decode(&project_root, &project_id, integer(&commit_version)?, rows)?;
            encoded("state", &state)
        }
        StorageRequest::Publication {
            state_json,
            plan_json,
        } => publication(&state_json, &plan_json),
        StorageRequest::Contents {
            delta_json,
            request_json,
            rows,
        } => content::attach(&delta_json, request_json.as_deref(), &rows),
        StorageRequest::Collection {
            state_json,
            plan_json,
            policy_json,
            now_ms,
            next_commit,
        } => collection(
            &state_json,
            &plan_json,
            &policy_json,
            integer(&now_ms)?,
            integer(&next_commit)?,
        ),
    }
}

fn opaque_evaluation(request_json: &str) -> SemanticRevisionResult<StorageResult> {
    let request = serde_json::from_str(request_json)?;
    if matches!(request, super::RevisionPolicyRequest::Storage { .. }) {
        return Err(SemanticRevisionError::invalid_value(
            "storage",
            "a non-recursive policy request",
        ));
    }
    let result = serde_json::to_value(super::evaluate(request)?)?;
    Ok(StorageResult::Encoded {
        kind: result["kind"]
            .as_str()
            .expect("tagged policy result")
            .into(),
        value_json: serde_json::to_string(&result["value"])?,
    })
}

fn publication(state_json: &str, plan_json: &str) -> SemanticRevisionResult<StorageResult> {
    let before: SemanticRevisionState = serde_json::from_str(state_json)?;
    let mut after = before.clone();
    let outcome = after.apply_publication(serde_json::from_str(plan_json)?)?;
    if after.selection("public") != before.selection("public") {
        after.snapshot("public", "")?;
    }
    delta(&before, &after, &outcome)
}

fn collection(
    state_json: &str,
    plan_json: &str,
    policy_json: &str,
    now_ms: i64,
    next_commit: i64,
) -> SemanticRevisionResult<StorageResult> {
    let before: SemanticRevisionState = serde_json::from_str(state_json)?;
    let mut after = before.clone();
    let receipt = after.apply_retention(
        &serde_json::from_str(plan_json)?,
        &serde_json::from_str(policy_json)?,
        now_ms,
        next_commit,
    )?;
    delta(&before, &after, &receipt)
}

fn delta(
    before: &SemanticRevisionState,
    after: &SemanticRevisionState,
    outcome: &impl Serialize,
) -> SemanticRevisionResult<StorageResult> {
    let previous = rows::index(rows::encode(before)?);
    let current = rows::index(rows::encode(after)?);
    let mut removed = previous
        .keys()
        .filter(|key| !current.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    for (key, row) in &current {
        if key.kind == "record"
            && previous
                .get(key)
                .is_some_and(|old| old.body_json != row.body_json)
        {
            removed.push(vectors::key(&serde_json::from_str(&key.identity)?)?);
        }
    }
    Ok(StorageResult::Delta {
        expected_commit: before.commit_version.to_string(),
        commit_version: after.commit_version.to_string(),
        removed,
        rows: current
            .iter()
            .filter(|(key, row)| previous.get(*key) != Some(*row))
            .map(|(_, row)| row.clone())
            .collect(),
        outcome_json: serde_json::to_string(outcome)?,
    })
}

fn encoded(kind: &str, value: &impl Serialize) -> SemanticRevisionResult<StorageResult> {
    Ok(StorageResult::Encoded {
        kind: kind.into(),
        value_json: serde_json::to_string(value)?,
    })
}

fn integer(value: &str) -> SemanticRevisionResult<i64> {
    value.parse::<i64>().map_err(|_| {
        SemanticRevisionError::invalid_value(
            value,
            "a signed 64-bit integer encoded as a decimal string",
        )
    })
}
