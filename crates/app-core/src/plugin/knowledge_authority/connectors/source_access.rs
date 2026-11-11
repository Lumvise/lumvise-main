//! Owns durable connector read/index grants; native review is the only decision writer.
mod api;
#[cfg(test)]
mod tests;

use super::*;
use crate::plugin::host_capability_broker::SourceImportAuthorization;
use lumvise_contracts::{
    ConnectorSourceAccess, ConnectorSourceAccessChange, ConnectorSourceFolder,
    ConnectorSourceFolderGrant, ConnectorSourceMode,
};
use lumvise_plugin_runtime::PluginInvocationContext;
use std::path::{Path, PathBuf};

struct SourceFolderAccess<'a> {
    authority: &'a KnowledgeAuthority,
    access: ConnectorSourceAccess,
    folder: ConnectorSourceFolder,
    control: &'a InvocationControl,
}

impl KnowledgeAuthority {
    pub(in crate::plugin) fn require_source_folder(
        &self,
        context: &PluginInvocationContext,
        policy: &SourceImportAuthorization,
        requested: &str,
        control: &InvocationControl,
        request_approval: bool,
    ) -> Result<PathBuf, HostCapabilityError> {
        let id = self.source_connector(context)?;
        let access = self.source_access(&id, control)?;
        let folder = SourceImportAuthorization::inspect_folder(requested)?;
        SourceFolderAccess {
            authority: self,
            access,
            folder,
            control,
        }
        .require(policy, request_approval)
    }

    fn source_connector(
        &self,
        context: &PluginInvocationContext,
    ) -> Result<String, HostCapabilityError> {
        let origin = context
            .mcp_origin()
            .ok_or_else(|| source_denied("missing origin", "authenticated MCP connector"))?;
        let id = origin.connector_id.as_deref().ok_or_else(|| {
            source_denied("unidentified connector", "authenticated MCP connector")
        })?;
        self.mcp_membership
            .require_connector(&origin.connection_id, Some(id))
            .map_err(|error| source_denied(id, &error))?;
        Ok(id.into())
    }

    fn source_access(
        &self,
        id: &str,
        control: &InvocationControl,
    ) -> Result<ConnectorSourceAccess, HostCapabilityError> {
        let connector = self.connector_identity(id, control)?;
        let stored = storage::read(self.relational.as_ref(), &source_key(id), control)?;
        match stored {
            Some(value) => checked_source_record(value, &connector),
            None => Ok(ConnectorSourceAccess {
                connector,
                revision: 0,
                mode: ConnectorSourceMode::Restricted,
                autoapprove_roots: Vec::new(),
                grants: Vec::new(),
            }),
        }
    }

    fn save_source_access(
        &self,
        previous: &ConnectorSourceAccess,
        next: &mut ConnectorSourceAccess,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        next.revision = previous.revision.checked_add(1).ok_or_else(|| {
            source_denied(
                &previous.revision.to_string(),
                "incrementable source access revision",
            )
        })?;
        let expected = (previous.revision != 0).then(|| json!(previous));
        storage::replace(
            self.relational.as_ref(),
            &source_key(&next.connector.connector_id),
            expected,
            json!(next),
            control,
        )
    }

    fn claim_source_notice(
        &self,
        id: &str,
        folder: &ConnectorSourceFolder,
        control: &InvocationControl,
    ) -> Result<bool, HostCapabilityError> {
        let key = format!(
            "connector-source-notice:{:x}",
            Sha256::digest(json!([id, folder]).to_string().as_bytes())
        );
        let previous = storage::read(self.relational.as_ref(), &key, control)?;
        let current = json!(self.incarnation);
        if previous.as_ref() == Some(&current) {
            return Ok(false);
        }
        // Re-prompt once after restart, while duplicate calls in one runtime share the popup.
        match storage::replace(
            self.relational.as_ref(),
            &key,
            previous,
            current.clone(),
            control,
        ) {
            Ok(()) => Ok(true),
            Err(error) => match storage::read(self.relational.as_ref(), &key, control)? {
                Some(observed) if observed == current => Ok(false),
                _ => Err(error),
            },
        }
    }

