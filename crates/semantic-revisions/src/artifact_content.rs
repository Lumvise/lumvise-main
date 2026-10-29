//! Storage-neutral artifact content policy. Adapters own blob persistence and
//! reference allocation; the portable record owns preview and size semantics.
use crate::SemanticArtifact;
use crate::{SemanticArtifactRevisionWrite, SemanticRevisionResult};
pub const SEARCHABLE_ARTIFACT_TEXT_LIMIT_BYTES: usize = 4 * 1024;

impl SemanticArtifact {
    /// Describes content stored under an adapter-allocated blob reference.
    /// Example: `artifact.with_blob_content("sql://blob/1".into(), b"hello")`.
    pub fn with_blob_content(&self, content_ref: String, content: &[u8]) -> Self {
        let mut artifact = self.clone();
        artifact.content_size_bytes = Some(content.len());
        artifact.searchable_text = searchable_artifact_text(content);
        artifact.content = None;
        artifact.content_ref = Some(content_ref);
        artifact
    }
}

impl SemanticArtifactRevisionWrite {
    /// Allocates deterministic content identity before an adapter stages bytes.
    /// Example: `write.stored_artifact("project-uuid")?` also survives checkout relocation.
    pub fn stored_artifact(&self, project_id: &str) -> SemanticRevisionResult<SemanticArtifact> {
        crate::revision_index::require_project_identity(project_id)?;
        let Some(content) = &self.artifact.content else {
            return Ok(self.artifact.clone());
        };
        let content_ref = format!(
            "revision-blob:{}",
            crate::revision_index::digest(&(project_id, &self.media_type, content))?
        );
        Ok(self
            .artifact
            .with_blob_content(content_ref, content.as_bytes()))
    }
}

pub fn searchable_artifact_text(content: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(content);
    (!text.is_empty()).then(|| {
        text.chars()
            .take(SEARCHABLE_ARTIFACT_TEXT_LIMIT_BYTES)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_preserves_character_limit_and_lossy_utf8_policy() {
        assert_eq!(searchable_artifact_text(b""), None);
        assert_eq!(searchable_artifact_text(&[255]), Some("�".into()));
        assert_eq!(
            searchable_artifact_text("é".repeat(5000).as_bytes())
                .unwrap()
                .chars()
                .count(),
            4096
        );
    }

    #[test]
    fn deterministic_content_identity_is_project_scoped_and_requires_its_durable_id() {
        let write: SemanticArtifactRevisionWrite = serde_json::from_value(serde_json::json!({
            "artifact": { "artifact_id":"note", "semantic_element_id":"source", "artifact_kind":"annotation",
                "title":"Note", "content":"same bytes", "dependencies":[], "metadata":{} },
            "media_type":"text/plain"
        })).unwrap();
        let first = write.stored_artifact("project-one").unwrap();
        assert_eq!(first, write.stored_artifact("project-one").unwrap());
        assert_ne!(
            first.content_ref,
            write.stored_artifact("project-two").unwrap().content_ref
        );
        assert!(write.stored_artifact("").is_err());
    }
}
