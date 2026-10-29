//! Owns frozen, instance-scoped generation intents in the existing plugin row store.
//! The generation owner calls capture/list/start/freeze_proposal/finish; no revision policy lives here.
use super::*;
use lumvise_contracts::KnowledgeDocumentIdentity;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GenerationJobScope {
    client_instance_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GenerationDocumentBase {
    pub(super) document_id: String,
    pub(super) expected: Option<documents::ObservedDocumentRevision>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingArtifactJob {
    pub(super) client_instance_id: String,
    pub(super) project_id: String,
    pub(super) job_id: Option<String>,
    pub(super) project_root: String,
    pub(super) semantic_element_id: String,
    pub(super) content_fingerprint: String,
    pub(super) priority: SubmitPriority,
    pub(super) lost_resubmits: u32,
    pub(super) input: Value,
    pub(super) document: GenerationDocumentBase,
    pub(super) proposal: Option<KnowledgeDocument>,
}

impl GenerationJobScope {
    pub(super) fn current(context: &mut PluginContext<'_>) -> Result<Self, PluginError> {
        let output = context.host_call(CAPABILITY, json!({"operation":"scope"}))?;
        let scope: Self = crate::parse(output, "execution scope with a saved client_instance_id")?;
        if scope.client_instance_id.trim().is_empty() || scope.client_instance_id.contains('\0') {
            return Err(invalid_generation_client(
                &scope.client_instance_id,
                "a non-empty saved startup identity without NUL characters",
            ));
        }
        Ok(scope)
    }

    pub(super) fn pending(
        &self,
        context: &mut PluginContext<'_>,
    ) -> Result<Vec<PendingArtifactJob>, PluginError> {
        let rows: Vec<PendingArtifactJob> = storage::list(context, JOBS, Some(&self.prefix()))?;
        if let Some(foreign) = rows
            .iter()
            .find(|row| row.client_instance_id != self.client_instance_id)
        {
            return Err(invalid_generation_client(
                &foreign.client_instance_id,
                &self.client_instance_id,
            ));
        }
        Ok(rows)
    }

    pub(super) fn failures(
        &self,
        context: &mut PluginContext<'_>,
    ) -> Result<Vec<ArtifactGenerationFailure>, PluginError> {
        storage::list(context, FAILURES, Some(&self.prefix()))
    }

    pub(super) fn key(&self, element: &SemanticElement) -> String {
        generation_job_key(
            &self.client_instance_id,
            &element.project_root,
            &element.semantic_element_id,
        )
    }

    pub(super) fn hold(
        &self,
        element: &SemanticElement,
        error: &PluginError,
        context: &mut PluginContext<'_>,
    ) -> Result<(), PluginError> {
        let key = self.key(element);
        storage::mutate_rows_if_unchanged::<PendingArtifactJob>(
            context,
            JOBS,
            &key,
            None,
            vec![
                json!({"operation":"put", "table_name":FAILURES, "row_key":key,
                "value":ArtifactGenerationFailure::for_element(element, error)}),
            ],
        )?;
        Ok(())
    }

    fn prefix(&self) -> String {
        generation_client_prefix(&self.client_instance_id)
    }
}

impl PendingArtifactJob {
    pub(super) fn capture(
        element: &SemanticElement,
        artifacts: &[KnowledgeDocument],
        priority: SubmitPriority,
        scope: &GenerationJobScope,
        sources: &crate::source_revisions::SelectedSources,
        context: &mut PluginContext<'_>,
    ) -> Result<Self, PluginError> {
        let document = GenerationDocumentBase::capture(element, artifacts, sources, context)?;
        let mut input = target_input(element).ok_or_else(|| stale_element(element))?;
        input["document_id"] = json!(document.document_id);
        Ok(Self {
            client_instance_id: scope.client_instance_id.clone(),
            project_id: crate::publication_observation::project_id(context, &element.project_root)?,
            job_id: None,
            project_root: element.project_root.clone(),
            semantic_element_id: element.semantic_element_id.clone(),
            content_fingerprint: element.content_fingerprint.clone().unwrap_or_default(),
            priority,
            lost_resubmits: 0,
            input,
            document,
            proposal: None,
        })
    }

    pub(super) fn key(&self) -> String {
        generation_job_key(
            &self.client_instance_id,
            &self.project_root,
            &self.semantic_element_id,
        )
    }

    pub(super) fn replace(
        &self,
        expected: Option<&Self>,
        context: &mut PluginContext<'_>,
    ) -> Result<(), PluginError> {
        storage::mutate_rows_if_unchanged(
            context,
            JOBS,
            &self.key(),
            expected,
            vec![json!({"operation":"put", "table_name":JOBS, "row_key":self.key(), "value":self})],
        )?;
        Ok(())
    }

    pub(super) fn start(&self, context: &mut PluginContext<'_>) -> Result<Self, PluginError> {
        self.validate_lineage(context)?;
        let output = context.host_call(
            CAPABILITY,
            json!({"operation":"submit", "project_root":self.project_root,
            "capability_id":GENERATOR, "idempotency_key":self.idempotency_key()?,
            "input":self.input, "priority":self.priority}),
        )?;
        let id = output["job_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid_job(&output))?;
        let mut started = self.clone();
        started.job_id = Some(id.into());
        started.replace(Some(self), context)?;
        Ok(started)
    }

    pub(super) fn freeze_proposal(
        &mut self,
        document: KnowledgeDocument,
        context: &mut PluginContext<'_>,
    ) -> Result<(), PluginError> {
        let mut prepared = self.clone();
        prepared.proposal = Some(document);
        prepared.replace(Some(self), context)?;
        *self = prepared;
        Ok(())
    }

    pub(super) fn finish(
        &self,
        failure: Option<ArtifactGenerationFailure>,
        context: &mut PluginContext<'_>,
    ) -> Result<(), PluginError> {
        let key = self.key();
        let result = match failure {
            Some(failure) => {
                json!({"operation":"put", "table_name":FAILURES, "row_key":key, "value":failure})
            }
            None => json!({"operation":"delete", "table_name":FAILURES, "row_key":key}),
        };
        storage::mutate_rows_if_unchanged(
            context,
            JOBS,
            &key,
            Some(self),
            vec![
                result,
                json!({"operation":"delete", "table_name":JOBS, "row_key":key}),
            ],
        )?;
        Ok(())
    }

    pub(super) fn validate_lineage(
        &self,
        context: &mut PluginContext<'_>,
    ) -> Result<(), PluginError> {
        let actual = crate::publication_observation::project_id(context, &self.project_root)?;
        if actual == self.project_id {
            return Ok(());
        }
        Err(PluginError::new(
            "knowledge_project_mismatch",
            format!(
                "generation for `{}` returned lineage `{actual}`; expected frozen lineage `{}`",
                self.project_root, self.project_id
            ),
            false,
        ))
    }

    fn idempotency_key(&self) -> Result<String, PluginError> {
        let frozen = (
            &self.client_instance_id,
            &self.project_id,
            &self.semantic_element_id,
            &self.input,
            &self.document,
        );
        let encoded =
            serde_json::to_vec(&frozen).map_err(|error| invalid_job(&json!(error.to_string())))?;
        Ok(format!("functional:{:x}", Sha256::digest(encoded)))
    }
}

impl GenerationDocumentBase {
    fn capture(
        element: &SemanticElement,
        artifacts: &[KnowledgeDocument],
        sources: &crate::source_revisions::SelectedSources,
        context: &mut PluginContext<'_>,
    ) -> Result<Self, PluginError> {
        let observed = functional::existing_for_source(artifacts, element, sources);
        let document_id = observed.map_or_else(
            || functional::document_id_for(element),
            |document| document.document_id.clone(),
        );
        let document = documents::observe_generation_document(
            context,
            &KnowledgeDocumentIdentity {
                project_root: element.project_root.clone(),
                document_id: document_id.clone(),
            },
        )?;
        Ok(Self {
            document_id,
            expected: observed
                .or(document.as_ref())
                .map(documents::ObservedDocumentRevision::from),
        })
    }
}

fn stale_element(element: &SemanticElement) -> PluginError {
    PluginError::new(
        "invalid_generation_target",
        format!(
            "generation target `{}` has fingerprint `{:?}`; expected indexed source text with a line range",
            element.semantic_element_id, element.content_fingerprint
        ),
        false,
    )
}

fn generation_job_key(client: &str, root: &str, element_id: &str) -> String {
    format!("{}{root}\0{element_id}", generation_client_prefix(client))
}

fn generation_client_prefix(client: &str) -> String {
    // A fixed-width digest keeps opaque client names in distinct row-key namespaces.
    format!("client:{:x}:", Sha256::digest(client.as_bytes()))
}

fn invalid_generation_client(actual: &str, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_project_execution_scope",
        format!("generation client_instance_id `{actual:?}`; expected `{expected}`"),
        false,
    )
}
