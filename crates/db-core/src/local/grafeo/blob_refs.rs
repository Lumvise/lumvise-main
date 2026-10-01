pub(crate) const SEARCHABLE_ARTIFACT_TEXT_LIMIT_BYTES: usize = 4 * 1024;

pub(crate) fn unique_blob_content_ref(artifact_id: &str) -> String {
    format!(
        "sql://artifact_blobs/{}/{}",
        artifact_id,
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
}

pub(crate) fn searchable_artifact_text(content: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(content);
    if text.is_empty() {
        return None;
    }
    Some(
        text.chars()
            .take(SEARCHABLE_ARTIFACT_TEXT_LIMIT_BYTES)
            .collect(),
    )
}
