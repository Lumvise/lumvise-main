use crate::local::grafeo::blob_refs::unique_blob_content_ref;
use crate::local::grafeo::graph_rows::{apply_artifact_upsert, prepare_artifact_upsert};
use crate::local::grafeo::semantic_storage::{SemanticStorage, validate_artifact};
use crate::local::sql::artifact_blobs::ArtifactBlobRepository;
use crate::local::sql::validation::require_non_empty;
use crate::{ArtifactTextVector, ChangeDisposition, Result, SemanticArtifact};

pub(crate) type ArtifactVectorWrite<'a> = Option<(&'a str, &'a ArtifactTextVector)>;

impl<'db> SemanticStorage<'db> {
    /// Upserts one semantic artifact and records a storage change.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult, SemanticArtifact, SemanticElement};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let element = SemanticElement { project_root: "/repo".into(), semantic_element_id: "e".into(), semantic_source_id: "s".into(), path: "p".into(), element_kind: "file".into(), name: "p".into(), parent_element_id: None, content_fingerprint: None, start_line: None, end_line: None, lifecycle: "active".into(), match_evidence: None, metadata: serde_json::json!({}) };
    /// let synced = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::SyncStructure {
    ///         project_root: "/repo".into(),
    ///         elements: vec![element],
    ///         relationships: vec![],
    ///     },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(synced, SemanticResult::SyncStructure(report) if report.elements_upserted == 1));
    /// let artifact = SemanticArtifact { artifact_id: "a".into(), semantic_element_id: "e".into(), artifact_kind: "definition".into(), title: "A".into(), content_ref: None, content: None, searchable_text: None, content_size_bytes: None, dependencies: vec![], metadata: serde_json::json!({}) };
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::UpsertArtifact { artifact, media_type: "text/plain".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::UpsertedArtifact { artifact_id } if artifact_id == "a"));
    /// ```
    pub fn upsert_artifact(&self, artifact: &SemanticArtifact) -> Result<()> {
        self.commit_artifact_update(artifact, None)
    }

    /// Stores artifact content inline in Grafeo or spills large content to SQL.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult, SemanticArtifact, SemanticElement};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let element = SemanticElement { project_root: "/repo".into(), semantic_element_id: "e".into(), semantic_source_id: "s".into(), path: "p".into(), element_kind: "file".into(), name: "p".into(), parent_element_id: None, content_fingerprint: None, start_line: None, end_line: None, lifecycle: "active".into(), match_evidence: None, metadata: serde_json::json!({}) };
    /// let synced = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::SyncStructure {
    ///         project_root: "/repo".into(),
    ///         elements: vec![element],
    ///         relationships: vec![],
    ///     },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(synced, SemanticResult::SyncStructure(report) if report.elements_upserted == 1));
    /// let mut artifact = SemanticArtifact { artifact_id: "a".into(), semantic_element_id: "e".into(), artifact_kind: "definition".into(), title: "A".into(), content_ref: None, content: None, searchable_text: None, content_size_bytes: None, dependencies: vec![], metadata: serde_json::json!({}) };
    /// artifact.content = Some("hello".into());
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::UpsertArtifact { artifact, media_type: "text/plain".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::UpsertedArtifact { artifact_id } if artifact_id == "a"));
    /// let loaded = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::Artifact { artifact_id: "a".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(loaded, SemanticResult::Artifact(Some(artifact)) if artifact.content.as_deref() == Some("hello")));
    /// ```
    pub fn upsert_artifact_content(
        &self,
        artifact: &SemanticArtifact,
        media_type: &str,
        content: &[u8],
    ) -> Result<()> {
        require_non_empty(media_type, "non-empty media type")?;
        let artifact = self.artifact_with_content_policy(artifact, media_type, content)?;
        self.commit_staged_artifact_update(&artifact, None)
    }

    /// Stores one raw blob an artifact owns, such as a canvas image, under its
    /// original content ref without touching the artifact record.
    pub(crate) fn put_owned_blob(&self, blob: &crate::ArtifactBlob) -> Result<()> {
        ArtifactBlobRepository::with_clock(self.conn, self.clock.clone()).put_blob(
            &blob.content_ref,
            &blob.artifact_id,
            &blob.media_type,
            &blob.content,
        )
    }

    pub(crate) fn artifact_with_content_policy(
        &self,
        artifact: &SemanticArtifact,
        media_type: &str,
        content: &[u8],
    ) -> Result<SemanticArtifact> {
        let content_ref = unique_blob_content_ref(&artifact.artifact_id);
        ArtifactBlobRepository::with_clock(self.conn, self.clock.clone()).put_blob(
            &content_ref,
            &artifact.artifact_id,
            media_type,
            content,
        )?;
        Ok(artifact.with_blob_content(content_ref, content))
    }

    pub(crate) fn commit_staged_artifact_update(
        &self,
        artifact: &SemanticArtifact,
        vector_write: ArtifactVectorWrite<'_>,
    ) -> Result<()> {
        let staged_ref = artifact.content_ref.clone();
        match self.commit_artifact_update(artifact, vector_write) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.delete_blob_ref(staged_ref.as_deref());
                Err(error)
            }
        }
    }

    pub(crate) fn commit_artifact_update(
        &self,
        artifact: &SemanticArtifact,
        vector_write: ArtifactVectorWrite<'_>,
    ) -> Result<()> {
        validate_artifact(artifact)?;
        self.require_element_exists(&artifact.semantic_element_id)?;
        let replaced_ref = self.replaced_blob_ref(artifact)?;
        self.commit_artifact_graph_update(artifact, vector_write)?;
        self.delete_blob_ref(replaced_ref.as_deref());
        Ok(())
    }

    pub(crate) fn commit_artifact_graph_update(
        &self,
        artifact: &SemanticArtifact,
        vector_write: ArtifactVectorWrite<'_>,
    ) -> Result<()> {
        self.commit_preplanned_graph_write(
            |database| prepare_artifact_upsert(database, artifact, vector_write),
            |graph, commit_version, collector, plan| {
                let owner = apply_artifact_upsert(graph, artifact, commit_version, plan)?;
                collector.record_element(&owner, ChangeDisposition::Upserted);
                Ok(())
            },
        )
    }

    fn replaced_blob_ref(&self, artifact: &SemanticArtifact) -> Result<Option<String>> {
        let old_ref = self
            .artifact(&artifact.artifact_id)?
            .and_then(|old| old.content_ref);
        Ok(old_ref.filter(|old_ref| Some(old_ref) != artifact.content_ref.as_ref()))
    }

    fn delete_blob_ref(&self, content_ref: Option<&str>) {
        if let Some(content_ref) = content_ref {
            let _ = ArtifactBlobRepository::with_clock(self.conn, self.clock.clone())
                .delete_blob(content_ref);
        }
    }
}
