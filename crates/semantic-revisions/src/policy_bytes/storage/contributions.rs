//! Gateway-owned challenge acceptance. Generic revision requests cannot enter this purpose.
use super::*;
use serde_json::{Value, json};
pub(super) mod context;
pub(super) mod views;
mod writes;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::policy_bytes) enum ChallengePurpose {
    Contribution,
    Vote,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChallengeRequest {
    pub purpose: ChallengePurpose,
    pub project_id: String,
    pub actor: String,
    pub owner_override: bool,
    pub input: Value,
    pub operation_id: String,
    pub report_id: String,
    pub identity_domain: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ChallengeReceipt {
    operation_id: String,
    outcome_json: String,
}

pub(super) fn prepare(
    purpose: ChallengePurpose,
    project: &str,
    actor: &str,
    owner_override: bool,
    input: &str,
) -> SemanticRevisionResult<StorageResult> {
    crate::revision_index::require_project_identity(project)?;
    if actor.trim().is_empty() {
        return Err(invalid(actor, "an authenticated challenge actor"));
    }
    let input: Value = serde_json::from_str(input)?;
    let identity_domain = format!("https://app.lumvise.com/{project}");
    let digest = crate::revision_index::digest(&(&identity_domain, actor, &purpose, &input))?;
    let operation_id = format!("challenge-contribution:operation:{digest}");
    let report_id = input["reportId"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("challenge-report:{digest}"));
    let request = ChallengeRequest {
        purpose,
        project_id: project.into(),
        actor: actor.into(),
        owner_override,
        input,
        operation_id,
        report_id,
        identity_domain,
    };
    request.validate()?;
    encoded(
        "challenge_request",
        &json!({"request_json":serde_json::to_string(&request)?,"intent_json":serde_json::to_string(&read::ReadIntent::Challenge { request:request.clone() })?,"identity_domain":request.identity_domain,"report_id":request.report_id}),
    )
}

impl ChallengeRequest {
    pub(super) fn validate(&self) -> SemanticRevisionResult<()> {
        if self.actor.is_empty()
            || self.report_id.is_empty()
            || !self
                .operation_id
                .starts_with("challenge-contribution:operation:")
        {
            return Err(invalid(
                &self.operation_id,
                "a gateway-owned challenge operation",
            ));
        }
        Ok(())
    }
    pub(super) fn author(&self) -> String {
        author(&self.identity_domain, &self.actor)
    }
}

pub(super) fn author(domain: &str, actor: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "contributor:{:x}",
        Sha256::digest(format!("{domain}/{actor}").as_bytes())
    )
}

pub(super) fn execute(
    request: &str,
    state: &str,
    rows: &[StorageRow],
    prepared: Option<&str>,
    now: &SemanticRevisionTimestamp,
) -> SemanticRevisionResult<StorageResult> {
    let request: ChallengeRequest = serde_json::from_str(request)?;
    request.validate()?;
    let state: SemanticRevisionState = serde_json::from_str(state)?;
    if request.project_id != state.project_id {
        return Err(invalid(request.project_id, "the accepted source lineage"));
    }
    if let Some(outcome) = replay(&request, &state, rows)? {
        return encoded("challenge_replay", &outcome);
    }
    let Some(prepared) = prepared else {
        return context::project(&request, &state, rows);
    };
    writes::accept(&request, &state, rows, prepared, now)
}

fn replay(
    request: &ChallengeRequest,
    state: &SemanticRevisionState,
    rows: &[StorageRow],
) -> SemanticRevisionResult<Option<Value>> {
    if let Some(row) = rows
        .iter()
        .find(|row| is_receipt(row) && row.key.identity == request.operation_id)
    {
        let receipt: ChallengeReceipt = serde_json::from_str(&row.metadata_json)?;
        return Ok(Some(
            json!({"outcome":serde_json::from_str::<Value>(&receipt.outcome_json)?,"unavailable":false}),
        ));
    }
    if state
        .accepted_operations
        .iter()
        .any(|operation| operation.receipt.operation_id == request.operation_id)
    {
        return Ok(Some(json!({"outcome":null,"unavailable":true})));
    }
    Ok(None)
}

pub(super) fn receipt(operation: &str, outcome: &Value) -> SemanticRevisionResult<StorageRow> {
    Ok(StorageRow {
        key: StorageRowKey {
            kind: "challenge_receipt".into(),
            identity: operation.into(),
        },
        metadata_json: serde_json::to_string(&ChallengeReceipt {
            operation_id: operation.into(),
            outcome_json: serde_json::to_string(outcome)?,
        })?,
        body_json: None,
        indexes: vec![],
    })
}

pub(super) fn is_receipt(row: &StorageRow) -> bool {
    row.key.kind == "challenge_receipt"
}

pub(super) fn validate_receipt(row: &StorageRow) -> SemanticRevisionResult<()> {
    let receipt: ChallengeReceipt = serde_json::from_str(&row.metadata_json)?;
    if receipt.operation_id != row.key.identity
        || !receipt
            .operation_id
            .starts_with("challenge-contribution:operation:")
        || row.body_json.is_some()
    {
        return Err(invalid(
            &row.key.identity,
            "an exact private challenge outcome receipt",
        ));
    }
    serde_json::from_str::<Value>(&receipt.outcome_json)?;
    Ok(())
}

pub(super) fn logical(artifact: &SemanticArtifact) -> SemanticRevisionResult<&str> {
    artifact
        .metadata
        .get("contributionId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            invalid(
                &artifact.artifact_id,
                "a purpose-owned contribution identity",
            )
        })
}

pub(super) fn selection(logical: &str) -> SemanticRevisionResult<String> {
    Ok(format!(
        "challenge-contribution:{}",
        crate::revision_index::digest(&logical)?
    ))
}

pub(super) fn invalid(value: impl Into<String>, expected: &str) -> SemanticRevisionError {
    SemanticRevisionError::invalid_value(value, expected)
}
