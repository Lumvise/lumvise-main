//! Remote archive requests use server-owned staging paths exclusively. The
//! adapter still consumes the unchanged portable filesystem operation locally.

use std::io::Write;

use lumvise_db_core::{DbError, SemanticOperation, SemanticPersistence, SemanticResult};
use lumvise_resource_routing::{
    InvocationControl,
    protocol::{PZ_ARCHIVE_CHUNK_TYPE, TypedBinaryChunkV1},
};

pub(crate) fn execute(
    semantic: &dyn SemanticPersistence,
    operation: SemanticOperation,
    chunks: &[&TypedBinaryChunkV1],
    control: &InvocationControl,
) -> Result<(SemanticResult, Option<Vec<u8>>), DbError> {
    match operation {
        SemanticOperation::ImportPzSnapshot {
            project_root,
            input_path,
        } => {
            require_empty_path(&input_path)?;
            let mut archive = tempfile::NamedTempFile::new()?;
            write_chunks(archive.as_file_mut(), chunks, control)?;
            let result = semantic.execute(
                SemanticOperation::ImportPzSnapshot {
                    project_root,
                    input_path: archive.path().to_string_lossy().into_owned(),
                },
                control,
            )?;
            Ok((result, None))
        }
        SemanticOperation::CreatePzSnapshot {
            project_root,
            output_path,
        } => {
            require_empty_path(&output_path)?;
            require_no_chunks(chunks)?;
            export_archive(semantic, project_root, control)
        }
        operation => {
            require_no_chunks(chunks)?;
            semantic
                .execute(operation, control)
                .map(|result| (result, None))
        }
    }
}

fn export_archive(
    semantic: &dyn SemanticPersistence,
    project_root: String,
    control: &InvocationControl,
) -> Result<(SemanticResult, Option<Vec<u8>>), DbError> {
    let directory = tempfile::tempdir()?;
    let archive = directory.path().join("snapshot.pz");
    let result = semantic.execute(
        SemanticOperation::CreatePzSnapshot {
            project_root,
            output_path: archive.to_string_lossy().into_owned(),
        },
        control,
    )?;
    let SemanticResult::PzSnapshot(mut metadata) = result else {
        return Err(invalid_archive("missing PzSnapshot result"));
    };
    ensure_active(control)?;
    let bytes = std::fs::read(&archive)?;
    ensure_active(control)?;
    if metadata.output_bytes != bytes.len() as u64 {
        return Err(invalid_archive("archive size differs from snapshot result"));
    }
    metadata.output_path.clear();
    Ok((SemanticResult::PzSnapshot(metadata), Some(bytes)))
}

fn write_chunks(
    output: &mut std::fs::File,
    chunks: &[&TypedBinaryChunkV1],
    control: &InvocationControl,
) -> Result<(), DbError> {
    if chunks.is_empty() {
        return Err(invalid_archive("missing archive binary chunks"));
    }
    for (index, chunk) in chunks.iter().enumerate() {
        ensure_active(control)?;
        if chunk.type_name != PZ_ARCHIVE_CHUNK_TYPE
            || chunk.metadata_json.is_some()
            || chunk.final_chunk != (index + 1 == chunks.len())
        {
            return Err(invalid_archive(
                "unordered PZ binary chunks or invalid final chunk",
            ));
        }
        output.write_all(&chunk.bytes)?;
    }
    output.flush()?;
    ensure_active(control)
}

fn ensure_active(control: &InvocationControl) -> Result<(), DbError> {
    if control.is_cancelled() || control.is_expired() {
        return Err(DbError::pz(
            lumvise_db_core::PzFailurePhase::Cancellation,
            "archive invocation cancelled or expired",
        ));
    }
    Ok(())
}
fn require_empty_path(path: &str) -> Result<(), DbError> {
    if !path.is_empty() {
        return Err(invalid_archive("remote filesystem path"));
    }
    Ok(())
}
fn require_no_chunks(chunks: &[&TypedBinaryChunkV1]) -> Result<(), DbError> {
    if !chunks.is_empty() {
        return Err(invalid_archive(
            "unexpected archive bytes for this operation",
        ));
    }
    Ok(())
}

fn invalid_archive(value: &str) -> DbError {
    DbError::invalid_value(
        value,
        "PZ binary transfer with empty remote paths and ordered chunks",
    )
}
