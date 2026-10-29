//! Resolves one checkout's immutable logical source through its existing relational journal.
use super::*;
use lumvise_db_core::RelationalPersistence;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

/// Resolves identity before projection without treating current membership as an editing base.
/// Example: reopen a persisted checkout route without loading its selected record bodies.
pub(super) fn resolve(
    semantic: &dyn SemanticPersistence,
    relational: Arc<dyn RelationalPersistence>,
    client: &str,
    root: &Path,
    project_root: &str,
    provider: &str,
    control: &InvocationControl,
) -> Result<IndexCheckoutBinding> {
    let mut binding = checkout_route(semantic, client, root, project_root, provider, control)?;
    let rows = CheckpointRows::source_route(relational, &binding)?;
    rows.ensure(control)?;
    if let Some(saved) = rows.get::<IndexCheckoutBinding>("binding", control)? {
        return validated_binding(saved, &binding);
    }
    // Resolve membership only on first attachment; later opens must keep the same source identity.
    binding.semantic_source_id = established_source(semantic, project_root, &binding, control)?;
    store_binding(&rows, &binding, control)?;
    Ok(binding)
}

fn checkout_route(
    semantic: &dyn SemanticPersistence,
    client: &str,
    root: &Path,
    project_root: &str,
    provider: &str,
    control: &InvocationControl,
) -> Result<IndexCheckoutBinding> {
    let root = checkout_directory(root)?;
    let project_id = lineage(semantic, project_root, control)?;
    let binding = IndexCheckoutBinding {
        client_instance_id: client.into(),
        checkout_id: format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes())),
        semantic_source_id: format!("filesystem:{project_id}:main"),
        project_id,
        provider_instance_id: provider.into(),
        selection_id: "working".into(),
        source_context: "local".into(),
    };
    binding.validate()?;
    Ok(binding)
}

fn checkout_directory(root: &Path) -> Result<std::path::PathBuf> {
    let canonical = root.canonicalize().map_err(|error| {
        invalid_checkpoint(
            root.display().to_string(),
            format!("an existing checkout directory ({error})"),
        )
    })?;
    if !canonical.is_dir() {
        return Err(invalid_checkpoint(
            root.display().to_string(),
            "a checkout directory",
        ));
    }
    Ok(canonical)
}

fn lineage(
    semantic: &dyn SemanticPersistence,
    project_root: &str,
    control: &InvocationControl,
) -> Result<String> {
    let result = semantic.execute(
        SemanticOperation::ProjectIdentity {
            project_root: project_root.into(),
        },
        control,
    )?;
    match result {
        SemanticResult::ProjectIdentity(identity) => Ok(identity.project_id),
        _ => Err(invalid_checkpoint(project_root, "a project lineage")),
    }
}

fn established_source(
    semantic: &dyn SemanticPersistence,
    project_root: &str,
    binding: &IndexCheckoutBinding,
    control: &InvocationControl,
) -> Result<String> {
    let sources = selection_sources(semantic, project_root, binding, control)?;
    if sources.len() > 1 {
        return Err(invalid_checkpoint(
            &binding.provider_instance_id,
            "one established logical filesystem source in the provider",
        ));
    }
    Ok(sources
        .into_iter()
        .next()
        .unwrap_or_else(|| binding.semantic_source_id.clone()))
}

fn selection_sources(
    semantic: &dyn SemanticPersistence,
    project_root: &str,
    binding: &IndexCheckoutBinding,
    control: &InvocationControl,
) -> Result<BTreeSet<String>> {
    let result = semantic.execute(
        SemanticOperation::RevisionSelection {
            project_root: project_root.into(),
            selection_id: binding.selection_id.clone(),
        },
        control,
    )?;
    let SemanticResult::RevisionSelection(snapshot) = result else {
        return Err(invalid_checkpoint(
            project_root,
            "the current working selection",
        ));
    };
    Ok(snapshot
        .iter()
        .flat_map(|snapshot| &snapshot.elements)
        .filter(|element| {
            element.metadata["provider_instance_id"].as_str()
                == Some(binding.provider_instance_id.as_str())
        })
        .map(|element| element.semantic_source_id.clone())
        .collect())
}

fn validated_binding(
    saved: IndexCheckoutBinding,
    route: &IndexCheckoutBinding,
) -> Result<IndexCheckoutBinding> {
    saved.validate()?;
    let mut expected = route.clone();
    expected
        .semantic_source_id
        .clone_from(&saved.semantic_source_id);
    if saved != expected {
        return Err(invalid_checkpoint(
            format!("{saved:?}"),
            "the original checkout/lineage source route",
        ));
    }
    Ok(saved)
}

fn store_binding(
    rows: &CheckpointRows,
    binding: &IndexCheckoutBinding,
    control: &InvocationControl,
) -> Result<()> {
    let mutations = vec![
        rows.require_value::<IndexCheckoutBinding>("binding", None)?,
        rows.put("binding", binding)?,
    ];
    if let Err(error) = rows.apply(mutations, control) {
        // Another opener or a lost acknowledgement is safe only for the identical binding.
        if rows
            .get::<IndexCheckoutBinding>("binding", control)?
            .as_ref()
            != Some(binding)
        {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
