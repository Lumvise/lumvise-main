//! Native imports start one lifecycle; delegated imports use the supplied nested runtime port.
use super::*;
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{
    HostCapabilityError, HostPluginInvocation, HostPluginInvoker, PluginInvocationClass,
    PluginInvocationContext,
};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_NATIVE_IMPORT: AtomicU64 = AtomicU64::new(1);

pub(super) struct NativeImportPublisher<'a> {
    app: &'a AppCore,
    context: PluginInvocationContext,
}

impl<'a> NativeImportPublisher<'a> {
    pub(super) fn new(app: &'a AppCore) -> Self {
        let sequence = NEXT_NATIVE_IMPORT.fetch_add(1, Ordering::Relaxed);
        Self {
            app,
            context: PluginInvocationContext::new(
                format!("native-import-{}-{sequence}", std::process::id()),
                IMPORTER_PLUGIN_ID,
                PluginInvocationClass::Foreground,
                Instant::now() + Duration::from_secs(60),
            ),
        }
    }

    pub(super) fn control(&self) -> InvocationControl {
        InvocationControl::with_deadline(
            self.context
                .deadline()
                .saturating_duration_since(Instant::now()),
        )
    }
}

impl HostPluginInvoker for NativeImportPublisher<'_> {
    fn invoke_plugin(&self, request: HostPluginInvocation) -> Result<Value, HostCapabilityError> {
        if request.plugin_id != SEMANTIC_PLUGIN_ID {
            return Err(HostCapabilityError::new(
                "project.import",
                "invalid_import_target",
                format!("target {}; expected builtin.semantic", request.plugin_id),
                false,
            ));
        }
        let export_id = request.export_id.clone();
        let outcome = self
            .app
            .plugin_system()
            .invoke_controlled(lumvise_plugin_runtime::PluginInvocationRequest::new(
                request.plugin_id,
                request.export_id,
                request.input,
                self.context.clone(),
            ))
            .map_err(|error| {
                HostCapabilityError::new(
                    "project.import",
                    "semantic_unavailable",
                    format!("builtin.semantic/{export_id} unavailable: {error}"),
                    false,
                )
            })?;
        match outcome {
            WireOutcome::Succeeded { value } => Ok(value),
            WireOutcome::Failed { error } => Err(HostCapabilityError::new(
                "project.import",
                error.code,
                error.message,
                error.retryable,
            )),
        }
    }
}
