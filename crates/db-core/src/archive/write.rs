use super::*;
use crate::PzSnapshotResult;

pub(super) fn publish(
    archive: &SemanticArchive,
    output_path: &Path,
    control: &lumvise_resource_routing::InvocationControl,
) -> Result<PzSnapshotResult> {
    verify_control(control)?;
    let output_path = output_path.to_path_buf();
    let tables = build_tables(
        &archive.snapshot,
        &archive.artifact_blobs,
        &archive.artifact_text_vectors,
        &archive.element_name_vectors,
    )
    .map_err(|error| DbError::pz(crate::PzFailurePhase::Encode, error.to_string()))?;
    verify_control(control)?;
    let mut manifest = build_manifest(
        &archive.snapshot,
        &archive.project_id,
        &archive.snapshot_id,
        &tables,
    )
    .map_err(|error| DbError::pz(crate::PzFailurePhase::Validate, error.to_string()))?;
    manifest.project.canonical_remote = archive.canonical_remote.clone();
    super::validation::validate_manifest(&manifest)
        .map_err(|error| DbError::pz(crate::PzFailurePhase::Validate, error.to_string()))?;
    let manifest_bytes = canonical_json(&manifest)
        .map_err(|error| DbError::pz(crate::PzFailurePhase::Encode, error.to_string()))?;
    let mut entries = Vec::with_capacity(PZ_REQUIRED_ENTRIES.len());
    entries.push(("manifest.json", manifest_bytes));
    for table in &tables {
        verify_control(control)?;
        entries.push((table.name, table.bytes.clone()));
    }
    let temp_path = temporary_sibling(&output_path, &archive.snapshot_id);
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| DbError::pz(crate::PzFailurePhase::Publish, error.to_string()))?;
    }
    let mut committed = false;
    let result: Result<PzSnapshotResult> = (|| {
        verify_control(control)?;
        write_zip(&temp_path, &entries)
            .map_err(|error| DbError::pz(crate::PzFailurePhase::Publish, error.to_string()))?;
        verify_control(control)?;
        let archive = PzArchive::open(&temp_path)
            .map_err(|error| DbError::pz(crate::PzFailurePhase::Validate, error.to_string()))?;
        let mut result = archive
            .result(&temp_path)
            .map_err(|error| DbError::pz(crate::PzFailurePhase::Validate, error.to_string()))?;
        verify_control(control)?;
        atomic_replace(&temp_path, &output_path)
            .map_err(|error| DbError::pz(crate::PzFailurePhase::Publish, error.to_string()))?;
        committed = true;
        result.output_path = output_path.clone();
        Ok(result)
    })();
    if !committed {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

pub(crate) fn verify_control(control: &lumvise_resource_routing::InvocationControl) -> Result<()> {
    if control.is_cancelled() {
        return Err(DbError::pz(
            crate::PzFailurePhase::Cancellation,
            "cancelled invocation",
        ));
    }
    if control.is_expired() {
        return Err(DbError::pz(
            crate::PzFailurePhase::Cancellation,
            "expired invocation",
        ));
    }
    Ok(())
}

pub(super) fn build_manifest(
    snapshot: &SemanticProjectSnapshot,
    project_id: &str,
    snapshot_id: &str,
    tables: &[TableBytes],
) -> Result<PzManifest> {
    let mut entries = Vec::with_capacity(PZ_REQUIRED_ENTRIES.len() - 1);
    for table in tables {
        let (footer, row_groups) = super::validation::integrity_for_table(&table.bytes)?;
        entries.push(PzEntry {
            name: table.name.to_owned(),
            schema: table.schema_id.to_owned(),
            sha256: sha256_hex(&table.bytes),
            uncompressed_bytes: table.bytes.len() as u64,
            row_count: table.rows,
            footer,
            row_groups,
        });
    }
    Ok(PzManifest {
        format_version: PZ_FORMAT_VERSION,
        project_id: project_id.to_owned(),
        snapshot_id: snapshot_id.to_owned(),
        commit_version: snapshot.commit_version,
        published_at: snapshot.published_at.clone(),
        project: PzProjectInfo::default(),
        entries,
    })
}
pub(super) fn write_zip(path: &Path, entries: &[(&str, Vec<u8>)]) -> Result<()> {
    let file = File::create(path)?;
    let mut writer = ZipWriter::new(file);
    writer.set_zip64_comment(Some("PZ1"));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for (name, bytes) in entries {
        writer.start_file(*name, options).map_err(zip_error)?;
        writer.write_all(bytes)?;
    }
    writer.finish().map_err(zip_error)?;
    Ok(())
}

pub(super) fn zip_error(error: zip::result::ZipError) -> DbError {
    DbError::invalid_value(error.to_string(), "valid ZIP64 PZ archive")
}
pub(super) fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(Into::into)
}
pub(super) fn canonical_json_value(value: &serde_json::Value) -> Result<String> {
    Ok(String::from_utf8(canonical_json(value)?).unwrap_or_else(|_| "null".into()))
}
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}
pub(super) fn temporary_sibling(path: &Path, id: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("graph_db.pz");
    path.with_file_name(format!(".{name}.{id}.tmp"))
}
#[cfg(not(windows))]
pub(super) fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
pub(super) fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
pub(crate) fn normalized_project_root(root: &str) -> Result<String> {
    let normalized = root.trim().replace('\\', "/");
    if normalized.is_empty() {
        return Err(DbError::pz(
            crate::PzFailurePhase::Capture,
            format!("invalid project root `{root}`; expected a non-empty root"),
        ));
    }
    if normalized.chars().all(|character| character == '/') {
        return Ok("/".to_owned());
    }
    Ok(normalized.trim_end_matches('/').to_owned())
}
