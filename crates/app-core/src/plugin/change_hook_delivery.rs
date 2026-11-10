//! Fixed-cadence delivery of graph change-hook batches to internal consumers.

use std::collections::{BTreeSet, HashSet};

use lumvise_db_core::{
    ChangeDisposition, ChangeHookRegistration, ChangeHookScope, RelationalOperation,
    RelationalResult, SemanticOperation, SemanticResult, StoredArtifactTextVector,
    StoredSemanticElementNameVector,
};
#[cfg(test)]
use lumvise_plugin_protocol::{
    CURRENT_PROTOCOL_VERSION, FrameCodec, MessageBody, StorageTriggerResponse, WireMessage,
    WireOutcome,
};
use lumvise_plugin_protocol::{
    StorageTriggerChanged, StorageTriggerDisposition, StorageTriggerRequest,
};
use lumvise_plugin_runtime::{BackgroundExportKind, PublishedBackgroundExport};
use lumvise_resource_routing::InvocationControl;
use serde_json::Value;
use tracing::error;

use crate::{AppCoreError, PluginEndpoints, Result};
mod vector;

const STORAGE_HOOK_PREFIX: &str = "storage-trigger:";
const VECTOR_HOOK_PREFIX: &str = "vector-reindex:";
const HOOK_BATCH_SIZE: usize = 128;

