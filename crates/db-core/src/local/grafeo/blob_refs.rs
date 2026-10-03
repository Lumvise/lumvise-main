pub(crate) fn unique_blob_content_ref(artifact_id: &str) -> String {
    format!(
        "sql://artifact_blobs/{}/{}",
        artifact_id,
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
}
