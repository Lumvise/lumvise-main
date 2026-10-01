//! Public recurring-task API backed only by ready compiled plugin exports.

use crate::PluginInvocationStatus;
use lumvise_plugin_package::BackgroundDeliveryPolicy;
use lumvise_plugin_runtime::BackgroundExportKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{PluginEndpoints, Result};

/// One signed recurring task currently published by a ready compiled plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRecurringTask {
    pub plugin_id: String,
    pub task_id: String,
    pub description: String,
    pub interval_seconds: u64,
    pub delivery: BackgroundDeliveryPolicy,
}

/// Outcome of one durable recurring delivery attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginRecurringTaskRun {
    pub plugin_id: String,
    pub task_id: String,
    pub delivery_id: String,
    pub attempt: u32,
    pub status: PluginInvocationStatus,
    pub output: Value,
    pub job_id: Option<String>,
}

impl PluginEndpoints<'_> {
    /// Lists recurring tasks from ready compiled plugins only.
    pub fn plugin_recurring_tasks(&self) -> Result<Vec<PluginRecurringTask>> {
        Ok(self
            .app
            .plugin_system()
            .background_exports()?
            .into_iter()
            .filter_map(recurring_task)
            .collect())
    }

    /// Enqueues and invokes recurring deliveries due at the supplied Unix second.
    pub fn run_due_plugin_recurring_tasks(
        &self,
        now_unix_seconds: i64,
    ) -> Result<Vec<PluginRecurringTaskRun>> {
        let exports = self.app.plugin_system().background_exports()?;
        self.enqueue_due_recurring(&exports, now_unix_seconds)?;
        Ok(self
            .process_due_background_kind(&exports, now_unix_seconds, "recurring_task")?
            .into_iter()
            .filter(|run| run.delivery.delivery_kind == "recurring_task")
            .map(task_run)
            .collect())
    }
}

fn recurring_task(
    export: lumvise_plugin_runtime::PublishedBackgroundExport,
) -> Option<PluginRecurringTask> {
    let BackgroundExportKind::RecurringTask {
        interval_seconds,
        delivery,
    } = export.kind
    else {
        return None;
    };
    Some(PluginRecurringTask {
        plugin_id: export.plugin_id,
        task_id: export.export_id.clone(),
        description: export.export_id,
        interval_seconds,
        delivery,
    })
}

fn task_run(run: super::background_delivery::BackgroundDeliveryRun) -> PluginRecurringTaskRun {
    let job_id = run
        .output
        .get("job_id")
        .and_then(Value::as_str)
        .or_else(|| run.output.pointer("/job/job_id").and_then(Value::as_str))
        .map(str::to_owned);
    PluginRecurringTaskRun {
        plugin_id: run.delivery.plugin_id,
        task_id: run.delivery.export_id,
        delivery_id: run.delivery.delivery_id,
        attempt: run.delivery.attempts,
        status: run.status,
        output: run.output,
        job_id,
    }
}
