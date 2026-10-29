//! Read-only source-family choices for Knowledge workflows; write bases remain untouched.
use lumvise_contracts::{
    KnowledgeDocument, KnowledgeDocumentReference, SemanticRecordKind, SemanticRecordRef,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use lumvise_semantic_revisions::SemanticRevisionRemap;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(crate) struct SelectedSources {
    requested: BTreeSet<String>,
    remaps: BTreeMap<String, String>,
}

impl SelectedSources {
    pub(crate) fn load(
        context: &mut PluginContext<'_>,
        root: &str,
        requested: BTreeSet<String>,
        commit: Option<i64>,
    ) -> Result<Self, PluginError> {
        if requested.is_empty() {
            return Ok(Self::default());
        }
        let records = requested
            .iter()
            .map(SemanticRecordRef::element)
            .collect::<Vec<_>>();
        let output = context.host_call("storage.semantic", json!({"operation":"revision_families", "query":{
            "project_root":root, "selection_id":"working", "expected_commit_version":commit, "records":records}}))?;
        let remaps: Vec<SemanticRevisionRemap> =
            crate::parse(output["remaps"].clone(), "explicit source-family remaps")?;
        let mut selected = Self {
            requested,
            remaps: BTreeMap::new(),
        };
        for remap in remaps {
            selected.include(remap)?;
        }
        Ok(selected)
    }

    pub(crate) fn for_documents(
        context: &mut PluginContext<'_>,
        root: &str,
        documents: &[KnowledgeDocument],
    ) -> Result<Self, PluginError> {
        Self::load(context, root, document_source_ids(documents), None)
    }

    pub(crate) fn resolve<'a>(&'a self, id: &'a str) -> Option<&'a str> {
        match self.remaps.get(id) {
            Some(selected) => Some(selected),
            None if !self.requested.contains(id) => Some(id),
            None => None,
        }
    }

    pub(crate) fn matches(&self, previous: &str, selected: &str) -> bool {
        self.resolve(previous) == Some(selected)
    }

    fn include(&mut self, remap: SemanticRevisionRemap) -> Result<(), PluginError> {
        if remap.incoming.kind != SemanticRecordKind::Element
            || remap.accepted.kind != SemanticRecordKind::Element
            || !self.requested.contains(&remap.incoming.record_id)
            || remap.accepted.record_id.trim().is_empty()
            || self.remaps.contains_key(&remap.incoming.record_id)
        {
            return Err(PluginError::new(
                "invalid_source_family_read",
                format!(
                    "invalid source remap `{remap:?}`; expected one selected element for each requested source"
                ),
                false,
            ));
        }
        self.remaps
            .insert(remap.incoming.record_id, remap.accepted.record_id);
        Ok(())
    }
}

pub(crate) fn document_source_ids(documents: &[KnowledgeDocument]) -> BTreeSet<String> {
    documents
        .iter()
        .flat_map(|document| {
            document
                .references
                .iter()
                .filter_map(|reference| match reference {
                    KnowledgeDocumentReference::SemanticElement {
                        semantic_element_id,
                    } => Some(semantic_element_id.as_str()),
                    _ => None,
                })
                .chain(
                    [
                        document.metadata["generation"]["source_element_id"].as_str(),
                        document.metadata["provenance"]["semantic_element_id"].as_str(),
                        document.metadata["nucleus"]["target_element_id"].as_str(),
                    ]
                    .into_iter()
                    .flatten(),
                )
        })
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
#[path = "source_revisions/tests.rs"]
mod tests;
