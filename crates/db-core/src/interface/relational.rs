use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::semantic::PersistenceResult;
use super::types::{
    McpInstance, PluginBackgroundDelivery, PluginBackgroundRegistration,
    PluginBackgroundRegistrationSpec, PluginDataMutation, PluginDataMutationResult,
    PluginDataRowRecord, PluginDataRowsPage, PluginDataTableRecord, PluginSettingsRecord,
    SettingRecord,
};

/// Owned, engine-neutral relational persistence requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RelationalOperation {
    GetPersistentSetting {
        scope: String,
        key: String,
    },
    SetPersistentSetting {
        scope: String,
        key: String,
        value: Value,
    },
    GetPluginSetting {
        plugin_id: String,
    },
    SetPluginSetting {
        plugin_id: String,
        enabled: bool,
        config: Value,
    },
    EnsurePluginDataTable {
        plugin_id: String,
        table_name: String,
        schema: Value,
    },
    UpdatePluginDataTable {
        plugin_id: String,
        table_name: String,
        schema: Value,
    },
    ListPluginDataTables {
        plugin_id: String,
    },
    PutPluginData {
        plugin_id: String,
        table_name: String,
        row_key: String,
        value: Value,
    },
    GetPluginData {
        plugin_id: String,
        table_name: String,
        row_key: String,
    },
    DeletePluginData {
        plugin_id: String,
        table_name: String,
        row_key: String,
    },
    ListPluginData {
        plugin_id: String,
        table_name: String,
    },
    PagePluginData {
        plugin_id: String,
        table_name: String,
        key_prefix: Option<String>,
        after_key: Option<String>,
        limit: usize,
    },
    TrimPluginData {
        plugin_id: String,
        table_name: String,
        retained_rows: usize,
    },
    ApplyMutations {
        plugin_id: String,
        mutations: Vec<PluginDataMutation>,
    },
    SyncBackgroundRegistrations {
        registrations: Vec<PluginBackgroundRegistrationSpec>,
    },
    ListBackgroundRegistrations,
    EnqueueRecurring {
        plugin_id: String,
        export_id: String,
        scheduled_at: i64,
        now_unix_seconds: i64,
        interval_seconds: u64,
        payload: Value,
    },
    EnqueueChangeHookBatch {
        plugin_id: String,
        export_id: String,
        now_unix_seconds: i64,
        deliveries: Vec<(String, Value)>,
    },
    DueDeliveriesByKind {
        delivery_kind: String,
        now_unix_seconds: i64,
        limit: usize,
    },
    ClaimDelivery {
        delivery_id: String,
        now_unix_seconds: i64,
        lease_seconds: i64,
    },
    CompleteDelivery {
        delivery_id: String,
    },
    RetryDelivery {
        delivery_id: String,
        next_attempt_at: i64,
        error: String,
    },
    DeadLetterDelivery {
        delivery_id: String,
        error: String,
    },
    PruneDeadLetters {
        plugin_id: String,
        export_id: String,
        updated_before: String,
        max_entries: u32,
    },
    DeliveryDiagnostics {
        delivery_id: String,
    },
    RegisterMcpInstance {
        instance_id: String,
        project_root: String,
        display_name: String,
        capabilities_json: String,
        control_channel_json: Option<String>,
    },
    HeartbeatMcpInstance {
        instance_id: String,
        project_root: String,
        status: String,
    },
    ActiveMcpInstances,
    AllMcpInstances,
    DeregisterMcpInstance {
        instance_id: String,
    },
}

/// Every successful relational operation returns the data it observed or wrote.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RelationalResult {
    PersistentSetting(Option<SettingRecord>),
    PersistentSettingUpdated(SettingRecord),
    PluginSetting(Option<PluginSettingsRecord>),
    PluginSettingUpdated(PluginSettingsRecord),
    PluginDataTable(PluginDataTableRecord),
    PluginDataTables(Vec<PluginDataTableRecord>),
    PluginDataRow(Option<PluginDataRowRecord>),
    PluginDataRows(Vec<PluginDataRowRecord>),
    PluginDataPage(PluginDataRowsPage),
    PluginDataTrimmed { removed_rows: usize },
    ChangeHookBatchEnqueued { inserted: usize },
    PluginDataMutations(PluginDataMutationResult),
    BackgroundRegistrations(Vec<PluginBackgroundRegistration>),
    BackgroundDelivery(Option<PluginBackgroundDelivery>),
    BackgroundDeliveries(Vec<PluginBackgroundDelivery>),
    DeliveryTransition { changed: bool },
    DeadLettersPruned { removed_deliveries: usize },
    McpInstance(McpInstance),
    McpInstances(Vec<McpInstance>),
    McpInstanceDeregistered { instance_id: String },
}

pub trait RelationalPersistence: Send + Sync {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &lumvise_resource_routing::InvocationControl,
    ) -> PersistenceResult<RelationalResult>;
    fn readiness(&self) -> PersistenceResult<RelationalReadiness>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationalReadiness {
    pub ready: bool,
}
