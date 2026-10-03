//! Owns desktop filesystem staging for remote PZ operations. Only bytes and
//! portable metadata cross the authenticated resource transport.

use std::{io::Write, path::PathBuf};

use lumvise_resource_routing::{
    InvocationControl,
    protocol::{
        InvocationEnvelopeV1, PZ_ARCHIVE_CHUNK_TYPE, TypedBinaryChunkV1,
        invocation_envelope_v1::Payload,
    },
};

use crate::{DbError, PzFailurePhase, Result, SemanticArchive, SemanticOperation, SemanticResult};

pub(super) struct SemanticArchiveTransfer {
    pub input: Option<Vec<u8>>,
    output: Option<(String, PathBuf)>,
}

impl SemanticArchiveTransfer {
    pub fn prepare(
        mut operation: SemanticOperation,
        control: &InvocationControl,
    ) -> Result<(SemanticOperation, Self)> {
        let mut input = None;
        let mut output = None;
        match &mut operation {
            SemanticOperation::ImportPzSnapshot { input_path, .. } => {
                ensure_active(control)?;
                input =
                    Some(std::fs::read(&input_path).map_err(|error| {
                        DbError::pz(PzFailurePhase::Capture, error.to_string())
                    })?);
                input_path.clear();
            }
            SemanticOperation::CreatePzSnapshot {
                project_root,
                output_path,
            } => {
                output = Some((
                    project_root.clone(),
                    PathBuf::from(std::mem::take(output_path)),
                ));
            }
            _ => {}
        }
        ensure_active(control)?;
        Ok((operation, Self { input, output }))
    }

    pub fn append_input(&mut self, envelopes: &mut Vec<InvocationEnvelopeV1>) {
        let Some(bytes) = self.input.take() else {
            return;
        };
        let mut envelope = envelopes[0].clone();
        envelope.sequence = envelopes.len() as u64;
        envelope.payload = Some(Payload::BinaryChunk(TypedBinaryChunkV1 {
            type_name: PZ_ARCHIVE_CHUNK_TYPE.into(),
            bytes,
            metadata_json: None,
            final_chunk: true,
        }));
        envelopes.push(envelope);
    }

    pub fn finish(
        self,
        result: SemanticResult,
        envelopes: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> Result<SemanticResult> {
        let chunks = envelopes
            .iter()
            .filter_map(|envelope| match &envelope.payload {
                Some(Payload::BinaryChunk(chunk)) => Some(chunk),
                _ => None,
            })
            .collect::<Vec<_>>();
        let Some((project_root, output)) = self.output else {
            if !chunks.is_empty() {
                return Err(invalid_archive("unexpected binary result"));
            }
            return Ok(result);
        };
        let SemanticResult::PzSnapshot(mut metadata) = result else {
            return Err(invalid_archive("missing PzSnapshot result"));
        };
        ensure_active(control)?;
        let parent = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        let length = write_chunks(staged.as_file_mut(), &chunks, control)?;
        let archive = SemanticArchive::read(staged.path(), &project_root, control)?;
        if metadata.output_bytes != length
            || metadata.project_id != archive.project_id
            || metadata.snapshot_id != archive.snapshot_id
            || metadata.commit_version != archive.snapshot.commit_version
        {
            return Err(invalid_archive("snapshot metadata differs from archive"));
        }
        ensure_active(control)?;
        staged
            .persist(&output)
            .map_err(|error| DbError::pz(PzFailurePhase::Publish, error.error.to_string()))?;
        metadata.output_path = output;
        Ok(SemanticResult::PzSnapshot(metadata))
    }
}

fn write_chunks(
    output: &mut std::fs::File,
    chunks: &[&TypedBinaryChunkV1],
    control: &InvocationControl,
) -> Result<u64> {
    if chunks.is_empty() {
        return Err(invalid_archive("missing archive binary chunks"));
    }
    let mut length = 0_u64;
    for (index, chunk) in chunks.iter().enumerate() {
        ensure_active(control)?;
        if chunk.type_name != PZ_ARCHIVE_CHUNK_TYPE
            || chunk.metadata_json.is_some()
            || chunk.final_chunk != (index + 1 == chunks.len())
        {
            return Err(invalid_archive("invalid archive chunk type or sequence"));
        }
        output.write_all(&chunk.bytes)?;
        length = length
            .checked_add(chunk.bytes.len() as u64)
            .ok_or_else(|| invalid_archive("archive length overflow"))?;
    }
    output.sync_all()?;
    Ok(length)
}

fn invalid_archive(message: &str) -> DbError {
    DbError::pz(PzFailurePhase::Validate, message)
}
fn ensure_active(control: &InvocationControl) -> Result<()> {
    if control.is_cancelled() || control.is_expired() {
        return Err(DbError::pz(
            PzFailurePhase::Cancellation,
            "archive invocation cancelled or expired",
        ));
    }
    Ok(())
}
