//! Storage-neutral artifact content policy. Adapters own blob persistence and
//! reference allocation; the portable record owns preview and size semantics.
use crate::SemanticArtifact;
pub(crate) const SEARCHABLE_ARTIFACT_TEXT_LIMIT_BYTES: usize = 4 * 1024;

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

pub(crate) fn searchable_artifact_text(content: &[u8]) -> Option<String> {
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
}