/// Observable result of one coordinator sweep. Delivery remains opaque to callers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeHookCycleReport {
    pub hooks: Vec<ChangeHookRun>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeHookRun {
    pub hook_name: String,
    pub base_revision: i64,
    pub target_revision: i64,
    pub delivered: usize,
    pub retrying: usize,
    pub dead_lettered: usize,
    pub acknowledged: bool,
}
impl PluginEndpoints<'_> {
    /// Runs one deterministic change-hook coordinator cycle. This is public so
    /// embeddings and focused tests can drive the worker without sleeping.
    pub fn run_change_hook_cycle(&self, now_unix_seconds: i64) -> Result<ChangeHookCycleReport> {
        let exports = self.app.plugin_system().background_exports()?;
        self.ensure_internal_hooks(&exports)?;
        let registrations = self.change_hook_registrations()?;
        let mut report = ChangeHookCycleReport::default();
        for registration in registrations {
            // One registration stuck on unfixable data (e.g. an artifact whose
            // owning semantic element was deleted out from under it) must never
            // starve every other project's delivery forever: isolate the
            // failure and keep sweeping the rest of the list.
            if registration.hook_name.starts_with(VECTOR_HOOK_PREFIX) {
                let Some(vectorizer) = self.plugin_vectorizer()? else {
                    continue;
                };
                let hook_name = registration.hook_name.clone();
                match self.run_vector_hook(registration, vectorizer.as_ref()) {
                    Ok(run) => report.hooks.push(run),
                    Err(error) => log_hook_error(&hook_name, &error.to_string()),
                }
                continue;
            }
            let Some(export) = storage_hook_export(&exports, &registration) else {
                continue;
            };
            let hook_name = registration.hook_name.clone();
            match self.run_registered_hook(&exports, export, registration, now_unix_seconds) {
                Ok(run) => report.hooks.push(run),
                Err(error) => log_hook_error(&hook_name, &error.to_string()),
            }
        }
        Ok(report)
    }

    fn ensure_internal_hooks(&self, exports: &[PublishedBackgroundExport]) -> Result<()> {
        let roots = match self.app.semantic.execute(
            SemanticOperation::ProjectRoots,
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ProjectRoots(roots) => roots,
            result => return Err(semantic_result_error("project roots", result)),
        };
        for root in &roots {
            self.register_hook(
                format!("{VECTOR_HOOK_PREFIX}{root}"),
                root.clone(),
                BTreeSet::new(),
            )?;
            for export in exports.iter().filter(external_storage_export) {
                let BackgroundExportKind::StorageTrigger { entity_kinds, .. } = &export.kind else {
                    continue;
                };
                self.register_hook(
                    format!(
                        "{STORAGE_HOOK_PREFIX}{}:{}:{}",
                        export.plugin_id, export.export_id, root
                    ),
                    root.clone(),
                    entity_kinds.iter().cloned().collect(),
                )?;
            }
        }
        let desired = roots
            .iter()
            .flat_map(|root| {
                let vector = format!("{VECTOR_HOOK_PREFIX}{root}");
                let storage = exports
                    .iter()
                    .filter(|export| {
                        matches!(export.kind, BackgroundExportKind::StorageTrigger { .. })
                    })
                    .map(move |export| {
                        format!(
                            "{STORAGE_HOOK_PREFIX}{}:{}:{root}",
                            export.plugin_id, export.export_id
                        )
                    });
                std::iter::once(vector).chain(storage)
            })
            .collect::<BTreeSet<_>>();
        let registrations = match self.app.semantic.execute(
            SemanticOperation::ChangeHookRegistrations,
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookRegistrations(registrations) => registrations,
            result => return Err(semantic_result_error("change hook registrations", result)),
        };
        for registration in registrations {
            if !desired.contains(&registration.hook_name) {
                self.deregister_hook(registration.hook_name)?;
            }
        }
        Ok(())
    }

    fn register_hook(
        &self,
        hook_name: String,
        project_root: String,
        entity_kinds: BTreeSet<String>,
    ) -> Result<()> {
        let result = self.app.semantic.execute(
            SemanticOperation::RegisterChangeHook {
                hook_name,
                scope: ChangeHookScope {
                    project_root,
                    entity_kinds,
                },
            },
            &InvocationControl::sixty_seconds(),
        )?;
        if matches!(result, SemanticResult::ChangeHookRegistration(_)) {
            Ok(())
        } else {
            Err(semantic_result_error("register change hook", result))
        }
    }

    fn deregister_hook(&self, hook_name: String) -> Result<()> {
        let result = self.app.semantic.execute(
            SemanticOperation::DeregisterChangeHook { hook_name },
            &InvocationControl::sixty_seconds(),
        )?;
        if matches!(result, SemanticResult::ChangeHookDeregistered { .. }) {
            Ok(())
        } else {
            Err(semantic_result_error("deregister change hook", result))
        }
    }
    fn change_hook_registrations(&self) -> Result<Vec<ChangeHookRegistration>> {
        match self.app.semantic.execute(
            SemanticOperation::ChangeHookRegistrations,
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookRegistrations(registrations) => Ok(registrations),
            result => Err(semantic_result_error("change hook registrations", result)),
        }
    }

    fn acknowledge_change_hook(&self, batch: lumvise_db_core::ChangeBatch) -> Result<bool> {
        match self.app.semantic.execute(
            SemanticOperation::AcknowledgeChangeHook { batch },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookAcknowledged { advanced } => Ok(advanced),
            result => Err(semantic_result_error("acknowledge change hook", result)),
        }
    }

    fn run_registered_hook(
        &self,
        exports: &[PublishedBackgroundExport],
        export: &PublishedBackgroundExport,
        registration: ChangeHookRegistration,
        now_unix_seconds: i64,
    ) -> Result<ChangeHookRun> {
        let batch = match self.app.semantic.execute(
            SemanticOperation::ChangeHookDirtyBatch {
                hook_name: registration.hook_name.clone(),
                maximum_elements: HOOK_BATCH_SIZE,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookBatch(batch) => batch,
            result => return Err(semantic_result_error("change hook dirty batch", result)),
        };
        let mut run = ChangeHookRun {
            hook_name: registration.hook_name.clone(),
            base_revision: batch.base_revision,
            target_revision: batch.target_revision,
            delivered: 0,
            retrying: 0,
            dead_lettered: 0,
            acknowledged: false,
        };
        let deliveries = if batch.changed.is_empty() {
            Vec::new()
        } else {
            let source_key = format!(
                "{}:{}:{}",
                registration.hook_name, batch.base_revision, batch.target_revision
            );
            let delivery_id = change_hook_delivery_id(export, &source_key);
            let payload = change_hook_payload(&registration.scope.project_root, &batch)?;
            vec![(source_key, delivery_id, payload)]
        };
        if !deliveries.is_empty() {
            let result = self.app.relational.execute(
                RelationalOperation::EnqueueChangeHookBatch {
                    plugin_id: export.plugin_id.clone(),
                    export_id: export.export_id.clone(),
                    now_unix_seconds,
                    deliveries: deliveries
                        .iter()
                        .map(|(source_key, _, payload)| (source_key.clone(), payload.clone()))
                        .collect(),
                },
                &InvocationControl::sixty_seconds(),
            )?;
            if !matches!(result, RelationalResult::ChangeHookBatchEnqueued { .. }) {
                return Err(relational_result_error("enqueue change-hook batch", result));
            }
            // Reuse the same 60-second invocation, lease, retry, and dead-letter
            // path as all durable background exports.
            self.process_due_background_kind(exports, now_unix_seconds, "change_hook")?;
        }
        for (_, delivery_id, _) in &deliveries {
            let delivery = match self.app.relational.execute(
                RelationalOperation::DeliveryDiagnostics {
                    delivery_id: delivery_id.clone(),
                },
                &InvocationControl::sixty_seconds(),
            )? {
                RelationalResult::BackgroundDelivery(delivery) => delivery,
                result => return Err(relational_result_error("change-hook diagnostics", result)),
            };
            match delivery.as_ref().map(|delivery| delivery.state.as_str()) {
                Some("completed") => run.delivered = batch.changed.len(),
                Some("dead_letter") => run.dead_lettered = batch.changed.len(),
                _ => run.retrying = batch.changed.len(),
            }
        }
        if run.retrying == 0 {
            let result = self.app.semantic.execute(
                SemanticOperation::AcknowledgeChangeHook { batch },
                &InvocationControl::sixty_seconds(),
            )?;
            run.acknowledged = match result {
                SemanticResult::ChangeHookAcknowledged { advanced } => advanced,
                result => return Err(semantic_result_error("acknowledge change hook", result)),
            };
        }
        Ok(run)
    }
}

