//! Restart comparison uses producer output before accepted-ID reference rewriting.
use super::*;
use lumvise_contracts::{IndexBatchRequest, SemanticElementUpsert, SemanticRelationshipUpsert};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Default, serde::Serialize)]
struct ProjectedPath<'a> {
    elements: Vec<&'a SemanticElementUpsert>,
    relationships: Vec<&'a SemanticRelationshipUpsert>,
}

pub(super) fn projected_inventory(batch: &IndexBatchRequest) -> Result<Vec<IndexPathDigest>> {
    let paths = projected_paths(batch)?;
    paths
        .into_iter()
        .map(|(path, records)| {
            let mut digest = file_digest(path, &records.elements)?;
            let bytes = serde_json::to_vec(&records).map_err(lumvise_db_core::DbError::from)?;
            digest.projection_sha256 = Some(format!("{:x}", Sha256::digest(bytes)));
            Ok(digest)
        })
        .collect()
}

pub(super) fn affected_callers(
    batch: &IndexBatchRequest,
    changed: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    let paths = batch
        .semantic_elements
        .iter()
        .map(|element| (element.semantic_element_id.as_str(), element.path.as_str()))
        .collect::<BTreeMap<_, _>>();
    batch
        .semantic_relationships
        .iter()
        .filter_map(|edge| {
            let target = paths.get(edge.target_element_id.as_deref()?)?;
            changed
                .contains(*target)
                .then(|| {
                    paths
                        .get(edge.source_element_id.as_str())
                        .map(|path| (*path).to_owned())
                })
                .flatten()
        })
        .collect()
}

fn projected_paths(batch: &IndexBatchRequest) -> Result<BTreeMap<&str, ProjectedPath<'_>>> {
    let mut paths = BTreeMap::<&str, ProjectedPath<'_>>::new();
    let mut owners = BTreeMap::new();
    for element in &batch.semantic_elements {
        paths
            .entry(&element.path)
            .or_default()
            .elements
            .push(element);
        owners.insert(element.semantic_element_id.as_str(), element.path.as_str());
    }
    for edge in &batch.semantic_relationships {
        let path = owners.get(edge.source_element_id.as_str()).ok_or_else(|| {
            invalid_checkpoint(
                &edge.source_element_id,
                "a projected source for each index relationship",
            )
        })?;
        paths
            .get_mut(path)
            .ok_or_else(|| invalid_checkpoint(*path, "a projected source path"))?
            .relationships
            .push(edge);
    }
    Ok(paths)
}

fn file_digest(path: &str, elements: &[&SemanticElementUpsert]) -> Result<IndexPathDigest> {
    let roots = elements
        .iter()
        .filter(|element| {
            element.metadata.as_ref().is_some_and(|metadata| {
                metadata["kind"] == "directory" || metadata["source_digest_sha256"].is_string()
            })
        })
        .collect::<Vec<_>>();
    let [root] = roots.as_slice() else {
        return Err(invalid_checkpoint(
            path,
            "one projected file/directory root with a scanner digest",
        ));
    };
    let metadata = root
        .metadata
        .as_ref()
        .ok_or_else(|| invalid_checkpoint(path, "source metadata"))?;
    let digest = IndexPathDigest {
        path: path.into(),
        directory: metadata["kind"] == "directory",
        source_sha256: metadata["source_digest_sha256"].as_str().map(str::to_owned),
        projection_sha256: None,
    };
    digest.validate()?;
    Ok(digest)
}
