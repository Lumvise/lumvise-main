//! Producer ownership rules shared by publication adapters; no backend or staging state.

use crate::{SemanticElement, SemanticPartition, SemanticRevisionError, SemanticRevisionResult};
use std::collections::HashSet;

impl SemanticPartition {
    /// Select a record by project, producer and optional replacement paths.
    /// Example: `partition.contains_element(&element)` selects one publication owner.
    pub fn contains_element(&self, element: &SemanticElement) -> bool {
        element.project_root == self.project_root
            && self.contains_path(&element.path)
            && self.provider_instance_id.as_ref().is_none_or(|provider| {
                element.metadata["provider_instance_id"].as_str() == Some(provider)
            })
    }

    /// Reject another producer's input or identity before changing the current graph.
    /// Example: `partition.validate_ownership(&current_identities, &incoming)?`.
    pub fn validate_ownership(
        &self,
        existing: &[SemanticElement],
        incoming: &[SemanticElement],
    ) -> SemanticRevisionResult<()> {
        let Some(provider) = &self.provider_instance_id else {
            return Ok(());
        };
        let foreign = foreign_identities(existing, &self.project_root, provider);
        for element in incoming {
            validate_element_owner(element, &self.project_root, provider)?;
            validate_provider_identity(element, &foreign, &self.project_root, provider)?;
        }
        Ok(())
    }
}

fn validate_provider_identity(
    element: &SemanticElement,
    foreign: &HashSet<&str>,
    root: &str,
    provider: &str,
) -> SemanticRevisionResult<()> {
    if !foreign.contains(element.semantic_element_id.as_str()) {
        return Ok(());
    }
    Err(SemanticRevisionError::invalid_value(
        &element.semantic_element_id,
        format!("unused identity or one owned by provider `{provider}` in `{root}`"),
    ))
}

fn foreign_identities<'a>(
    existing: &'a [SemanticElement],
    root: &str,
    provider: &str,
) -> HashSet<&'a str> {
    existing
        .iter()
        .filter(|element| {
            element.project_root != root
                || element.metadata["provider_instance_id"].as_str() != Some(provider)
        })
        .map(|element| element.semantic_element_id.as_str())
        .collect()
}

fn validate_element_owner(
    element: &SemanticElement,
    root: &str,
    provider: &str,
) -> SemanticRevisionResult<()> {
    if element.project_root == root
        && element.metadata["provider_instance_id"].as_str() == Some(provider)
    {
        return Ok(());
    }
    Err(SemanticRevisionError::invalid_value(
        format!(
            "{} in {} owned by {}",
            element.semantic_element_id,
            element.project_root,
            element.metadata["provider_instance_id"]
        ),
        format!("element in `{root}` owned by provider `{provider}`"),
    ))
}
