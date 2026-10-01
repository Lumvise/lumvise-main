use serde::{Deserialize, Serialize};

use crate::{DatabaseStatus, NeuralRuntimeStatus};

pub const API_VERSION: &str = "2026-04-29.remake-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Daemon,
    Desktop,
    Test,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]

pub struct HealthResponse {
    pub ok: bool,
    pub app_name: String,
    pub api_version: String,
    pub runtime_mode: RuntimeMode,
    pub database: DatabaseStatus,
    pub neural: NeuralRuntimeStatus,
    pub registered_mcp_instances: usize,
}
