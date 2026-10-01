//! Runtime-owned correlation and cancellation for accepted invocations.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use crate::{
    PluginInvocationCancellation, PluginInvocationCancellationRequest, PluginInvocationContext,
    PluginRuntimeError,
};

pub(crate) struct ActivePluginInvocations {
    records: Mutex<HashMap<InvocationKey, ActiveInvocation>>,
}

pub(crate) struct ActiveInvocationGuard<'registry> {
    registry: &'registry ActivePluginInvocations,
    key: InvocationKey,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct InvocationKey {
    owner_id: String,
    request_id: String,
}

struct ActiveInvocation {
    plugin_id: String,
    session_id: Option<String>,
    scope_id: Option<String>,
    cancellation: PluginInvocationCancellation,
}

impl ActivePluginInvocations {
    pub(crate) fn new() -> Self {
        Self {
            records: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn register(
        &self,
        plugin_id: &str,
        context: &PluginInvocationContext,
    ) -> Result<ActiveInvocationGuard<'_>, PluginRuntimeError> {
        let key = InvocationKey::new(context.owner_id(), context.request_id());
        let mut records = self.lock_records()?;
        if records.contains_key(&key) {
            return Err(PluginRuntimeError::InvocationIdentityConflict {
                owner_id: key.owner_id,
                request_id: key.request_id,
            });
        }
        records.insert(key.clone(), ActiveInvocation::new(plugin_id, context));
        Ok(ActiveInvocationGuard {
            registry: self,
            key,
        })
    }

    pub(crate) fn cancel(
        &self,
        request: &PluginInvocationCancellationRequest,
    ) -> Result<bool, PluginRuntimeError> {
        let key = InvocationKey::new(&request.owner_id, &request.request_id);
        let records = self.lock_records()?;
        let Some(active) = records.get(&key) else {
            return Ok(false);
        };
        if !active.matches(request) {
            return Ok(false);
        }
        active.cancellation.cancel();
        Ok(true)
    }

    fn lock_records(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<InvocationKey, ActiveInvocation>>, PluginRuntimeError> {
        self.records
            .lock()
            .map_err(|_| PluginRuntimeError::InvocationAdmissionPoisoned)
    }
}

impl InvocationKey {
    fn new(owner_id: &str, request_id: &str) -> Self {
        Self {
            owner_id: owner_id.to_owned(),
            request_id: request_id.to_owned(),
        }
    }
}

impl ActiveInvocation {
    fn new(plugin_id: &str, context: &PluginInvocationContext) -> Self {
        Self {
            plugin_id: plugin_id.to_owned(),
            session_id: context.session_id().map(str::to_owned),
            scope_id: context.scope_id().map(str::to_owned),
            cancellation: context.cancellation(),
        }
    }

    fn matches(&self, request: &PluginInvocationCancellationRequest) -> bool {
        request
            .plugin_id
            .as_ref()
            .is_none_or(|plugin_id| self.plugin_id == *plugin_id)
            && self.session_id == request.session_id
            && self.scope_id == request.scope_id
    }
}

impl Drop for ActiveInvocationGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut records) = self.registry.records.lock() {
            records.remove(&self.key);
        }
    }
}
