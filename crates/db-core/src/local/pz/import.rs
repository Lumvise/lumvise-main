//! PZ archive import into one local project root.
//!
//! The inverse of `create_semantic_snapshot_controlled`: the archive passes the
//! same full validation `PzArchive::open` performs, then its rows are written
//! through the live storage paths so publication, change hooks, and blob
//! policy stay authoritative.

use super::*;
use crate::local::grafeo::storage_manager::StorageManager;
use crate::{
    ArtifactBlob, ArtifactTextVector, PzImportResult, SemanticArtifact, SemanticMatchEvidence,
    SemanticRelationship, StoredSemanticElementNameVector,
};
use arrow_array::{Array, Float32Array};
use bytes::Bytes;
use lumvise_resource_routing::InvocationControl;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::collections::{HashMap, HashSet};

/// Imports one validated PZ archive into `project_root`.
///
/// Structure replaces the project's published structure. Archive artifacts
/// whose IDs the database lacks are inserted with their blob content, their
/// attachment blobs (canvas images, under their original refs), and their text
/// vector; existing artifact IDs stay authoritative. External references are
/// not imported because their foreign targets live in other projects.
pub fn import_semantic_snapshot_controlled(
    storage: &StorageManager<'_>,
    project_root: &str,
    input_path: impl AsRef<Path>,
    control: &InvocationControl,
) -> Result<PzImportResult> {
    verify_control(control)?;
    let project_root = normalized_project_root(project_root)?;
    let (manifest, tables) = super::archive::read_validated_tables(input_path.as_ref())
        .and_then(|(manifest, tables)| {
            Ok((manifest, DecodedArchive::decode(&tables, &project_root)?))
        })
        .map_err(|error| DbError::pz(crate::PzFailurePhase::Validate, error.to_string()))?;
    verify_control(control)?;

    let semantic = storage.semantic_storage().with_control(control.clone());
    let structure =
        semantic.sync_semantic_structure(&project_root, &tables.elements, &tables.relationships)?;
    semantic.store_element_name_vectors(&project_root, &tables.element_name_vectors)?;

    let archived_ids = tables
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact_id.clone())
        .collect::<HashSet<_>>();
    let existing_ids = semantic
        .artifacts_by_ids(&archived_ids)?
        .into_iter()
        .map(|artifact| artifact.artifact_id)
        .collect::<HashSet<_>>();
    let mut artifacts_imported = 0;
    for artifact in &tables.artifacts {
        if existing_ids.contains(&artifact.artifact_id) {
            continue;
        }
        verify_control(control)?;
        let vector = tables
            .artifact_vectors
            .get(&artifact.artifact_id)
            .map(|(source_text, vector)| (source_text.as_str(), vector));
        let blob = artifact
            .content_ref
            .as_ref()
            .and_then(|content_ref| tables.blobs.get(content_ref));
        match blob {
            Some(blob) => {
                let staged = semantic.artifact_with_content_policy(
                    artifact,
                    &blob.media_type,
                    &blob.content,
                )?;
                semantic.commit_staged_artifact_update(&staged, vector)?;
            }
            None => {
                let mut artifact = artifact.clone();
                artifact.content_ref = None;
                semantic.commit_artifact_update(&artifact, vector)?;
            }
        }
        // Attachments (canvas images) keep their original refs: scenes
        // reference them by content ref.
        let mut attachments = tables
            .blobs
            .values()
            .filter(|attachment| {
                attachment.artifact_id == artifact.artifact_id
                    && artifact.content_ref.as_ref() != Some(&attachment.content_ref)
            })
            .collect::<Vec<_>>();
        attachments.sort_by(|left, right| left.content_ref.cmp(&right.content_ref));
        for attachment in attachments {
            semantic.put_owned_blob(attachment)?;
        }
        artifacts_imported += 1;
    }

    Ok(PzImportResult {
        project_id: manifest.project_id,
        snapshot_id: manifest.snapshot_id,
        canonical_remote: manifest.project.canonical_remote,
        structure,
        artifacts_imported,
        artifacts_kept: tables.artifacts.len() - artifacts_imported,
    })
}

/// Archive rows decoded into the records live writers accept.
struct DecodedArchive {
    elements: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
    artifacts: Vec<SemanticArtifact>,
    blobs: HashMap<String, ArtifactBlob>,
    artifact_vectors: HashMap<String, (String, ArtifactTextVector)>,
    element_name_vectors: Vec<StoredSemanticElementNameVector>,
}

