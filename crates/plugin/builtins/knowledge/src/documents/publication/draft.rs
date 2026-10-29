//! Prepares one observed locator and its payload; acceptance remains the shared policy's job.
use lumvise_contracts::{KnowledgeDocument, SemanticIndexObservation, SemanticRecordRef};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use crate::documents::{record_format as format, records, reference_graph};
use crate::publication_journal::FrozenKnowledgePublication;

pub(super) struct DocumentPublicationDraft<'a> {
    document: &'a KnowledgeDocument,
    anchor: Value,
    previous: Option<Value>,
    project_id: String,
    artifacts: Vec<Value>,
}

impl<'a> DocumentPublicationDraft<'a> {
    pub(super) fn capture(
        context: &mut PluginContext<'_>,
        document: &'a KnowledgeDocument,
        artifacts: Vec<Value>,
        expected: Option<&str>,
    ) -> Result<Self, PluginError> {
        let anchor = super::root::ensure(context, &document.project_root)?;
        let previous =
            records::selected_locator(context, &document.project_root, &document.document_id)?;
        super::validate_base(expected, previous.as_ref())?;
        let project_id =
            crate::publication_observation::project_id(context, &document.project_root)?;
        Ok(Self {
            document,
            anchor,
            previous,
            project_id,
            artifacts,
        })
    }

    pub(super) fn freeze(
        mut self,
        context: &mut PluginContext<'_>,
        operation_id: &str,
        dependencies: Vec<Value>,
    ) -> Result<FrozenKnowledgePublication, PluginError> {
        let incoming = self
            .previous
            .as_ref()
            .and_then(format::body_id)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "knowledge-document-body:{}:{}:{operation_id}",
                    self.project_id,
                    hex::encode(&self.document.document_id)
                )
            });
        let mut body = format::body_artifact(
            self.document,
            self.anchor["semantic_element_id"].as_str().unwrap(),
            &incoming,
        );
        body["dependencies"]
            .as_array_mut()
            .expect("prepared body dependencies")
            .extend(dependencies);
        body["dependencies"]
            .as_array_mut()
            .expect("prepared body dependencies")
            .extend(reference_graph::artifact_dependencies(
                context,
                self.document,
            )?);
        let body_id = self.payload_id(context, operation_id, &body)?;
        self.artifacts.push(body);
        let paths = vec![format::storage_path(&self.document.document_id)];
        let observation = super::observe(
            context,
            &self.document.project_root,
            &paths,
            &self.artifacts,
            self.previous.as_ref(),
        )?;
        self.require_lineage(&observation)?;
        self.finish(context, operation_id, paths, observation, &body_id)
    }

    fn finish(
        mut self,
        context: &mut PluginContext<'_>,
        operation_id: &str,
        paths: Vec<String>,
        observation: SemanticIndexObservation,
        body_id: &str,
    ) -> Result<FrozenKnowledgePublication, PluginError> {
        let element = self.locator(body_id, operation_id);
        self.remap_resources(&element["semantic_element_id"]);
        let relationships = reference_graph::relationships(
            context,
            self.document,
            element["semantic_element_id"].as_str().unwrap(),
        )?;
        Ok(super::batch(
            &self.document.project_root,
            &self.anchor["semantic_source_id"],
            observation,
            operation_id,
            json!({
                "semantic_elements":[element], "semantic_relationships":relationships,
                "replace_paths":paths, "semantic_artifacts":self.artifacts
            }),
        ))
    }

    fn require_lineage(&self, observation: &SemanticIndexObservation) -> Result<(), PluginError> {
        if observation.project_id == self.project_id {
            return Ok(());
        }
        Err(crate::documents::invalid(
            "knowledge_project_mismatch",
            &observation.project_id,
            &format!("original document lineage {}", self.project_id),
        ))
    }

    fn payload_id(
        &self,
        context: &mut PluginContext<'_>,
        operation_id: &str,
        body: &Value,
    ) -> Result<String, PluginError> {
        let incoming = body["artifact_id"]
            .as_str()
            .expect("prepared payload identity");
        if self.previous.as_ref().and_then(format::body_id).is_none()
            || !body_changed(context, incoming, body)?
        {
            return Ok(incoming.into());
        }
        lumvise_semantic_revisions::semantic_revision_successor_id(
            &self.project_id,
            "local",
            operation_id,
            &SemanticRecordRef::artifact(incoming),
        )
        .map_err(|error| {
            crate::documents::invalid("invalid_document_revision", incoming, &error.to_string())
        })
    }

    fn locator(&self, body_id: &str, operation_id: &str) -> Value {
        let id = self
            .previous
            .as_ref()
            .map(|row| row["semantic_element_id"].clone())
            .unwrap_or_else(|| {
                json!(format!(
                    "knowledge-document:{}:{}:{operation_id}",
                    self.project_id,
                    hex::encode(&self.document.document_id)
                ))
            });
        let mut locator = json!({"schema":2, "document_id":self.document.document_id,
            "artifact_id":body_id});
        if let Some(access) = &self.document.access {
            locator["access"] = json!(access);
        }
        json!({"semantic_element_id":id, "semantic_source_id":self.anchor["semantic_source_id"],
            "semantic_element_type":"virtual_document", "semantic_element_name":self.document.title,
            "path":format::storage_path(&self.document.document_id), "parent_element_id":null,
            "revision_dependencies":[SemanticRecordRef::artifact(body_id)],
            "metadata":{"knowledge_document":locator}})
    }

    fn remap_resources(&mut self, owner: &Value) {
        for artifact in &mut self.artifacts {
            if artifact["semantic_element_id"] == self.document.semantic_element_id {
                artifact["semantic_element_id"] = owner.clone();
            }
        }
    }
}

fn body_changed(
    context: &mut PluginContext<'_>,
    id: &str,
    body: &Value,
) -> Result<bool, PluginError> {
    let stored = context.host_call(
        "storage.semantic",
        json!({"operation":"artifact", "artifact_id":id}),
    )?;
    Ok([
        "artifact_kind",
        "semantic_element_id",
        "title",
        "metadata",
        "content",
        "dependencies",
    ]
    .iter()
    .any(|key| body[key] != stored["artifact"][key]))
}
