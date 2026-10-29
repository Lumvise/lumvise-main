//! Host-owned memory authority. Trusted session owners bind lifecycle facts;
//! Knowledge may read current authority, never model-supplied owner or grant fields.
//! Persistence and session row formats remain private to this module.
mod agent;
mod citation_retention;
mod delegation;
mod inspection;
mod mcp;
mod project_access;
mod provenance;
pub(super) mod semantic_access;
mod storage;
#[cfg(test)]
mod tests;

use lumvise_contracts::{
    KnowledgeAccessContext, KnowledgePrincipalKind, KnowledgeProjectGrant,
    KnowledgeProjectIdentity, KnowledgeSpaceDescriptor, KnowledgeSpaceKind,
};
use lumvise_db_core::{
    RelationalPersistence, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_plugin_runtime::{HostCapabilityError, PluginInvocationContext};
use lumvise_resource_routing::InvocationControl;
use project_access::{project_grants, resolve_grant, resolve_project};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub(super) const CAPABILITY: &str = "runtime.knowledge_scope";
pub(super) use lumvise_contracts::is_knowledge_space_root as is_memory_root;
const ASSISTANT: &str = "builtin.assistant";

pub(super) struct KnowledgeAuthority {
    relational: Arc<dyn RelationalPersistence>,
    mcp_membership: Arc<crate::app::mcp_session_registry::McpSessionMembership>,
    incarnation: String,
    binding_write: Mutex<()>,
    agent_bindings: Mutex<std::collections::HashMap<u64, agent::AgentScopeBinding>>,
    inspection_bindings: Mutex<std::collections::HashMap<u64, inspection::HumanInspectionBinding>>,
    delegation_bindings: Mutex<std::collections::HashMap<u64, delegation::MemoryDelegationBinding>>,
    executor_setups: Mutex<std::collections::HashMap<u64, delegation::MemoryExecutorSetupBinding>>,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ScopeRequest {
    Current {},
    ProjectAccess {
        project_root: String,
    },
    ExecutorPreparation {
        request: delegation::ExecutorPreparationRequest,
    },
    Delegation {
        request: delegation::MemoryDelegationRequest,
    },
    InspectSpace {
        selection: lumvise_contracts::KnowledgeSpaceInspection,
    },
    Citation {
        request: citation_retention::CitationAuthorityRequest,
    },
    BindAgent {
        binding: agent::AgentScopeEvidence,
    },
    AuthorizeAgentGrants {
        grants: Vec<agent::AgentScopeGrant>,
    },
    BindAssistant {
        session_id: String,
        session_epoch: u64,
        active: bool,
        #[serde(default)]
        project_root: Option<String>,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AssistantScopeBinding {
    incarnation: String,
    session_epoch: u64,
    active: bool,
    active_project: Option<KnowledgeProjectIdentity>,
    source_projects: Vec<KnowledgeProjectIdentity>,
}

impl KnowledgeAuthority {
    pub(super) fn new(
        relational: Arc<dyn RelationalPersistence>,
        mcp_membership: Arc<crate::app::mcp_session_registry::McpSessionMembership>,
    ) -> Self {
        Self {
            relational,
            mcp_membership,
            incarnation: uuid::Uuid::new_v4().to_string(),
            binding_write: Mutex::new(()),
            agent_bindings: Mutex::default(),
            inspection_bindings: Mutex::default(),
            delegation_bindings: Mutex::default(),
            executor_setups: Mutex::default(),
        }
    }

    pub(super) fn invoke(
        &self,
        plugin: &str,
        input: Value,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        let request = serde_json::from_value(input.clone())
            .map_err(|_| invalid(&input, "current or trusted memory-owner binding request"))?;
        let bridge = super::plugin_host_capabilities::invocation_control(context);
        let control = bridge.control();
        match request {
            ScopeRequest::ProjectAccess { project_root } => self
                .project_access(plugin, &project_root, semantic, context, &control)
                .map(|grant| json!(grant)),
            ScopeRequest::ExecutorPreparation { request } => {
                self.executor_preparation(plugin, request, semantic, context, &control)
            }
            ScopeRequest::Delegation { request } => delegation::MemoryDelegationAuthority::new(
                self, plugin, semantic, context, &control,
            )?
            .invoke(request),
            ScopeRequest::InspectSpace { selection } => {
                self.bind_inspection(plugin, selection, semantic, context, &control)
            }
            ScopeRequest::Citation { request } => citation_retention::CitationAuthority::new(
                self, plugin, semantic, context, &control,
            )?
            .invoke(request),
            ScopeRequest::AuthorizeAgentGrants { grants } => {
                self.authorize_agent_grants(plugin, &grants, semantic, context, &control)?;
                Ok(json!({"authorized":true}))
            }
            ScopeRequest::BindAgent { binding } => {
                self.bind_agent(plugin, binding, semantic, context, &control)?;
                Ok(json!({"bound":true}))
            }
            ScopeRequest::Current {} => self
                .current(semantic, context, &control)
                .map(|scope| json!(scope)),
            ScopeRequest::BindAssistant {
                session_id,
                session_epoch,
                active,
                project_root,
            } => {
                self.bind_assistant(
                    plugin,
                    &session_id,
                    session_epoch,
                    active,
                    project_root.as_deref(),
                    semantic,
                    &control,
                )?;
                Ok(json!({"bound":active}))
            }
        }
    }

    fn identity(&self, control: &InvocationControl) -> Result<String, HostCapabilityError> {
        storage::ensure(self.relational.as_ref(), control)?;
        if let Some(value) = storage::read(self.relational.as_ref(), "local-profile", control)? {
            return identity_value(value);
        }
        let value = json!({"authority":uuid::Uuid::new_v4().to_string()});
        let result = storage::replace(
            self.relational.as_ref(),
            "local-profile",
            None,
            value,
            control,
        );
        // A competing host may have installed the one durable profile first.
        if let Some(value) = storage::read(self.relational.as_ref(), "local-profile", control)? {
            return identity_value(value);
        }
        result?;
        Err(invalid(&Value::Null, "persisted local memory authority"))
    }

    fn bind_assistant(
        &self,
        plugin: &str,
        session: &str,
        epoch: u64,
        active: bool,
        root: Option<&str>,
        semantic: &dyn SemanticPersistence,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if plugin != ASSISTANT || session.trim().is_empty() || epoch == 0 {
            return Err(invalid(
                &json!([plugin, session, epoch]),
                "builtin.assistant with nonempty session and positive epoch",
            ));
        }
        let authority = self.identity(control)?;
        let active_project = root
            .map(|root| resolve_project(semantic, root, &authority, control))
            .transpose()?;
        let key = binding_key(session);
        let _guard = self
            .binding_write
            .lock()
            .map_err(|_| invalid(&json!(session), "available session binding lock"))?;
        let previous = storage::read(self.relational.as_ref(), &key, control)?;
        let binding = self.next_binding(previous.as_ref(), epoch, active, active_project)?;
        storage::replace(
            self.relational.as_ref(),
            &key,
            previous,
            json!(binding),
            control,
        )
    }

    fn next_binding(
        &self,
        previous: Option<&Value>,
        epoch: u64,
        active: bool,
        project: Option<KnowledgeProjectIdentity>,
    ) -> Result<AssistantScopeBinding, HostCapabilityError> {
        let previous = previous.map(decode_binding).transpose()?;
        if previous.as_ref().is_some_and(|old| {
            old.session_epoch > epoch || (old.session_epoch == epoch && !old.active && active)
        }) {
            return Err(invalid(
                &json!(epoch),
                "newer session epoch after replacement or termination",
            ));
        }
        let mut sources = previous
            .filter(|old| old.session_epoch == epoch)
            .map(|old| old.source_projects)
            .unwrap_or_default();
        if let Some(project) = &project {
            if !sources.contains(project) {
                sources.push(project.clone());
            }
        }
        Ok(AssistantScopeBinding {
            incarnation: self.incarnation.clone(),
            session_epoch: epoch,
            active,
            active_project: project,
            source_projects: sources,
        })
    }

    pub(super) fn current(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<KnowledgeAccessContext, HostCapabilityError> {
        if delegation::delegation_frame(context).is_some() {
            return self.current_delegation(semantic, context, control);
        }
        if let Some(frame) = agent::agent_frame(context) {
            return self.current_agent(frame, semantic, context, control);
        }
        if inspection::inspection_frame(context).is_some() {
            return self.current_inspection(semantic, context, control);
        }
        self.current_origin(semantic, context, control)
    }

    fn current_origin(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<KnowledgeAccessContext, HostCapabilityError> {
        if context.scope_id() == Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE) {
            return self.current_mcp(semantic, context, control);
        }
        if context.scope_id() != Some("assistant_session") || context.owner_id() != ASSISTANT {
            return Err(unbound(context));
        }
        let key = binding_key(context.session_id().ok_or_else(|| unbound(context))?);
        let binding = storage::read(self.relational.as_ref(), &key, control)?
            .as_ref()
            .map(decode_binding)
            .transpose()?
            .ok_or_else(|| unbound(context))?;
        if !binding.active
            || binding.incarnation != self.incarnation
            || context.scope_epoch() != Some(binding.session_epoch)
        {
            return Err(unbound(context));
        }
        let authority = self.identity(control)?;
        let grants = project_grants(semantic, &authority, control)?;
        if binding.source_projects.iter().any(|project| {
            !grants
                .iter()
                .any(|grant| grant.project.same_project(project))
        }) {
            return Err(invalid(
                &json!(binding.source_projects),
                "current grants for supplied context; rebuild the conversation after revocation",
            ));
        }
        Ok(KnowledgeAccessContext {
            principal_kind: KnowledgePrincipalKind::Assistant,
            space: Some(assistant_space(&authority)),
            principal_id: "default".into(),
            security_domain: authority.clone(),
            storage_authority: authority,
            project_grants: grants,
            active_project: binding.active_project,
            source_projects: binding.source_projects,
        })
    }
}

fn assistant_space(authority: &str) -> KnowledgeSpaceDescriptor {
    KnowledgeSpaceDescriptor {
        space_id: format!("assistant:{authority}:default"),
        space_kind: KnowledgeSpaceKind::Assistant,
        project_root: format!("assistant-space://{authority}/default"),
        principal_id: "default".into(),
        security_domain: authority.into(),
        storage_authority: authority.into(),
        shared_project: None,
        revision: 1,
    }
}

fn identity_value(value: Value) -> Result<String, HostCapabilityError> {
    value["authority"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(&value, "nonempty durable authority"))
}

fn binding_key(session: &str) -> String {
    format!("assistant-session:{session}")
}

fn decode_binding(value: &Value) -> Result<AssistantScopeBinding, HostCapabilityError> {
    serde_json::from_value(value.clone()).map_err(|_| invalid(value, "registered Assistant scope"))
}

fn scope_failure(error: lumvise_db_core::DbError) -> HostCapabilityError {
    HostCapabilityError::new(
        CAPABILITY,
        "knowledge_authority_unavailable",
        error.to_string(),
        false,
    )
}

fn unbound(context: &PluginInvocationContext) -> HostCapabilityError {
    invalid(
        &json!([
            context.scope_id(),
            context.owner_id(),
            context.session_id(),
            context.scope_epoch()
        ]),
        "active host-registered memory scope with matching session epoch",
    )
}

fn invalid(value: &Value, expected: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        CAPABILITY,
        "knowledge_scope_denied",
        format!("invalid memory authority `{value}`; expected {expected}"),
        false,
    )
}
