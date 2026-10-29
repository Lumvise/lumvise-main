//! Capability adapter for the common source publication owner.
use super::{AppCoreHostCapabilityBroker, Arc, HostCapabilityError, InvocationControl, Value};

impl AppCoreHostCapabilityBroker {
    /// Supplies the embedding application's persisted identity for source indexing.
    /// Example: construct a broker with new(...).with_index_client(saved_uuid)? before use.
    pub fn with_index_client(self, client: String) -> crate::Result<Self> {
        crate::index_publication::install_index_client(&self.index_publication_client, client)?;
        Ok(self)
    }

    pub(super) fn invoke_index_publication(
        &self,
        input: Value,
        control: &InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        crate::index_publication::invoke_index_publication(
            Arc::clone(&self.semantic),
            Arc::clone(&self.relational),
            self.index_publication_client.get().map(String::as_str),
            input,
            control,
        )
        .map_err(|error| {
            HostCapabilityError::new(
                "project.index",
                "source_publication_failed",
                error.to_string(),
                false,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppCore;
    use lumvise_plugin_runtime::{
        HostCapabilityBroker, HostCapabilityRequest, PluginInvocationClass, PluginInvocationContext,
    };
    use serde_json::json;
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    fn prepare_request(root: &Path) -> HostCapabilityRequest {
        HostCapabilityRequest {
            plugin_id: "builtin.canvas".into(),
            invocation_id: "source-import".into(),
            call_id: "prepare".into(),
            capability_id: "project.index".into(),
            required_version: "^1.0".into(),
            input: json!({"operation":"prepare", "folder_path":root, "project_root":"/source-test", "provider_instance_id":"builtin-canvas"}),
        }
    }

    #[test]
    fn source_broker_uses_the_same_identity_installed_after_app_construction() {
        let app = AppCore::in_memory().unwrap();
        let broker = app.default_plugin_host_capability_broker().unwrap();
        let root = tempfile::tempdir().unwrap();
        let error = broker.invoke(prepare_request(root.path())).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("persisted startup client identity"),
            "{error}"
        );
        app.install_index_publication_client("saved-device".into())
            .unwrap();
        assert_eq!(
            broker.index_publication_client.get().map(String::as_str),
            Some("saved-device")
        );
        let prepared = broker.invoke(prepare_request(root.path())).unwrap();
        assert_eq!(prepared["state"], "scan");
        assert_eq!(prepared["requires_full_scan"], true);
        assert!(
            prepared["base_token"]
                .as_str()
                .is_some_and(|token| token.len() == 64)
        );
        assert!(
            app.install_index_publication_client("another-device".into())
                .is_err()
        );
    }

    #[test]
    fn cancelled_and_expired_source_calls_do_not_prepare_a_checkpoint() {
        let app = AppCore::in_memory().unwrap();
        app.install_index_publication_client("saved-device".into())
            .unwrap();
        let broker = app.default_plugin_host_capability_broker().unwrap();
        let root = tempfile::tempdir().unwrap();
        let cancelled = PluginInvocationContext::new(
            "cancelled",
            "source-test",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        );
        cancelled.cancellation().cancel();
        let expired = PluginInvocationContext::new(
            "expired",
            "source-test",
            PluginInvocationClass::Foreground,
            Instant::now() - Duration::from_secs(1),
        );
        for context in [&cancelled, &expired] {
            assert!(
                broker
                    .invoke_controlled(prepare_request(root.path()), context)
                    .is_err()
            );
        }
        let roots = broker
            .semantic
            .execute(
                lumvise_db_core::SemanticOperation::ProjectRoots,
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
        assert!(
            matches!(roots, lumvise_db_core::SemanticResult::ProjectRoots(roots) if roots.is_empty())
        );
    }
}
