//! Copies a referenced attachment through portable persistence; blob bytes stay
//! inside the host instead of traversing the plugin process boundary as JSON.
use super::{execute, unexpected_result};
use lumvise_db_core::{DbError, SemanticOperation, SemanticPersistence, SemanticResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) fn copy_attachment(
    semantic: &dyn SemanticPersistence,
    source_id: &str,
    target_id: &str,
    content_ref: &str,
) -> lumvise_db_core::Result<Value> {
    if target_id.trim().is_empty() || target_id == source_id {
        return Err(DbError::invalid_value(
            target_id,
            "a distinct nonempty destination artifact id",
        ));
    }
    let blob = required_attachment(semantic, source_id, content_ref)?;
    let prefix = if content_ref.starts_with("canvas-file:") {
        "canvas-file"
    } else {
        "artifact-transfer"
    };
    let target_ref = format!("{prefix}:{target_id}:{:x}", Sha256::digest(&blob.content));
    let result = execute(
        semantic,
        SemanticOperation::ArtifactBlobPut {
            content_ref: target_ref.clone(),
            artifact_id: target_id.into(),
            media_type: blob.media_type,
            content: blob.content,
        },
    )?;
    match result {
        SemanticResult::ArtifactBlob(Some(_)) => Ok(json!({"content_ref": target_ref})),
        other => unexpected_result("copied artifact attachment", other),
    }
}

fn required_attachment(
    semantic: &dyn SemanticPersistence,
    source_id: &str,
    content_ref: &str,
) -> lumvise_db_core::Result<lumvise_db_core::ArtifactBlob> {
    let result = execute(
        semantic,
        SemanticOperation::ArtifactBlobGet {
            content_ref: content_ref.into(),
        },
    )?;
    let blob = match result {
        SemanticResult::ArtifactBlob(Some(blob)) => blob,
        other => {
            return Err(DbError::invalid_value(
                format!("{other:?}"),
                format!("existing attachment `{content_ref}` for source `{source_id}`"),
            ));
        }
    };
    if blob.artifact_id == source_id {
        return Ok(blob);
    }
    let source = execute(
        semantic,
        SemanticOperation::Artifact {
            artifact_id: source_id.into(),
        },
    )?;
    if let SemanticResult::Artifact(Some(source)) = source {
        if source.metadata["knowledge"]["metadata"]["inheritance"]["origin_artifact_id"]
            == blob.artifact_id
        {
            return Ok(blob);
        }
    }
    Err(DbError::invalid_value(
        content_ref,
        format!("attachment owned by source artifact `{source_id}` or its recorded origin"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_db_core::LocalPersistence;

    #[test]
    fn copied_attachment_has_independent_ownership_and_preserves_source() {
        let persistence = LocalPersistence::in_memory().unwrap();
        execute(
            &persistence,
            SemanticOperation::ArtifactBlobPut {
                content_ref: "canvas-file:source:image".into(),
                artifact_id: "source".into(),
                media_type: "image/png".into(),
                content: vec![1, 2, 3],
            },
        )
        .unwrap();
        let first =
            copy_attachment(&persistence, "source", "copy", "canvas-file:source:image").unwrap();
        let second =
            copy_attachment(&persistence, "source", "copy", "canvas-file:source:image").unwrap();
        assert_eq!(first, second);
        let copied =
            required_attachment(&persistence, "copy", first["content_ref"].as_str().unwrap())
                .unwrap();
        assert_eq!(copied.artifact_id, "copy");
        assert_eq!(copied.content, vec![1, 2, 3]);
        assert!(required_attachment(&persistence, "source", "canvas-file:source:image").is_ok());
        assert!(
            copy_attachment(&persistence, "other", "copy", "canvas-file:source:image").is_err()
        );
    }
}