impl DecodedArchive {
    fn decode(tables: &HashMap<String, Vec<u8>>, project_root: &str) -> Result<Self> {
        let mut decoded = Self {
            elements: Vec::new(),
            relationships: Vec::new(),
            artifacts: Vec::new(),
            blobs: HashMap::new(),
            artifact_vectors: HashMap::new(),
            element_name_vectors: Vec::new(),
        };
        for_each_row(tables, "elements.parquet", |row| {
            decoded.elements.push(SemanticElement {
                project_root: project_root.to_owned(),
                semantic_element_id: row.required("semantic_element_id")?,
                semantic_source_id: row.required("semantic_source_id")?,
                path: row.required("path")?,
                element_kind: row.required("element_kind")?,
                name: row.required("name")?,
                parent_element_id: row.text("parent_element_id")?,
                content_fingerprint: row.text("content_fingerprint")?,
                start_line: row.int("start_line")?,
                end_line: row.int("end_line")?,
                lifecycle: row.required("lifecycle")?,
                match_evidence: row
                    .text("match_evidence_json")?
                    .map(|json| serde_json::from_str::<Option<SemanticMatchEvidence>>(&json))
                    .transpose()?
                    .flatten(),
                metadata: row.json("metadata_json")?,
            });
            Ok(())
        })?;
        for_each_row(tables, "relationships.parquet", |row| {
            decoded.relationships.push(SemanticRelationship {
                project_root: project_root.to_owned(),
                source_element_id: row.required("source_element_id")?,
                target_element_id: row.required("target_element_id")?,
                relationship_kind: row.required("relationship_kind")?,
                label: row.required("label")?,
                metadata: row.json("metadata_json")?,
            });
            Ok(())
        })?;
        for_each_row(tables, "artifacts.parquet", |row| {
            decoded.artifacts.push(SemanticArtifact {
                artifact_id: row.required("artifact_id")?,
                semantic_element_id: row.required("semantic_element_id")?,
                artifact_kind: row.required("artifact_kind")?,
                title: row.required("title")?,
                content_ref: row.text("content_ref")?,
                content: None,
                searchable_text: row.text("searchable_text")?,
                content_size_bytes: row.int("content_size_bytes")?.map(|size| size as usize),
                dependencies: Vec::new(),
                metadata: row.json("metadata_json")?,
            });
            Ok(())
        })?;
        for_each_row(tables, "artifact_blobs.parquet", |row| {
            let blob = ArtifactBlob {
                content_ref: row.required("content_ref")?,
                artifact_id: row.required("artifact_id")?,
                media_type: row.required("media_type")?,
                content: row.binary("content")?,
                updated_at: row.required("updated_at")?,
            };
            decoded.blobs.insert(blob.content_ref.clone(), blob);
            Ok(())
        })?;
        for_each_row(tables, "artifact_text_vectors.parquet", |row| {
            decoded.artifact_vectors.insert(
                row.required("artifact_id")?,
                (row.required("source_text")?, row.vector()?),
            );
            Ok(())
        })?;
        for_each_row(tables, "element_name_vectors.parquet", |row| {
            decoded
                .element_name_vectors
                .push(StoredSemanticElementNameVector {
                    semantic_element_id: row.required("semantic_element_id")?,
                    project_root: project_root.to_owned(),
                    source_text: row.required("source_text")?,
                    vector: row.vector()?,
                });
            Ok(())
        })?;
        Ok(decoded)
    }
}

/// Visits every row of one validated table in archive order.
fn for_each_row(
    tables: &HashMap<String, Vec<u8>>,
    name: &str,
    mut visit: impl FnMut(&Row<'_>) -> Result<()>,
) -> Result<()> {
    let bytes = tables
        .get(name)
        .ok_or_else(|| DbError::invalid_value(name, "validated PZ table"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(bytes))
        .map_err(|error| DbError::invalid_value(error.to_string(), "readable parquet"))?
        .with_batch_size(4096)
        .build()
        .map_err(|error| DbError::invalid_value(error.to_string(), "readable parquet batches"))?;
    for batch in reader {
        let batch = batch
            .map_err(|error| DbError::invalid_value(error.to_string(), "readable parquet batch"))?;
        for index in 0..batch.num_rows() {
            visit(&Row {
                batch: &batch,
                index,
            })?;
        }
    }
    Ok(())
}

/// One typed row view; nulls stay `None` instead of collapsing to empty text.
struct Row<'a> {
    batch: &'a RecordBatch,
    index: usize,
}

impl Row<'_> {
    fn column<T: Array + 'static>(&self, name: &str) -> Result<Option<&T>> {
        let array = self
            .batch
            .column_by_name(name)
            .and_then(|array| array.as_any().downcast_ref::<T>())
            .ok_or_else(|| DbError::invalid_value(name, "typed PZ column"))?;
        Ok((!array.is_null(self.index)).then_some(array))
    }

    fn text(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .column::<StringArray>(name)?
            .map(|array| array.value(self.index).to_owned()))
    }

    fn required(&self, name: &str) -> Result<String> {
        self.text(name)?
            .ok_or_else(|| DbError::invalid_value(name, "non-null PZ column"))
    }

    fn int(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .column::<Int64Array>(name)?
            .map(|array| array.value(self.index)))
    }

    fn binary(&self, name: &str) -> Result<Vec<u8>> {
        self.column::<BinaryArray>(name)?
            .map(|array| array.value(self.index).to_vec())
            .ok_or_else(|| DbError::invalid_value(name, "non-null PZ column"))
    }

    fn json(&self, name: &str) -> Result<serde_json::Value> {
        Ok(self
            .text(name)?
            .map(|json| serde_json::from_str(&json))
            .transpose()?
            .unwrap_or(serde_json::Value::Null))
    }

    /// Decodes the shared vector columns of both vector tables.
    fn vector(&self) -> Result<ArtifactTextVector> {
        let values = self
            .column::<ListArray>("vector")?
            .map(|array| array.value(self.index))
            .ok_or_else(|| DbError::invalid_value("vector", "non-null PZ column"))?;
        let values = values
            .as_any()
            .downcast_ref::<Float32Array>()
            .ok_or_else(|| DbError::invalid_value("vector", "float32 PZ vector"))?;
        Ok(ArtifactTextVector {
            engine_id: self.required("engine_id")?,
            model: self.text("model")?,
            dimensions: self
                .int("dimensions")?
                .ok_or_else(|| DbError::invalid_value("dimensions", "non-null PZ column"))?
                as usize,
            vector: values.values().to_vec(),
            normalized: self
                .column::<BooleanArray>("normalized")?
                .is_some_and(|array| array.value(self.index)),
            metadata: self.json("metadata_json")?,
        })
    }
}