    fn enqueue_source_review(
        &self,
        id: &str,
        folder: &ConnectorSourceFolder,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if !self.claim_source_notice(id, folder, control)? {
            return Ok(());
        }
        if let Some(actions) = &self.connector_review_actions {
            actions
                .lock()
                .map_err(|_| source_denied(id, "available native review queue"))?
                .push(json!({"action":"connector.source_review","payload":{"connector_id":id}}));
        }
        Ok(())
    }
}

impl SourceFolderAccess<'_> {
    fn require(
        mut self,
        policy: &SourceImportAuthorization,
        request: bool,
    ) -> Result<PathBuf, HostCapabilityError> {
        if self.revoked() {
            return Err(source_error(
                "source_import_revoked",
                &self.folder.requested_path,
                "a new native Allow decision",
            ));
        }
        if self.allowed(policy)? {
            return Ok(PathBuf::from(&self.folder.canonical_path));
        }
        if request {
            self.request_review()?;
        }
        Err(source_error(
            "source_import_approval_required",
            &self.folder.requested_path,
            "native folder approval; retry after Allow",
        ))
    }

    fn revoked(&self) -> bool {
        self.access.grants.iter().any(|grant| {
            grant.state == ConnectorGrantState::Revoked
                && folder_contains(&grant.folder, &self.folder)
        })
    }

    fn allowed(&self, policy: &SourceImportAuthorization) -> Result<bool, HostCapabilityError> {
        let explicit = self
            .access
            .grants
            .iter()
            .filter(|grant| grant.state == ConnectorGrantState::Granted)
            .map(|grant| &grant.folder);
        let automatic = self
            .access
            .autoapprove_roots
            .iter()
            .filter(|_| self.access.mode == ConnectorSourceMode::Autoapprove);
        for root in explicit.chain(automatic) {
            if current_contains(root, &self.folder)? {
                return Ok(true);
            }
        }
        policy.bootstrap_allows(&self.folder)
    }

    fn request_review(&mut self) -> Result<(), HostCapabilityError> {
        if self
            .access
            .grants
            .iter()
            .any(|grant| grant.folder == self.folder)
        {
            return self.authority.enqueue_source_review(
                &self.access.connector.connector_id,
                &self.folder,
                self.control,
            );
        }
        let previous = self.access.clone();
        self.access.grants.push(ConnectorSourceFolderGrant {
            folder: self.folder.clone(),
            state: ConnectorGrantState::Pending,
        });
        self.authority
            .save_source_access(&previous, &mut self.access, self.control)?;
        self.authority.enqueue_source_review(
            &self.access.connector.connector_id,
            &self.folder,
            self.control,
        )
    }
}

fn folder_contains(root: &ConnectorSourceFolder, folder: &ConnectorSourceFolder) -> bool {
    Path::new(&folder.canonical_path).starts_with(&root.canonical_path)
        || Path::new(&folder.requested_path).starts_with(&root.requested_path)
}

fn current_contains(
    root: &ConnectorSourceFolder,
    folder: &ConnectorSourceFolder,
) -> Result<bool, HostCapabilityError> {
    if !Path::new(&folder.canonical_path).starts_with(&root.canonical_path) {
        return Ok(false);
    }
    let current = SourceImportAuthorization::inspect_folder(&root.requested_path)?;
    if current != *root {
        return Err(source_denied(
            &root.requested_path,
            "the originally approved canonical folder",
        ));
    }
    Ok(true)
}

fn source_key(id: &str) -> String {
    format!("connector-source:{id}")
}

fn checked_source_record(
    value: Value,
    connector: &ConnectorIdentity,
) -> Result<ConnectorSourceAccess, HostCapabilityError> {
    let access: ConnectorSourceAccess = serde_json::from_value(value).map_err(|error| {
        source_denied(
            &connector.connector_id,
            &format!("valid source access record: {error}"),
        )
    })?;
    if access.connector != *connector || access.revision == 0 {
        return Err(source_denied(
            &access.connector.connector_id,
            "the authenticated connector and a persisted positive revision",
        ));
    }
    Ok(access)
}

fn source_denied(value: &str, expected: &str) -> HostCapabilityError {
    source_error("source_import_root_denied", value, expected)
}

fn source_error(code: &str, value: &str, expected: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        "project.import",
        code,
        format!("source folder/connector {value:?}; expected {expected}"),
        false,
    )
}
