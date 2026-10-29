//! Owns atomic document locator/body publication through Semantic and the shared retry journal.
//! Callers prepare a frozen batch, then resolve its exact receipt; storage policy stays in core.
mod draft;
mod reference;
mod root;
mod space;

pub(super) use space::ensure as ensure_space;

pub(crate) use reference::{accepted_lifecycle_revision, accepted_reference};

use lumvise_contracts::{KnowledgeDocument, SemanticIndexObservation, SemanticRecordKind};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use super::{record_format as format, records};
use crate::publication_journal::FrozenKnowledgePublication;

pub(super) fn prepare(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    resources: Vec<Value>,
    expected_locator: Option<&str>,
    operation_id: &str,
) -> Result<FrozenKnowledgePublication, PluginError> {
    let mut document = document.clone();
    let evidence = super::citations::prepare(context, &mut document, operation_id)?;
    draft::DocumentPublicationDraft::capture(context, &document, resources, expected_locator)?
        .freeze(context, operation_id, evidence.dependencies)
        .map(|publication| publication.with_citations(evidence.claims))
}

pub(super) fn prepare_remove(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    operation_id: &str,
) -> Result<FrozenKnowledgePublication, PluginError> {
    let previous =
        records::selected_locator(context, &document.project_root, &document.document_id)?
            .ok_or_else(|| {
                super::invalid(
                    "document_revision_conflict",
                    &document.document_id,
                    "the original selected locator",
                )
            })?;
    validate_base(Some(&document.semantic_element_id), Some(&previous))?;
    let artifacts = format::body_id(&previous)
        .map(|id| json!({"artifact_id":id}))
        .into_iter()
        .collect::<Vec<_>>();
    let paths = vec![format::storage_path(&document.document_id)];
    let observation = observe(
        context,
        &document.project_root,
        &paths,
        &artifacts,
        Some(&previous),
    )?;
    let element = json!({"semantic_element_id":previous["semantic_element_id"], "semantic_source_id":previous["semantic_source_id"],
        "semantic_element_type":"virtual_document", "semantic_element_name":"Deleted document", "path":paths[0],
        "revision_dependencies":[], "metadata":{"knowledge_document_deleted":format::deletion(document, &previous)}});
    let removed = artifacts
        .iter()
        .map(|artifact| artifact["artifact_id"].clone())
        .collect::<Vec<_>>();
    Ok(batch(
        &document.project_root,
        &previous["semantic_source_id"],
        observation,
        operation_id,
        json!({
            "semantic_elements":[element], "semantic_relationships":[], "replace_paths":paths,
            "semantic_artifacts":[], "removed_artifact_ids":removed
        }),
    ))
}

fn validate_base(
    expected_locator: Option<&str>,
    previous: Option<&Value>,
) -> Result<(), PluginError> {
    let actual = previous.and_then(|row| row["semantic_element_id"].as_str());
    if actual == expected_locator {
        return Ok(());
    }
    Err(super::invalid(
        "document_revision_conflict",
        &json!(actual).to_string(),
        &format!("original observed document membership {expected_locator:?}"),
    ))
}

pub(super) fn observe(
    context: &mut PluginContext<'_>,
    root: &str,
    paths: &[String],
    artifacts: &[Value],
    previous: Option<&Value>,
) -> Result<SemanticIndexObservation, PluginError> {
    let artifact_ids = artifacts
        .iter()
        .filter_map(|artifact| artifact["artifact_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let coverage = json!({"kind":"partition", "partition":{"project_root":root,
        "replace_paths":paths, "provider_instance_id":format::PROVIDER}});
    let observation = crate::publication_observation::read(context, root, coverage, &artifact_ids)?;
    let elements = observation
        .expected
        .records
        .iter()
        .filter(|record| record.kind == SemanticRecordKind::Element)
        .map(|record| record.record_id.as_str())
        .collect::<Vec<_>>();
    let expected = previous
        .and_then(|row| row["semantic_element_id"].as_str())
        .into_iter()
        .collect::<Vec<_>>();
    if elements != expected {
        return Err(super::invalid(
            "document_revision_conflict",
            &json!(elements).to_string(),
            &format!("original observed locator membership {expected:?}"),
        ));
    }
    Ok(observation)
}

pub(super) fn batch(
    root: &str,
    source_id: &Value,
    observation: SemanticIndexObservation,
    operation_id: &str,
    mut batch: Value,
) -> FrozenKnowledgePublication {
    batch["provider_instance_id"] = json!(format::PROVIDER);
    batch["project_root"] = json!(root);
    batch["ingestion_job_id"] = json!(operation_id);
    batch["ingestion_page_index"] = json!(0);
    batch["ingestion_page_count"] = json!(1);
    batch["semantic_sources"] = json!([{"semantic_source_id":source_id,"kind":"virtual_documents",
        "name":"Knowledge documents", "root_path":null, "root_uri":format!("lumvise:documents/{}", observation.project_id)}]);
    batch["revision_observation"] = json!(observation);
    FrozenKnowledgePublication::DocumentBatch {
        batch,
        citations: vec![],
    }
}

pub(super) fn accepted(
    context: &mut PluginContext<'_>,
    root: &str,
    document_id: &str,
    receipt: Option<&Value>,
) -> Result<KnowledgeDocument, PluginError> {
    let receipt = receipt.ok_or_else(|| {
        super::invalid(
            "invalid_document_storage",
            document_id,
            "an accepted document receipt",
        )
    })?;
    for remap in receipt["remaps"].as_array().into_iter().flatten() {
        if remap["accepted"]["kind"] != "element" {
            continue;
        }
        let id = remap["accepted"]["record_id"].as_str().unwrap_or_default();
        let Some(element) = accepted_element(context, root, id)? else {
            continue;
        };
        if element["path"] == format::storage_path(document_id) {
            return records::exact_document(context, element);
        }
    }
    Err(super::invalid(
        "document_revision_unavailable",
        document_id,
        "the exact accepted document locator and payload, which may have been collected",
    ))
}

fn accepted_element(
    context: &mut PluginContext<'_>,
    root: &str,
    id: &str,
) -> Result<Option<Value>, PluginError> {
    let revision = crate::storage::read_revision(
        context,
        root,
        lumvise_contracts::SemanticRecordRef::element(id),
    )?;
    let Some(revision) = revision else {
        return Ok(None);
    };
    match revision.body {
        lumvise_semantic_revisions::SemanticRevisionBody::Element(element)
            if element.project_root == root && element.semantic_element_id == id =>
        {
            Ok(Some(json!(element)))
        }
        lumvise_semantic_revisions::SemanticRevisionBody::Pruned => Ok(None),
        other => Err(super::invalid(
            "invalid_document_storage",
            &format!("{other:?}"),
            &format!("exact accepted element {id} in project {root}"),
        )),
    }
}
