use super::{PzManifest, SemanticArchive, verify_control};
use crate::{
    ArtifactBlob, ArtifactTextVector, DbError, Result, SemanticArtifact, SemanticElement,
    SemanticMatchEvidence, SemanticProjectSnapshot, SemanticRelationship, SemanticRevisionMetadata,
    StoredArtifactTextVector, StoredSemanticElementNameVector,
};
use arrow_array::{
    Array, BinaryArray, BooleanArray, Float32Array, Int64Array, ListArray, RecordBatch, StringArray,
};
use bytes::Bytes;
use lumvise_resource_routing::InvocationControl;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::collections::{BTreeSet, HashMap};

pub(super) fn read(
    manifest: PzManifest,
    tables: &HashMap<String, Vec<u8>>,
    project_root: &str,
    control: &InvocationControl,
) -> Result<SemanticArchive> {
    let decoded = DecodedArchive::decode(tables, project_root, control)?;
    let mut artifact_owners = HashMap::new();
    for artifact in &decoded.artifacts {
        artifact_owners
            .entry(artifact.artifact_id.as_str())
            .or_insert(artifact.semantic_element_id.as_str());
    }
    let mut artifact_text_vectors = decoded
        .artifact_vectors
        .into_iter()
        .filter_map(|(artifact_id, (source_text, vector))| {
            let semantic_element_id = artifact_owners.get(artifact_id.as_str())?;
            Some(StoredArtifactTextVector {
                artifact_id,
                semantic_element_id: (*semantic_element_id).to_owned(),
                source_text,
                vector,
            })
        })
        .collect::<Vec<_>>();
    artifact_text_vectors.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    let mut artifact_blobs = decoded.blobs.into_values().collect::<Vec<_>>();
    artifact_blobs.sort_by(|left, right| left.content_ref.cmp(&right.content_ref));
    let revisions = super::revisions::restore_revision_state(
        project_root,
        manifest.commit_version,
        decoded.revision_metadata,
        decoded.elements,
        decoded.artifacts,
    )?;
    super::revisions::validate_archive_project_identity(&manifest.project_id, &revisions)?;
    let selected = revisions
        .snapshot(&manifest.selection_id, &manifest.published_at)?
        .ok_or_else(|| {
            DbError::invalid_value(&manifest.selection_id, "an archived current selection")
        })?;
    validate_body_blobs(&revisions, &artifact_blobs)?;
    validate_selected_relationships(&selected.connections, &decoded.relationships)?;
    let expected = revisions
        .selection(&manifest.selection_id)
        .expect("validated selection");
    if decoded.selected_records != expected.records.iter().cloned().collect() {
        return Err(DbError::invalid_value(
            "selected",
            "the exact archived selection's record membership",
        ));
    }
    super::artifact_index::validate_associations(
        tables,
        &revisions.selected_associations(&manifest.selection_id),
    )?;
    Ok(SemanticArchive {
        display_name: manifest.project.display_name,
        source_revision: manifest.project.source_revision,
        selection_id: manifest.selection_id,
        revision_state: Some(revisions),
        project_id: manifest.project_id,
        snapshot_id: manifest.snapshot_id,
        canonical_remote: manifest.project.canonical_remote,
        snapshot: SemanticProjectSnapshot {
            commit_version: manifest.commit_version,
            published_at: manifest.published_at,
            project_root: project_root.to_owned(),
            elements: selected.elements,
            relationships: selected
                .connections
                .into_iter()
                .map(|connection| connection.relationship)
                .collect(),
            artifacts: selected.artifacts,
        },
        artifact_blobs,
        artifact_text_vectors,
        element_name_vectors: decoded.element_name_vectors,
    })
}

/// Archive rows decoded into the records live writers accept.
struct DecodedArchive {
    selected_records: BTreeSet<crate::SemanticRecordRef>,
    revision_metadata: Vec<SemanticRevisionMetadata>,
    elements: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
    artifacts: Vec<SemanticArtifact>,
    blobs: HashMap<String, ArtifactBlob>,
    artifact_vectors: HashMap<String, (String, ArtifactTextVector)>,
    element_name_vectors: Vec<StoredSemanticElementNameVector>,
}

