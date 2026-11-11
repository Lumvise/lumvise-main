//! Durable connector identity and native Knowledge/source approval, owned by the host authority.
//! Native review calls the AppCore API; MCP can register/request but cannot decide a grant.
mod api;
#[cfg(test)]
mod enforcement_tests;
mod policy;
mod source_access;
#[cfg(test)]
mod tests;

use super::{KnowledgeAuthority, invalid, storage};
use lumvise_contracts::{
    ConnectorCredential, ConnectorGrantState, ConnectorIdentity, ConnectorKnowledgeGrant,
    ConnectorKnowledgeOperation, KnowledgeProjectIdentity,
};
use lumvise_plugin_runtime::HostCapabilityError;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub use api::{ConnectorGrantMaintainer, generate_connector_credential};

impl KnowledgeAuthority {
    pub(in crate::plugin) fn with_connector_review_actions(
        mut self,
        actions: Option<std::sync::Arc<std::sync::Mutex<Vec<Value>>>>,
    ) -> Self {
        self.connector_review_actions = actions;
        self
    }
    pub(crate) fn connector_proof_identity(
        &self,
        credential: Option<&ConnectorCredential>,
        control: &InvocationControl,
    ) -> Result<Option<String>, HostCapabilityError> {
        credential
            .map(|proof| {
                self.authenticate_connector(proof, control)
                    .map(|identity| identity.connector_id)
            })
            .transpose()
    }

    pub(crate) fn register_connector(
        &self,
        credential: &ConnectorCredential,
        display_name: &str,
        control: &InvocationControl,
    ) -> Result<ConnectorIdentity, HostCapabilityError> {
        let id = credential_identity(credential)?;
        let name = display_name.trim();
        if name.is_empty() {
            return Err(invalid(&json!(name), "nonempty connector display name"));
        }
        let row = json!(ConnectorIdentity {
            connector_id: id.clone(),
            display_name: name.into()
        });
        self.install_connector_row(&format!("connector:{id}"), row, control)?;
        self.connector_identity(&id, control)
    }

    pub(crate) fn authenticate_connector(
        &self,
        credential: &ConnectorCredential,
        control: &InvocationControl,
    ) -> Result<ConnectorIdentity, HostCapabilityError> {
        self.connector_identity(&credential_identity(credential)?, control)
    }

    fn connector_identity(
        &self,
        id: &str,
        control: &InvocationControl,
    ) -> Result<ConnectorIdentity, HostCapabilityError> {
        let key = format!("connector:{id}");
        let value = storage::read(self.relational.as_ref(), &key, control)?;
        serde_json::from_value(value.unwrap_or(Value::Null))
            .map_err(|_| invalid(&json!(id), "registered authenticated connector"))
    }

    pub(crate) fn require_connector_write(
        &self,
        id: &str,
        project: &KnowledgeProjectIdentity,
        operation: ConnectorKnowledgeOperation,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        let grant = self.request_connector_grant(id, project, control)?;
        if grant.state == ConnectorGrantState::Granted && grant.operations.contains(&operation) {
            return Ok(());
        }
        Err(HostCapabilityError::new(
            "runtime.knowledge_scope",
            "connector_approval_required",
            format!(
                "connector {id:?}, project {:?}, grant {:?}; expected maintainer-approved Knowledge {operation:?} permission in Settings",
                project.project_id, grant.state
            ),
            false,
        ))
    }

    fn request_connector_grant(
        &self,
        id: &str,
        project: &KnowledgeProjectIdentity,
        control: &InvocationControl,
    ) -> Result<ConnectorKnowledgeGrant, HostCapabilityError> {
        let connector = self.connector_identity(id, control)?;
        let grant = ConnectorKnowledgeGrant {
            connector,
            project: project.clone(),
            revision: 1,
            state: ConnectorGrantState::Pending,
            operations: Vec::new(),
        };
        let key = grant_key(id, project);
        if self.install_connector_row(&key, json!(grant), control)? {
            if let Some(actions) = &self.connector_review_actions {
                actions.lock().map_err(|_| invalid(&json!(id), "available native connector review queue"))?
                    .push(json!({"action":"connector.request_review","payload":{"project_root":project.project_root}}));
            }
        }
        self.read_connector_grant(&key, control)
    }

    fn read_connector_grant(
        &self,
        key: &str,
        control: &InvocationControl,
    ) -> Result<ConnectorKnowledgeGrant, HostCapabilityError> {
        let row = storage::read(self.relational.as_ref(), key, control)?;
        serde_json::from_value(row.unwrap_or(Value::Null))
            .map_err(|_| invalid(&json!(key), "stored connector Knowledge grant"))
    }

    fn install_connector_row(
        &self,
        key: &str,
        value: Value,
        control: &InvocationControl,
    ) -> Result<bool, HostCapabilityError> {
        storage::ensure(self.relational.as_ref(), control)?;
        if storage::read(self.relational.as_ref(), key, control)?.is_some() {
            return Ok(false);
        }
        let installed = storage::replace(self.relational.as_ref(), key, None, value, control);
        if installed.is_ok() {
            return Ok(true);
        }
        if storage::read(self.relational.as_ref(), key, control)?.is_some() {
            return Ok(false);
        }
        installed.map(|_| true)
    }
}

fn credential_identity(credential: &ConnectorCredential) -> Result<String, HostCapabilityError> {
    credential
        .validate()
        .map_err(|message| invalid(&json!("[redacted]"), &message))?;
    Ok(format!(
        "connector:{:x}",
        Sha256::digest(credential.0.as_bytes())
    ))
}

fn grant_key(id: &str, project: &KnowledgeProjectIdentity) -> String {
    let lineage = serde_json::to_vec(&(id, &project.project_id, &project.storage_authority))
        .expect("connector lineage serializes");
    format!("connector-grant:{:x}", Sha256::digest(lineage))
}
