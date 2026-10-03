//! Owns the single PZ codec: ZIP64, Parquet tables, validation and atomic output.
//! Other persistence engines exchange owned SemanticArchive records; no storage
//! handles or archive-format internals cross the crate boundary.

use crate::{DbError, PzSnapshotResult, Result, SemanticElement, SemanticProjectSnapshot};
use arrow_array::types::Float32Type;
use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Int64Array, ListArray, RecordBatch, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

mod decode;
mod format;
mod tables;
mod validation;
mod write;

pub(crate) use format::PZ_REQUIRED_ENTRIES;
use format::*;
use tables::*;
pub(crate) use validation::PzArchive;
use write::*;
pub(crate) use write::{normalized_project_root, verify_control};

/// Owned project contents exchanged through the unchanged PZ snapshot format.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticArchive {
    pub project_id: String,
    pub snapshot_id: String,
    pub canonical_remote: Option<String>,
    pub snapshot: SemanticProjectSnapshot,
    pub artifact_blobs: Vec<crate::ArtifactBlob>,
    pub artifact_text_vectors: Vec<crate::StoredArtifactTextVector>,
    pub element_name_vectors: Vec<crate::StoredSemanticElementNameVector>,
}

impl SemanticArchive {
    /// Fully validates an archive and remaps its owned records to a project root.
    /// Foreign references remain archive metadata because their targets belong
    /// to other projects and the archive cannot reconstruct their labels.
    /// Example: `SemanticArchive::read("project.pz", "/repo", &control)?`.
    pub fn read(
        input_path: impl AsRef<Path>,
        project_root: &str,
        control: &lumvise_resource_routing::InvocationControl,
    ) -> Result<Self> {
        verify_control(control)?;
        let project_root = normalized_project_root(project_root)?;
        let decoded = validation::read_validated_tables(input_path.as_ref())
            .and_then(|(manifest, tables)| decode::read(manifest, &tables, &project_root, control))
            .map_err(|error| DbError::pz(crate::PzFailurePhase::Validate, error.to_string()));
        verify_control(control)?;
        decoded
    }

    /// Publishes a validated ZIP64 archive by atomic replacement.
    /// Example: `archive.write("project.pz", &control)?`.
    pub fn write(
        &self,
        output_path: impl AsRef<Path>,
        control: &lumvise_resource_routing::InvocationControl,
    ) -> Result<crate::PzSnapshotResult> {
        write::publish(self, output_path.as_ref(), control)
    }
}

#[cfg(test)]
mod tests;