impl DecodedArchive {
    fn decode(
        tables: &HashMap<String, Vec<u8>>,
        project_root: &str,
        control: &InvocationControl,
    ) -> Result<Self> {
        let mut decoded = Self {
            selected_records: BTreeSet::new(),
            revision_metadata: vec![],
            elements: Vec::new(),
            relationships: Vec::new(),
            artifacts: Vec::new(),
            blobs: HashMap::new(),
            artifact_vectors: HashMap::new(),
            element_name_vectors: Vec::new(),
        };
        for_each_row(tables, "elements.parquet", control, |row| {
            if row.boolean("selected")? {
                decoded
                    .selected_records
                    .insert(crate::SemanticRecordRef::element(
                        row.required("semantic_element_id")?,
                    ));
            }
            decoded.elements.push(SemanticElement {
                project_root: project_root.to_owned(),
                semantic_element_id: row.required("semantic_element_id")?,
                semantic_source_id: row.required("semantic_source_id")?,
                path: row.required("path")?,
                element_kind: row.required("element_kind")?,
                name: row.required("name")?,
                parent_element_id: row.text("parent_element_id")?,
                revision_dependencies: serde_json::from_value(
                    row.json("revision_dependencies_json")?,
                )?,
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
        for_each_row(tables, "relationships.parquet", control, |row| {
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
        for_each_row(tables, "artifacts.parquet", control, |row| {
            if row.boolean("selected")? {
                decoded
                    .selected_records
                    .insert(crate::SemanticRecordRef::artifact(
                        row.required("artifact_id")?,
                    ));
            }
            decoded.artifacts.push(SemanticArtifact {
                artifact_id: row.required("artifact_id")?,
                semantic_element_id: row.required("semantic_element_id")?,
                artifact_kind: row.required("artifact_kind")?,
                title: row.required("title")?,
                content_ref: row.text("content_ref")?,
                content: row.text("inline_content")?,
                searchable_text: row.text("searchable_text")?,
                content_size_bytes: row.int("content_size_bytes")?.map(|size| size as usize),
                dependencies: serde_json::from_str(&row.required("dependencies_json")?)?,
                metadata: row.json("metadata_json")?,
            });
            Ok(())
        })?;
        for_each_row(tables, "artifact_blobs.parquet", control, |row| {
            let blob = ArtifactBlob {
                content_ref: row.required("content_ref")?,
                artifact_id: row.required("artifact_id")?,
                media_type: row.required("media_type")?,
                content: row.binary("content")?,
                updated_at: row.required("updated_at")?,
            };
            if row.int("byte_size")? != Some(blob.content.len() as i64)
                || row.required("sha256")? != super::sha256_hex(&blob.content)
            {
                return Err(DbError::invalid_value(
                    &blob.content_ref,
                    "archived blob bytes matching their declared length and SHA-256",
                ));
            }
            if decoded
                .blobs
                .insert(blob.content_ref.clone(), blob)
                .is_some()
            {
                return Err(DbError::invalid_value(
                    row.required("content_ref")?,
                    "one archived blob per exact content reference",
                ));
            }
            Ok(())
        })?;
        for_each_row(tables, "artifact_text_vectors.parquet", control, |row| {
            decoded.artifact_vectors.insert(
                row.required("artifact_id")?,
                (row.required("source_text")?, row.vector()?),
            );
            Ok(())
        })?;
        for_each_row(tables, "element_name_vectors.parquet", control, |row| {
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
        for_each_row(tables, "revision_metadata.parquet", control, |row| {
            let value: SemanticRevisionMetadata =
                serde_json::from_str(&row.required("record_json")?)?;
            if value.kind() != row.required("kind")?
                || value.identity()? != row.required("identity")?
            {
                return Err(DbError::invalid_value(
                    row.required("identity")?,
                    "the declared portable revision metadata kind and identity",
                ));
            }
            decoded.revision_metadata.push(value);
            Ok(())
        })?;
        Ok(decoded)
    }
}

fn validate_body_blobs(state: &crate::SemanticRevisionState, blobs: &[ArtifactBlob]) -> Result<()> {
    let available = blobs
        .iter()
        .map(|blob| blob.content_ref.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for stored in &state.records {
        let crate::SemanticRevisionBody::Artifact(artifact) = &stored.body else {
            continue;
        };
        if let Some(reference) = &artifact.content_ref {
            if !available.contains(reference.as_str()) {
                return Err(DbError::invalid_value(
                    reference,
                    "exact archived bytes for every retained artifact content reference",
                ));
            }
        }
    }
    Ok(())
}

fn validate_selected_relationships(
    connections: &[crate::SemanticRevisionConnection],
    relationships: &[SemanticRelationship],
) -> Result<()> {
    let mut expected = connections
        .iter()
        .map(|connection| serde_json::to_string(&connection.relationship))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut actual = relationships
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    expected.sort();
    actual.sort();
    if expected != actual {
        return Err(DbError::invalid_value(
            "relationships.parquet",
            "the exact selected connection multiset",
        ));
    }
    Ok(())
}

/// Visits every row of one validated table in archive order.
fn for_each_row(
    tables: &HashMap<String, Vec<u8>>,
    name: &str,
    control: &InvocationControl,
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
        verify_control(control)?;
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

    fn boolean(&self, name: &str) -> Result<bool> {
        self.column::<BooleanArray>(name)?
            .map(|array| array.value(self.index))
            .ok_or_else(|| DbError::invalid_value(name, "a non-null PZ boolean"))
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
