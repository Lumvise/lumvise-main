//! Real persistence for the close document tests; this module owns no revision policy.
use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{
    LocalPersistence, RelationalPersistence, SemanticOperation, SemanticPersistence,
};
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) struct DocumentTestPersistence {
    persistence: Arc<LocalPersistence>,
    broker: Arc<AppCoreHostCapabilityBroker>,
}

impl Default for DocumentTestPersistence {
    fn default() -> Self {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        let broker = AppCoreHostCapabilityBroker::new(semantic, relational)
            .with_index_client("document-close-test".into())
            .unwrap();
        Self {
            persistence,
            broker: Arc::new(broker),
        }
    }
}

impl DocumentTestPersistence {
    pub(super) fn seed_project(&self, root: &str, elements: Vec<Value>, relationships: Vec<Value>) {
        self.execute(SemanticOperation::SyncStructure {
            project_root: root.into(),
            elements: serde_json::from_value(json!(elements)).unwrap(),
            relationships: serde_json::from_value(json!(relationships)).unwrap(),
        });
    }

    pub(super) fn seed_artifact(&self, artifact: &Value) {
        // Malformed rows remain explicit read-only fault fixtures.
        if let Ok(artifact) = serde_json::from_value(artifact.clone()) {
            self.execute(SemanticOperation::UpsertArtifact {
                artifact,
                media_type: "text/markdown".into(),
            });
        }
    }

    fn execute(&self, operation: SemanticOperation) {
        SemanticPersistence::execute(
            self.persistence.as_ref(),
            operation,
            &lumvise_resource_routing::InvocationControl::sixty_seconds(),
        )
        .unwrap();
    }

    pub(super) fn invoke(
        &self,
        plugin: &str,
        capability: &str,
        input: Value,
    ) -> Result<Value, PluginError> {
        self.broker
            .invoke(HostCapabilityRequest {
                plugin_id: plugin.into(),
                invocation_id: "document-close-test".into(),
                call_id: "document-close-test".into(),
                capability_id: capability.into(),
                required_version: "^1.0".into(),
                input,
            })
            .map_err(|error| PluginError::new("document_test_storage", error.to_string(), false))
    }

    pub(super) fn ingest(&self, input: Value) -> Result<Value, PluginError> {
        let mut host = DocumentSemanticTransport { persistence: self };
        lumvise_plugin_semantic::SemanticPlugin.dispatch(
            "ingest_index_batch",
            input,
            &mut PluginContext::for_test(&mut host),
        )
    }
}

struct DocumentSemanticTransport<'a> {
    persistence: &'a DocumentTestPersistence,
}

impl HostCallTransport for DocumentSemanticTransport<'_> {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.persistence
            .invoke("builtin.semantic", capability, input)
    }
}
