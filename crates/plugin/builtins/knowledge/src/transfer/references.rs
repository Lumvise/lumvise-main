use crate::{KnowledgeArtifact, inheritance::KnowledgeTransferMatch};
use lumvise_contracts::ArtifactDependencyTarget;
use lumvise_plugin_sdk::PluginError;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub(super) struct TransferReferences {
    artifacts: HashMap<String, String>,
    elements: HashMap<String, String>,
}

impl TransferReferences {
    pub(super) fn new(selected: &[&KnowledgeTransferMatch]) -> Self {
        let mut artifacts: HashMap<String, HashSet<String>> = HashMap::new();
        let mut elements: HashMap<String, HashSet<String>> = HashMap::new();
        for candidate in selected {
            artifacts
                .entry(candidate.source.artifact_id.clone())
                .or_default()
                .insert(candidate.copy.artifact_id.clone());
            elements
                .entry(candidate.source.semantic_element_id.clone())
                .or_default()
                .insert(candidate.target.semantic_element_id.clone());
        }
        Self {
            artifacts: unambiguous_targets(artifacts),
            elements: unambiguous_targets(elements),
        }
    }

    pub(super) fn prepare(
        &self,
        candidate: &KnowledgeTransferMatch,
    ) -> Result<KnowledgeArtifact, PluginError> {
        let mut copy = candidate.copy.clone();
        let mut artifacts = self.artifacts.clone();
        let mut elements = self.elements.clone();
        artifacts.insert(
            candidate.source.artifact_id.clone(),
            copy.artifact_id.clone(),
        );
        elements.insert(
            candidate.source.semantic_element_id.clone(),
            candidate.target.semantic_element_id.clone(),
        );
        for dependency in &mut copy.dependencies {
            let (id, replacements) = match &mut dependency.target {
                ArtifactDependencyTarget::SemanticElement {
                    semantic_element_id,
                } => (semantic_element_id, &elements),
                ArtifactDependencyTarget::Artifact { artifact_id } => (artifact_id, &artifacts),
            };
            if let Some(target) = replacements.get(id) {
                *id = target.clone();
            }
        }
        remap_structured_fields(&mut copy.metadata, &artifacts, &elements);
        if let Ok(mut document) = serde_json::from_str::<Value>(&copy.content) {
            remap_structured_fields(&mut document, &artifacts, &elements);
            copy.content =
                serde_json::to_string(&document).map_err(super::attachments::encoding_error)?;
        }
        Ok(copy)
    }
}

fn unambiguous_targets(destinations: HashMap<String, HashSet<String>>) -> HashMap<String, String> {
    destinations
        .into_iter()
        .filter_map(|(source, targets)| {
            (targets.len() == 1).then(|| (source, targets.into_iter().next().unwrap()))
        })
        .collect()
}

fn remap_structured_fields(
    value: &mut Value,
    artifacts: &HashMap<String, String>,
    elements: &HashMap<String, String>,
) {
    match value {
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| remap_structured_fields(value, artifacts, elements)),
        Value::Object(fields) => {
            // Explicit canvas links take precedence over natural node targets.
            if fields.len() == 2
                && matches!(
                    fields.get("kind").and_then(Value::as_str),
                    Some("canvas" | "artifact" | "element")
                )
            {
                let replacements = if fields["kind"] == "element" {
                    elements
                } else {
                    artifacts
                };
                if let Some(target) = fields
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| replacements.get(id))
                {
                    fields.insert("id".into(), Value::String(target.clone()));
                }
            }
            for (key, value) in fields {
                if key == "inheritance" {
                    continue;
                } // Provenance must retain the original identities.
                let replacements = if matches!(
                    key.as_str(),
                    "artifactId" | "artifact_id" | "canvasId" | "parentCanvasId"
                ) {
                    artifacts
                } else {
                    elements
                };
                if matches!(
                    key.as_str(),
                    "artifactId"
                        | "artifact_id"
                        | "canvasId"
                        | "elementId"
                        | "semanticElementId"
                        | "semantic_element_id"
                        | "anchorElementId"
                        | "target_element_id"
                        | "parentCanvasId"
                ) {
                    if let Some(replacement) = value.as_str().and_then(|id| replacements.get(id)) {
                        *value = Value::String(replacement.clone());
                    }
                }
                remap_structured_fields(value, artifacts, elements);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn destination_links_change_without_rewriting_text_or_source_provenance() {
        let mut document = json!({"elementId":"source", "title":"source", "inheritance":{"source_semantic_element_id":"source"},"nodes":[{"artifactId":"original","canvasId":"original", "link":{"kind":"canvas","id":"original"}}]});
        remap_structured_fields(
            &mut document,
            &HashMap::from([("original".into(), "copy".into())]),
            &HashMap::from([("source".into(), "target".into())]),
        );
        assert_eq!(document["elementId"], "target");
        assert_eq!(document["title"], "source");
        assert_eq!(
            document["inheritance"]["source_semantic_element_id"],
            "source"
        );
        assert_eq!(document["nodes"][0]["artifactId"], "copy");
        assert_eq!(document["nodes"][0]["canvasId"], "copy");
        assert_eq!(document["nodes"][0]["link"]["id"], "copy");
    }

    #[test]
    fn equal_ids_in_separate_reference_namespaces_keep_distinct_targets() {
        let mut document =
            json!({"artifactId":"same", "elementId":"same", "link":{"kind":"element","id":"same"}});
        remap_structured_fields(
            &mut document,
            &HashMap::from([("same".into(), "artifact-copy".into())]),
            &HashMap::from([("same".into(), "element-copy".into())]),
        );
        assert_eq!(document["artifactId"], "artifact-copy");
        assert_eq!(document["elementId"], "element-copy");
        assert_eq!(document["link"]["id"], "element-copy");
    }
}
