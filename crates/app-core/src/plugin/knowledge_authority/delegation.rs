//! Durable permission for one memory destination and named inputs. Agents owns scheduling;
//! only Knowledge may bind a bounded operation inside its signed execution frame.
mod activation;
mod binding;
mod executor;
mod preparation;
pub(super) use preparation::ExecutorPreparationRequest;
mod persistence;
pub(super) use executor::{MemoryExecutorSetupBinding, require_autonomous_executor_origin};

use super::*;
use lumvise_contracts::KnowledgeDocumentReference;
use sha2::{Digest, Sha256};
use std::time::Instant;

const KNOWLEDGE: &str = "builtin.knowledge";
const EXECUTE: &str = "execute_document_delegation";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryDelegationSave {
    operation_id: String,
    document_id: String,
    expected_revision: Option<u64>,
    enabled: bool,
    inputs: Vec<KnowledgeDocumentReference>,
    required_projects: Vec<KnowledgeProjectIdentity>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DelegatedMemoryAction {
    ReadDestination {},
    ReadInput {
        reference: KnowledgeDocumentReference,
    },
    UpdateDestination {
        operation_id: String,
        expected_semantic_element_id: String,
        expected_revision: u64,
        content: String,
        #[serde(default)]
        causal_paths: Vec<Vec<String>>,
    },
    ReadReceipt {
        operation_id: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum MemoryDelegationRequest {
    Save {
        intent: MemoryDelegationSave,
    },
    Get {
        document_id: String,
    },
    PrepareExecutor {
        delegation_id: String,
        revision: u64,
    },
    Activate {
        delegation_id: String,
        revision: u64,
        observations: Vec<agent::AgentScopeObservation>,
    },
    Bind {
        delegation_id: String,
        revision: u64,
        action: DelegatedMemoryAction,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredMemoryDelegation {
    delegation_id: String,
    revision: u64,
    executor_agent_id: String,
    document_id: String,
    enabled: bool,
    inputs: Vec<KnowledgeDocumentReference>,
    owner: KnowledgeAccessContext,
    executor_observations: Vec<agent::AgentScopeObservation>,
    executor_preparation: Option<preparation::MemoryExecutorPreparation>,
}

#[derive(Clone)]
pub(super) struct MemoryDelegationBinding {
    observed: StoredMemoryDelegation,
    setup: bool,
    deadline: Instant,
}

pub(super) fn delegation_frame(context: &PluginInvocationContext) -> Option<u64> {
    context
        .plugin_chain()
        .iter()
        .rev()
        .find(|frame| frame.plugin_id == KNOWLEDGE && frame.export_id == EXECUTE)
        .map(|frame| frame.binding_id)
}

pub(super) struct MemoryDelegationAuthority<'a> {
    authority: &'a KnowledgeAuthority,
    semantic: &'a dyn SemanticPersistence,
    context: &'a PluginInvocationContext,
    control: &'a InvocationControl,
}

impl<'a> MemoryDelegationAuthority<'a> {
    pub(super) fn new(
        authority: &'a KnowledgeAuthority,
        plugin: &str,
        semantic: &'a dyn SemanticPersistence,
        context: &'a PluginInvocationContext,
        control: &'a InvocationControl,
    ) -> Result<Self, HostCapabilityError> {
        if plugin != KNOWLEDGE
            || context
                .plugin_chain()
                .last()
                .is_none_or(|frame| frame.plugin_id != KNOWLEDGE)
        {
            return Err(unbound(context));
        }
        Ok(Self {
            authority,
            semantic,
            context,
            control,
        })
    }

    pub(super) fn invoke(
        &self,
        request: MemoryDelegationRequest,
    ) -> Result<Value, HostCapabilityError> {
        match request {
            MemoryDelegationRequest::PrepareExecutor {
                delegation_id,
                revision,
            } => self.prepare_executor(&delegation_id, revision),
            MemoryDelegationRequest::Get { document_id } => self.get(&document_id),
            MemoryDelegationRequest::Save { intent } => self.save(intent),
            MemoryDelegationRequest::Activate {
                delegation_id,
                revision,
                observations,
            } => self.activate(&delegation_id, revision, observations),
            MemoryDelegationRequest::Bind {
                delegation_id,
                revision,
                action,
            } => self.bind(&delegation_id, revision, action),
        }
    }

    fn owner(&self) -> Result<KnowledgeAccessContext, HostCapabilityError> {
        if delegation_frame(self.context).is_some() {
            return Err(unbound(self.context));
        }
        let scope = self
            .authority
            .current(self.semantic, self.context, self.control)?;
        if scope.space.is_none() || !project_access::memory_writable(&scope) {
            return Err(invalid(
                &json!(scope.space),
                "an owned writable memory space",
            ));
        }
        Ok(scope)
    }

    fn get(&self, document: &str) -> Result<Value, HostCapabilityError> {
        let scope = self
            .authority
            .current(self.semantic, self.context, self.control)?;
        let writable = project_access::memory_writable(&scope);
        let id = delegation_id(&scope, document)?;
        Ok(
            json!({"delegation_id":id,"project_root":scope.space.expect("owned space").project_root,
                "writable":writable,
                "delegation":read_delegation(self.authority, &id, self.control)?.map(delegation_view)}),
        )
    }
}

fn delegation_view(saved: StoredMemoryDelegation) -> Value {
    let mut view = json!(saved);
    view.as_object_mut()
        .expect("delegation object")
        .remove("executor_preparation");
    view
}

fn delegation_id(
    scope: &KnowledgeAccessContext,
    document: &str,
) -> Result<String, HostCapabilityError> {
    if document.trim().is_empty() {
        return Err(invalid(
            &json!(document),
            "a nonempty destination document identity",
        ));
    }
    let space = scope
        .space
        .as_ref()
        .ok_or_else(|| invalid(&json!(scope), "an owned memory space"))?;
    let digest = Sha256::digest(
        serde_json::to_vec(&json!([space.space_id, document])).expect("serializable identities"),
    );
    Ok(format!("{digest:x}"))
}

fn read_delegation(
    authority: &KnowledgeAuthority,
    id: &str,
    control: &InvocationControl,
) -> Result<Option<StoredMemoryDelegation>, HostCapabilityError> {
    storage::read(
        authority.relational.as_ref(),
        &format!("delegation:{id}"),
        control,
    )?
    .map(|value| {
        serde_json::from_value(value.clone())
            .map_err(|_| invalid(&value, "a stored memory delegation"))
    })
    .transpose()
}
