//! Owns vector change-hook computation; persistence and acknowledgement use the existing owner.
use super::*;
use crate::workspace_activity::EmbeddingBatch;
use lumvise_db_core::{
    ArtifactTextVector, ArtifactTextVectorizer, ChangeBatch, SemanticArtifact, SemanticElement,
    TextEmbeddingTask,
};

const PROGRESS_GROUP_SIZE: usize = 16;
type StoredVectorWork = (
    Vec<StoredSemanticElementNameVector>,
    Vec<StoredArtifactTextVector>,
);

struct VectorWork {
    elements: Vec<SemanticElement>,
    artifacts: Vec<SemanticArtifact>,
}

impl PluginEndpoints<'_> {
    pub(super) fn run_vector_hook(
        &self,
        registration: ChangeHookRegistration,
        vectorizer: &(dyn ArtifactTextVectorizer + Send + Sync),
    ) -> Result<ChangeHookRun> {
        let batch = self.vector_dirty_batch(&registration.hook_name)?;
        let work = self.vector_work(&registration, &batch)?;
        let mut run = vector_run(&registration.hook_name, &batch);
        let progress =
            self.compute_and_store_vectors(&registration.scope.project_root, work, vectorizer)?;
        run.delivered = batch.changed.len();
        run.acknowledged = self.acknowledge_change_hook(batch)?;
        if run.acknowledged {
            if let Some(progress) = &progress {
                progress.succeeded();
            }
        }
        Ok(run)
    }

    fn vector_dirty_batch(&self, hook_name: &str) -> Result<ChangeBatch> {
        match self.app.semantic.execute(
            SemanticOperation::ChangeHookDirtyBatch {
                hook_name: hook_name.into(),
                maximum_elements: HOOK_BATCH_SIZE,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookBatch(batch) => Ok(batch),
            other => Err(semantic_result_error(
                "vector change hook dirty batch",
                other,
            )),
        }
    }

    fn vector_work(
        &self,
        registration: &ChangeHookRegistration,
        batch: &ChangeBatch,
    ) -> Result<VectorWork> {
        let ids = batch
            .changed
            .iter()
            .map(|change| change.element_id.clone())
            .collect::<HashSet<_>>();
        let elements = self.vector_elements(&registration.scope.project_root, ids.clone())?;
        let artifacts = self.vector_artifacts(ids)?;
        Ok(VectorWork::active(elements, artifacts))
    }

    fn vector_elements(&self, root: &str, ids: HashSet<String>) -> Result<Vec<SemanticElement>> {
        match self.app.semantic.execute(
            SemanticOperation::ElementsByIds {
                project_root: root.into(),
                semantic_element_ids: ids,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::Elements(elements) => Ok(elements),
            other => Err(semantic_result_error("vector elements", other)),
        }
    }

    fn vector_artifacts(&self, ids: HashSet<String>) -> Result<Vec<SemanticArtifact>> {
        match self.app.semantic.execute(
            SemanticOperation::ArtifactsForElements {
                semantic_element_ids: ids,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::Artifacts(artifacts) => Ok(artifacts),
            other => Err(semantic_result_error("vector artifacts", other)),
        }
    }

    fn compute_and_store_vectors(
        &self,
        root: &str,
        work: VectorWork,
        vectorizer: &dyn ArtifactTextVectorizer,
    ) -> Result<Option<EmbeddingBatch>> {
        let texts = work.texts();
        if texts.is_empty() {
            return Ok(None);
        }
        let mut progress = self
            .app
            .plugin_host_services
            .activity()
            .track_embeddings(root, texts.len());
        let vectors = embed_work(vectorizer, &texts, &mut progress)?;
        self.publish_vector_work(root, work, vectors, &progress)?;
        Ok(Some(progress))
    }

    fn publish_vector_work(
        &self,
        root: &str,
        work: VectorWork,
        vectors: Vec<ArtifactTextVector>,
        progress: &EmbeddingBatch,
    ) -> Result<()> {
        progress.saving();
        let (names, artifacts) = work.stored(vectors);
        self.store_name_work(root, names)?;
        self.store_artifact_work(root, artifacts)
    }

    fn store_name_work(
        &self,
        root: &str,
        vectors: Vec<StoredSemanticElementNameVector>,
    ) -> Result<()> {
        if vectors.is_empty() {
            return Ok(());
        }
        self.app.semantic.execute(
            SemanticOperation::StoreElementNameVectors {
                project_root: root.into(),
                vectors,
            },
            &InvocationControl::sixty_seconds(),
        )?;
        Ok(())
    }

    fn store_artifact_work(
        &self,
        root: &str,
        vectors: Vec<StoredArtifactTextVector>,
    ) -> Result<()> {
        if vectors.is_empty() {
            return Ok(());
        }
        self.app.semantic.execute(
            SemanticOperation::StoreArtifactTextVectors {
                project_root: root.into(),
                vectors,
            },
            &InvocationControl::sixty_seconds(),
        )?;
        Ok(())
    }
}

fn vector_run(name: &str, batch: &ChangeBatch) -> ChangeHookRun {
    ChangeHookRun {
        hook_name: name.into(),
        base_revision: batch.base_revision,
        target_revision: batch.target_revision,
        delivered: 0,
        retrying: 0,
        dead_lettered: 0,
        acknowledged: false,
    }
}

impl VectorWork {
    fn active(elements: Vec<SemanticElement>, artifacts: Vec<SemanticArtifact>) -> Self {
        let artifacts = active_artifacts(&elements, artifacts);
        let elements = elements
            .into_iter()
            .filter(|element| element.lifecycle == "active" && !element.name.trim().is_empty())
            .collect();
        Self {
            elements,
            artifacts,
        }
    }

    fn texts(&self) -> Vec<&str> {
        self.elements
            .iter()
            .map(|element| element.name.trim())
            .chain(self.artifacts.iter().filter_map(artifact_text))
            .collect()
    }

    fn stored(self, vectors: Vec<ArtifactTextVector>) -> StoredVectorWork {
        let name_count = self.elements.len();
        let mut vectors = vectors.into_iter();
        let names = self
            .elements
            .into_iter()
            .zip(vectors.by_ref().take(name_count))
            .map(stored_name)
            .collect();
        let artifacts = self
            .artifacts
            .into_iter()
            .zip(vectors)
            .map(stored_artifact)
            .collect();
        (names, artifacts)
    }
}

fn active_artifacts(
    elements: &[SemanticElement],
    artifacts: Vec<SemanticArtifact>,
) -> Vec<SemanticArtifact> {
    let active_ids: HashSet<&str> = elements
        .iter()
        .filter(|element| element.lifecycle == "active")
        .map(|element| element.semantic_element_id.as_str())
        .collect();
    // Removal batches retain artifacts on inactive owners. They remain knowledge,
    // but cannot produce searchable vectors until that owner becomes active again.
    let artifacts = artifacts
        .into_iter()
        .filter(|artifact| {
            active_ids.contains(artifact.semantic_element_id.as_str())
                && artifact_text(artifact).is_some()
        })
        .collect();
    artifacts
}

fn artifact_text(artifact: &SemanticArtifact) -> Option<&str> {
    artifact
        .searchable_text
        .as_deref()
        .or(artifact.content.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn stored_name(
    (element, vector): (SemanticElement, ArtifactTextVector),
) -> StoredSemanticElementNameVector {
    StoredSemanticElementNameVector {
        source_text: element.name.trim().into(),
        semantic_element_id: element.semantic_element_id,
        project_root: element.project_root,
        vector,
    }
}

fn stored_artifact(
    (artifact, vector): (SemanticArtifact, ArtifactTextVector),
) -> StoredArtifactTextVector {
    StoredArtifactTextVector {
        source_text: artifact_text(&artifact)
            .expect("filtered artifact text")
            .into(),
        artifact_id: artifact.artifact_id,
        semantic_element_id: artifact.semantic_element_id,
        vector,
    }
}

fn embed_work(
    vectorizer: &dyn ArtifactTextVectorizer,
    texts: &[&str],
    progress: &mut EmbeddingBatch,
) -> Result<Vec<ArtifactTextVector>> {
    let mut vectors = Vec::with_capacity(texts.len());
    for group in texts.chunks(PROGRESS_GROUP_SIZE) {
        let output = embed_group(vectorizer, group)?;
        progress.advance(output.len());
        vectors.extend(output);
    }
    Ok(vectors)
}

fn embed_group(
    vectorizer: &dyn ArtifactTextVectorizer,
    group: &[&str],
) -> Result<Vec<ArtifactTextVector>> {
    let output = vectorizer.vectorize_artifact_texts(group, TextEmbeddingTask::Document)?;
    if output.len() != group.len() {
        return Err(lumvise_db_core::DbError::vectorizer(format!(
            "{} vectors; expected {} ordered vectors",
            output.len(),
            group.len()
        ))
        .into());
    }
    Ok(output)
}
