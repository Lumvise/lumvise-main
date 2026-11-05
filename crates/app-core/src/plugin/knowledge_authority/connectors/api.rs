//! Native permission review. This API is not registered as an MCP capability.
use super::super::project_access::resolve_grant;
use super::*;
use crate::AppCore;

/// Generates a private installation proof. Example: the connector setup stores it in its protected file.
pub fn generate_connector_credential() -> Result<ConnectorCredential, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        format!("connector entropy source; expected 32 secure random bytes: {error}")
    })?;
    Ok(ConnectorCredential(hex::encode(bytes)))
}

/// Fresh native maintainer proof, independent of an editor's write membership.
/// Example: the desktop account adapter checks the website's current `owner` role.
pub use lumvise_contracts::ConnectorGrantMaintainer;

impl AppCore {
    /// Lists permissions for native review after fresh project and maintainer checks.
    /// Example: Settings calls this with its account-owned maintainer adapter.
    pub fn connector_knowledge_grants(
        &self,
        project_root: &str,
        maintainer: &dyn ConnectorGrantMaintainer,
    ) -> Result<Vec<ConnectorKnowledgeGrant>, String> {
        let authority = self.connector_authority();
        let control = InvocationControl::sixty_seconds();
        let project =
            self.connector_review_project(&authority, project_root, maintainer, &control)?;
        let rows = storage::list_prefix(self.relational.as_ref(), "connector-grant:", &control)
            .map_err(|error| error.to_string())?;
        read_project_grants(rows, &project)
    }

    /// Approves or revokes exactly the permission revision the maintainer reviewed.
    /// Example: decide a pending grant with its observed revision and state `Granted`.
    pub fn decide_connector_knowledge_grant(
        &self,
        project_root: &str,
        connector_id: &str,
        expected_revision: u64,
        state: ConnectorGrantState,
        maintainer: &dyn ConnectorGrantMaintainer,
    ) -> Result<ConnectorKnowledgeGrant, String> {
        let authority = self.connector_authority();
        let control = InvocationControl::sixty_seconds();
        let project =
            self.connector_review_project(&authority, project_root, maintainer, &control)?;
        authority
            .decide_connector_grant(connector_id, &project, expected_revision, state, &control)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn connector_authority(&self) -> KnowledgeAuthority {
        KnowledgeAuthority::new(self.relational.clone(), self.mcp_session_membership.clone())
            .with_connector_review_actions(Some(self.frontend_actions.clone()))
    }

    fn connector_review_project(
        &self,
        authority: &KnowledgeAuthority,
        root: &str,
        maintainer: &dyn ConnectorGrantMaintainer,
        control: &InvocationControl,
    ) -> Result<KnowledgeProjectIdentity, String> {
        let identity = authority
            .identity(control)
            .map_err(|error| error.to_string())?;
        let grant = resolve_grant(self.semantic.as_ref(), root, &identity, control)
            .map_err(|error| error.to_string())?;
        if !grant.writable {
            return Err(format!(
                "project {root:?}; expected writable maintainer membership"
            ));
        }
        maintainer.require_maintainer(&grant.project)?;
        Ok(grant.project)
    }
}

impl KnowledgeAuthority {
    fn decide_connector_grant(
        &self,
        id: &str,
        project: &KnowledgeProjectIdentity,
        revision: u64,
        state: ConnectorGrantState,
        control: &InvocationControl,
    ) -> Result<ConnectorKnowledgeGrant, HostCapabilityError> {
        if state == ConnectorGrantState::Pending {
            return Err(invalid(&json!(state), "Granted or Revoked decision"));
        }
        let key = grant_key(id, project);
        let mut grant = self.read_connector_grant(&key, control)?;
        if grant.revision != revision {
            return Err(invalid(
                &json!(revision),
                &format!("current grant revision {}", grant.revision),
            ));
        }
        let previous = json!(grant);
        grant.state = state;
        grant.revision = revision
            .checked_add(1)
            .ok_or_else(|| invalid(&json!(revision), "incrementable grant revision"))?;
        grant.operations = approved_operations(state);
        storage::replace(
            self.relational.as_ref(),
            &key,
            Some(previous),
            json!(grant),
            control,
        )?;
        Ok(grant)
    }
}

fn approved_operations(state: ConnectorGrantState) -> Vec<ConnectorKnowledgeOperation> {
    if state != ConnectorGrantState::Granted {
        return Vec::new();
    }
    vec![
        ConnectorKnowledgeOperation::Create,
        ConnectorKnowledgeOperation::Update,
    ]
}

fn read_project_grants(
    rows: Vec<Value>,
    project: &KnowledgeProjectIdentity,
) -> Result<Vec<ConnectorKnowledgeGrant>, String> {
    let mut grants = Vec::new();
    for row in rows {
        let mut grant: ConnectorKnowledgeGrant = serde_json::from_value(row)
            .map_err(|error| format!("stored connector permission; expected grant: {error}"))?;
        if grant.project.same_project(project) {
            grant.project = project.clone();
            grants.push(grant);
        }
    }
    grants.sort_by(|a, b| a.connector.connector_id.cmp(&b.connector.connector_id));
    Ok(grants)
}
