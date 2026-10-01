use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernanceRuleRequest {
    pub title: String,
    pub content: String,
    pub source: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernanceRuleRecord {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub source: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernanceRuleResponse {
    pub rule: GovernanceRuleRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernanceRulesResponse {
    pub rules: Vec<GovernanceRuleRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernanceRuleCandidate {
    pub id: String,
    pub title: String,
    pub source_path: String,
    pub content: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub globs: Option<Vec<String>>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub global: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticGuidanceEvaluateRequest {
    pub query: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub verification: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub local_rules: Vec<GovernanceRuleCandidate>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub stale_after_days: Option<u64>,
    #[serde(default)]
    pub semantic_guidance_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticGuidanceEvaluateResponse {
    pub verification: String,
    pub verification_ok: bool,
    pub rules: Vec<GovernanceRuleCandidate>,
    pub warnings: Vec<String>,
    pub cooldown_hit: bool,
    pub stale_refresh_recommended: bool,
    pub delivered_at: String,
    pub next_refresh_after: Option<String>,
    pub guidance_text: String,
}
