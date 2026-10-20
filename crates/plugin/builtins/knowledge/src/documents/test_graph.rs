//! Named fake I/O for the real document owner used by Knowledge’s close tests.

use crate::KnowledgePlugin;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct FakeDocumentGraph {
    pub(crate) elements: BTreeMap<String, Value>,
    pub(crate) artifacts: BTreeMap<String, Value>,
    pub(crate) writes: usize,
    pub(crate) published_document_ids: Vec<String>,
    pub(crate) embeddings: FakeDocumentEmbeddings,
    pub(crate) reject_unscoped_reads: bool,
    pub(crate) targeted_reads: Vec<Value>,
}

#[derive(Default)]
pub(crate) struct FakeDocumentEmbeddings {
    pub(crate) enabled: bool,
}

impl FakeDocumentEmbeddings {
    fn embed(&self, input: &Value) -> Result<Value, PluginError> {
        if !self.enabled {
            return Err(PluginError::unknown_capability("neural.embed"));
        }
        let vectors = input["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|text| {
                let text = text.as_str().unwrap();
                if text.contains("pipeline") || text.contains("connections") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect::<Vec<_>>();
        Ok(json!({"vectors": vectors}))
    }
}

impl HostCallTransport for FakeDocumentGraph {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        match (capability, input["operation"].as_str()) {
            ("neural.embed", _) => self.embeddings.embed(&input),
            ("storage.semantic", Some("element")) if self.reject_unscoped_reads => {
                Err(PluginError::new(
                    "unscoped_document_read",
                    "Expected an exact project-scoped document query",
                    false,
                ))
            }
            ("storage.semantic", Some("element")) => Ok(
                json!({"element": self.elements.get(input["semantic_element_id"].as_str().unwrap())}),
            ),
            ("storage.semantic", Some("artifact")) => {
                Ok(json!({"artifact": self.artifacts.get(input["artifact_id"].as_str().unwrap())}))
            }
            ("storage.semantic", Some("project_artifacts")) => Ok(
                json!({"artifacts":self.artifacts.values().filter(|artifact| artifact["project_root"] == input["project_root"]).collect::<Vec<_>>()}),
            ),
            ("storage.semantic", Some("project_roots")) => {
                Ok(json!({"project_roots": self.roots()}))
            }
            ("storage.semantic", Some("scoped_read")) => Ok(self.scoped(&input)),
            ("storage.semantic", Some("elements_by_ids_including_inactive")) => {
                self.targeted_reads.push(input.clone());
                Ok(self.targeted(&input))
            }
            ("plugin.invoke", _) if input["export_id"] == "ingest_index_batch" => {
                self.ingest(&input["input"])
            }
            _ => Err(PluginError::unknown_capability(capability)),
        }
    }
}

impl FakeDocumentGraph {
    pub(crate) fn targeted(&self, input: &Value) -> Value {
        let ids = input["semantic_element_ids"].as_array().unwrap();
        let elements: Vec<&Value> = self
            .elements
            .values()
            .filter(|row| {
                let document_id =
                    row["metadata"]["indexer_metadata"]["knowledge_document"]["document_id"]
                        .as_str();
                row["project_root"] == input["project_root"]
                    && (ids.contains(&row["semantic_element_id"])
                        || document_id
                            .is_some_and(|id| ids.contains(&json!(super::records::element_id(id)))))
            })
            .collect();
        json!({"elements": elements})
    }
    pub(crate) fn call(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        let mut context = PluginContext::for_test(self);
        KnowledgePlugin::default().dispatch(export, input, &mut context)
    }

    pub(crate) fn roots(&self) -> Vec<String> {
        self.elements
            .values()
            .filter_map(|row| row["project_root"].as_str().map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn scoped(&self, input: &Value) -> Value {
        let root = &input["request"]["Structure"]["scope"]["project_root"];
        let elements: Vec<&Value> = self
            .elements
            .values()
            .filter(|row| {
                &row["project_root"] == root
                    && (input["request"]["Structure"]["include_inactive"] == true
                        || row["lifecycle"] != "inactive")
            })
            .collect();
        json!({"project_root": root, "elements": elements, "relationships": []})
    }

    pub(crate) fn ingest(&mut self, batch: &Value) -> Result<Value, PluginError> {
        self.writes += 1;
        let removed = batch["removed_paths"].as_array().unwrap();
        for row in self.elements.values_mut() {
            if row["project_root"] == batch["project_root"] && removed.contains(&row["path"]) {
                row["lifecycle"] = json!("inactive");
            }
        }
        for element in batch["semantic_elements"].as_array().unwrap() {
            self.published_document_ids.push(
                element["metadata"]["knowledge_document"]["document_id"]
                    .as_str()
                    .unwrap()
                    .into(),
            );
            let stored = self.stored_element(batch, element);
            self.elements.insert(
                element["semantic_element_id"].as_str().unwrap().into(),
                stored,
            );
        }
        Ok(json!({"output": {"accepted": true}}))
    }

    pub(crate) fn stored_element(&self, batch: &Value, element: &Value) -> Value {
        let mut stored = element.clone();
        stored["project_root"] = batch["project_root"].clone();
        stored["element_kind"] = element["semantic_element_type"].clone();
        stored["name"] = element["semantic_element_name"].clone();
        stored["lifecycle"] = json!("active");
        stored["metadata"] = json!({"provider_instance_id": batch["provider_instance_id"],
            "indexer_metadata": element["metadata"]});
        stored
    }
}
