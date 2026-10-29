//! Owns durable checkout bases and frozen source publications through portable persistence.
//! App imports and the Semantic host call this entrypoint. Row layout and acknowledgement
//! internals stay private; graph revision, conflict and collection policy stays in DB Core.
mod acknowledgement;
mod adoption;
mod host;
mod pending;
mod records;
mod source;
mod source_binding;
mod source_projection;
mod storage;
#[cfg(test)]
mod tests;
mod validation;

pub(crate) use host::invoke_index_publication;
use records::*;
pub(crate) use records::{FrozenIndexPublication, IndexCheckoutBinding, IndexPathDigest};
pub(crate) use source::IndexSourceCheckout;
use storage::CheckpointRows;

use crate::{AppCoreError, Result};
use lumvise_db_core::{SemanticOperation, SemanticPersistence, SemanticResult};
use lumvise_resource_routing::InvocationControl;
use std::sync::Arc;

impl crate::AppCore {
    pub(crate) fn install_index_publication_client(&self, client: String) -> Result<()> {
        install_index_client(&self.index_publication_client, client)
    }
}

pub(crate) fn install_index_client(
    identity: &std::sync::OnceLock<String>,
    client: String,
) -> Result<()> {
    if client.trim().is_empty() {
        return Err(invalid_checkpoint(
            client,
            "the persisted client identity from startup",
        ));
    }
    if let Err(client) = identity.set(client) {
        if identity.get() != Some(&client) {
            return Err(invalid_checkpoint(
                client,
                "the unchanged startup client identity",
            ));
        }
    }
    Ok(())
}

pub(crate) struct IndexPublicationJournal {
    binding: IndexCheckoutBinding,
    project_root: String,
    semantic: Arc<dyn SemanticPersistence>,
    rows: CheckpointRows,
}

impl IndexPublicationJournal {
    /// Binds one persistent client/checkout identity; roots are replaceable routes.
    /// Example: recreate this journal with the same binding after an app restart.
    pub(crate) fn new(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn lumvise_db_core::RelationalPersistence>,
        binding: IndexCheckoutBinding,
        project_root: String,
    ) -> Result<Self> {
        binding.validate()?;
        if project_root.trim().is_empty() {
            return Err(invalid_checkpoint(project_root, "a project route"));
        }
        let rows = CheckpointRows::new(relational, &binding)?;
        Ok(Self {
            binding,
            project_root,
            semantic,
            rows,
        })
    }

    fn semantic(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> Result<SemanticResult> {
        Ok(self.semantic.execute(operation, control)?)
    }

    fn header(&self, control: &InvocationControl) -> Result<CheckpointHeader> {
        let header: CheckpointHeader = self.rows.get("header", control)?.ok_or_else(|| {
            invalid_checkpoint(&self.binding.checkout_id, "an established checkout base")
        })?;
        if header.binding != self.binding {
            return Err(invalid_checkpoint(
                &self.binding.checkout_id,
                "the original checkpoint identity and source binding",
            ));
        }
        Ok(header)
    }
}

fn invalid_checkpoint(value: impl Into<String>, expected: impl Into<String>) -> AppCoreError {
    AppCoreError::invalid_value(value, expected)
}
