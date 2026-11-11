//! Native review API for the source-access owner; never registered as an MCP export.
use super::*;
use crate::AppCore;

impl AppCore {
    /// Lists registered connectors and their reviewed folder policy.
    /// Example: native Settings displays the default Restricted policy on first use.
    pub fn connector_source_access(&self) -> Result<Vec<ConnectorSourceAccess>, String> {
        let control = InvocationControl::sixty_seconds();
        let authority = self.connector_authority();
        storage::ensure(self.relational.as_ref(), &control).map_err(|error| error.to_string())?;
        let rows = storage::list_prefix(self.relational.as_ref(), "connector:", &control)
            .map_err(|error| error.to_string())?;
        rows.into_iter()
            .map(|row| {
                source_identity(row).and_then(|id| {
                    authority
                        .source_access(&id, &control)
                        .map_err(|error| error.to_string())
                })
            })
            .collect()
    }

    /// Applies one revision-fenced native decision without widening other permissions.
    /// Example: submit Folder after the user presses Allow in the Settings popup.
    pub fn change_connector_source_access(
        &self,
        change: ConnectorSourceAccessChange,
    ) -> Result<ConnectorSourceAccess, String> {
        self.connector_authority()
            .change_source_access(change, &InvocationControl::sixty_seconds())
            .map_err(|error| error.to_string())
    }
}

impl KnowledgeAuthority {
    fn change_source_access(
        &self,
        change: ConnectorSourceAccessChange,
        control: &InvocationControl,
    ) -> Result<ConnectorSourceAccess, HostCapabilityError> {
        let (id, revision) = change_identity(&change);
        let previous = self.source_access(id, control)?;
        if previous.revision != revision {
            return Err(source_denied(
                &revision.to_string(),
                &format!("current source access revision {}", previous.revision),
            ));
        }
        let mut next = previous.clone();
        apply_source_change(&mut next, change)?;
        self.save_source_access(&previous, &mut next, control)?;
        Ok(next)
    }
}

fn source_identity(value: Value) -> Result<String, String> {
    serde_json::from_value::<ConnectorIdentity>(value)
        .map(|connector| connector.connector_id)
        .map_err(|error| format!("connector record; expected registered identity: {error}"))
}

fn change_identity(change: &ConnectorSourceAccessChange) -> (&str, u64) {
    match change {
        ConnectorSourceAccessChange::Policy {
            connector_id,
            expected_revision,
            ..
        }
        | ConnectorSourceAccessChange::Folder {
            connector_id,
            expected_revision,
            ..
        } => (connector_id, *expected_revision),
    }
}

fn apply_source_change(
    access: &mut ConnectorSourceAccess,
    change: ConnectorSourceAccessChange,
) -> Result<(), HostCapabilityError> {
    match change {
        ConnectorSourceAccessChange::Policy { mode, roots, .. } => {
            set_source_policy(access, mode, roots)
        }
        ConnectorSourceAccessChange::Folder { folder, state, .. } => {
            decide_source_folder(access, folder, state)
        }
    }
}

fn set_source_policy(
    access: &mut ConnectorSourceAccess,
    mode: ConnectorSourceMode,
    roots: Vec<String>,
) -> Result<(), HostCapabilityError> {
    if (mode == ConnectorSourceMode::Autoapprove) == roots.is_empty() {
        return Err(source_denied(
            &format!("{mode:?} {roots:?}"),
            "Restricted with no roots or Autoapprove with selected folders",
        ));
    }
    let mut inspected = roots
        .iter()
        .map(|root| SourceImportAuthorization::inspect_folder(root))
        .collect::<Result<Vec<_>, _>>()?;
    inspected.sort_by(|a, b| a.canonical_path.cmp(&b.canonical_path));
    inspected.dedup_by(|a, b| a.canonical_path == b.canonical_path);
    access.mode = mode;
    access.autoapprove_roots = inspected;
    Ok(())
}

fn decide_source_folder(
    access: &mut ConnectorSourceAccess,
    folder: ConnectorSourceFolder,
    state: ConnectorGrantState,
) -> Result<(), HostCapabilityError> {
    if state == ConnectorGrantState::Pending {
        return Err(source_denied(
            "Pending",
            "Granted or Revoked native decision",
        ));
    }
    if state == ConnectorGrantState::Granted
        && SourceImportAuthorization::inspect_folder(&folder.requested_path)? != folder
    {
        return Err(source_denied(
            &folder.requested_path,
            "the canonical folder shown during review",
        ));
    }
    let grant = access
        .grants
        .iter_mut()
        .find(|grant| grant.folder == folder)
        .ok_or_else(|| source_denied(&folder.requested_path, "an existing reviewed request"))?;
    grant.state = state;
    Ok(())
}