fn log_hook_error(hook_name: &str, message: &str) {
    error!(
        target: "app-core::plugin_background_driver",
        export_kind = "change_hook_coordinator",
        hook_name = hook_name,
        message = message,
        "plugin background driver error"
    );
}
fn external_storage_export(export: &&PublishedBackgroundExport) -> bool {
    matches!(export.kind, BackgroundExportKind::StorageTrigger { .. })
}

fn storage_hook_export<'a>(
    exports: &'a [PublishedBackgroundExport],
    registration: &ChangeHookRegistration,
) -> Option<&'a PublishedBackgroundExport> {
    let suffix = registration.hook_name.strip_prefix(STORAGE_HOOK_PREFIX)?;
    let mut parts = suffix.splitn(3, ':');
    let plugin_id = parts.next()?;
    let export_id = parts.next()?;
    let root = parts.next()?;
    if root != registration.scope.project_root {
        return None;
    }
    exports.iter().find(|export| {
        export.plugin_id == plugin_id
            && export.export_id == export_id
            && matches!(export.kind, BackgroundExportKind::StorageTrigger { .. })
    })
}

fn change_hook_delivery_id(export: &PublishedBackgroundExport, source_key: &str) -> String {
    format!(
        "change_hook:{}:{}:{source_key}",
        export.plugin_id, export.export_id
    )
}

fn change_hook_payload(project_root: &str, batch: &lumvise_db_core::ChangeBatch) -> Result<Value> {
    let request = StorageTriggerRequest {
        project_root: project_root.to_owned(),
        base_revision: batch.base_revision,
        target_revision: batch.target_revision,
        changed: batch
            .changed
            .iter()
            .map(|changed| StorageTriggerChanged {
                entity_id: changed.element_id.clone(),
                entity_kind: changed.entity_kind.clone(),
                disposition: match changed.disposition {
                    ChangeDisposition::Upserted => StorageTriggerDisposition::Upserted,
                    ChangeDisposition::Removal => StorageTriggerDisposition::Removal,
                },
            })
            .collect(),
    };
    request.to_value().map_err(|error| {
        AppCoreError::invalid_value(error.to_string(), "valid StorageTrigger batch")
    })
}

fn semantic_result_error(operation: &str, result: SemanticResult) -> AppCoreError {
    AppCoreError::unsupported(
        operation,
        format!("unexpected semantic persistence result: {result:?}"),
    )
}

fn relational_result_error(operation: &str, result: RelationalResult) -> AppCoreError {
    AppCoreError::unsupported(
        operation,
        format!("unexpected relational persistence result: {result:?}"),
    )
}

#[cfg(test)]
mod tests;
