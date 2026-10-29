//! Derived vectors use separate immutable bodies; exact owner edits and collection retire them atomically.
use super::*;
pub(super) mod archive;
mod merge;
use lumvise_contracts::{StoredArtifactTextVector, StoredSemanticElementNameVector};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum VectorWrite {
    StoreElementNameVectors {
        project_root: String,
        vectors: Vec<StoredSemanticElementNameVector>,
    },
    StoreArtifactTextVectors {
        project_root: String,
        vectors: Vec<StoredArtifactTextVector>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "kind",
    content = "vectors",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum VectorPayload {
    Element(Vec<StoredSemanticElementNameVector>),
    Artifact(Vec<StoredArtifactTextVector>),
}

impl VectorWrite {
    pub(super) fn records(&self) -> Vec<SemanticRecordRef> {
        match self {
            Self::StoreElementNameVectors { vectors, .. } => vectors
                .iter()
                .map(|row| SemanticRecordRef::element(&row.semantic_element_id))
                .collect(),
            Self::StoreArtifactTextVectors { vectors, .. } => vectors
                .iter()
                .map(|row| SemanticRecordRef::artifact(&row.artifact_id))
                .collect(),
        }
    }
}

pub(super) fn key(record: &SemanticRecordRef) -> SemanticRevisionResult<StorageRowKey> {
    Ok(StorageRowKey {
        kind: match record.kind {
            SemanticRecordKind::Element => "element_vector",
            SemanticRecordKind::Artifact => "artifact_vector",
        }
        .into(),
        identity: serde_json::to_string(record)?,
    })
}

pub(super) fn is_vector(row: &StorageRow) -> bool {
    matches!(row.key.kind.as_str(), "element_vector" | "artifact_vector")
}

pub(super) fn engine_key(engine: &str, model: Option<&str>) -> SemanticRevisionResult<String> {
    // Native storage represents both absent and empty model names with an empty key.
    Ok(serde_json::to_string(&(engine, model.unwrap_or("")))?)
}

pub(super) fn owner(row: &StorageRow) -> SemanticRevisionResult<SemanticRecordRef> {
    let record: SemanticRecordRef = serde_json::from_str(&row.metadata_json)?;
    if key(&record)? != row.key {
        return Err(SemanticRevisionError::invalid_value(
            &row.key.identity,
            "exact vector owner identity",
        ));
    }
    Ok(record)
}

pub(super) fn write(
    state: &SemanticRevisionState,
    input: &str,
    loaded: &[StorageRow],
    next: i64,
) -> SemanticRevisionResult<StorageResult> {
    let request: VectorWrite = serde_json::from_str(input)?;
    let selected = state.snapshot("working", "")?.ok_or_else(|| {
        SemanticRevisionError::invalid_value(
            "working",
            "an available current selection for vectors",
        )
    })?;
    let (root, groups, count) = validated_groups(&selected, request)?;
    crate::revision_index::ensure_root(&root, &state.project_root)?;
    let rows = merge::rows(groups, loaded)?
        .into_iter()
        .filter(|row| !loaded.contains(row))
        .collect::<Vec<_>>();
    Ok(StorageResult::Delta {
        expected_commit: state.commit_version.to_string(),
        commit_version: if rows.is_empty() {
            state.commit_version
        } else {
            next
        }
        .to_string(),
        rows,
        removed: vec![],
        outcome_json: serde_json::to_string(&serde_json::json!({"count":count}))?,
    })
}

fn validated_groups(
    selected: &SemanticRevisionSelectionSnapshot,
    request: VectorWrite,
) -> SemanticRevisionResult<(String, BTreeMap<SemanticRecordRef, VectorPayload>, usize)> {
    let mut groups = BTreeMap::new();
    match request {
        VectorWrite::StoreElementNameVectors {
            project_root,
            vectors,
        } => {
            let count = vectors.len();
            for vector in vectors {
                validate_element(selected, &project_root, &vector)?;
                let group = groups
                    .entry(SemanticRecordRef::element(&vector.semantic_element_id))
                    .or_insert_with(|| VectorPayload::Element(vec![]));
                if let VectorPayload::Element(values) = group {
                    values.push(vector);
                }
            }
            Ok((project_root, groups, count))
        }
        VectorWrite::StoreArtifactTextVectors {
            project_root,
            vectors,
        } => {
            let count = vectors.len();
            for vector in vectors {
                validate_artifact(selected, &vector)?;
                let group = groups
                    .entry(SemanticRecordRef::artifact(&vector.artifact_id))
                    .or_insert_with(|| VectorPayload::Artifact(vec![]));
                if let VectorPayload::Artifact(values) = group {
                    values.push(vector);
                }
            }
            Ok((project_root, groups, count))
        }
    }
}

fn validate_element(
    selected: &SemanticRevisionSelectionSnapshot,
    root: &str,
    vector: &StoredSemanticElementNameVector,
) -> SemanticRevisionResult<()> {
    let element = selected.elements.iter().find(|element| {
        element.semantic_element_id == vector.semantic_element_id && element.lifecycle != "inactive"
    });
    if vector.project_root != root
        || element.is_none_or(|element| {
            element.project_root != root
                || element.name.trim().is_empty()
                || element.name.trim() != vector.source_text
        })
    {
        return Err(SemanticRevisionError::invalid_value(
            &vector.semantic_element_id,
            "vector matching an active current element, project root and trimmed name",
        ));
    }
    validate_vector_payload(&vector.semantic_element_id, &vector.vector)
}

fn validate_artifact(
    selected: &SemanticRevisionSelectionSnapshot,
    vector: &StoredArtifactTextVector,
) -> SemanticRevisionResult<()> {
    let artifact = selected
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == vector.artifact_id);
    let valid = artifact.is_some_and(|artifact| {
        let text = artifact
            .searchable_text
            .as_deref()
            .or(artifact.content.as_deref())
            .map(str::trim);
        artifact.semantic_element_id == vector.semantic_element_id
            && text.is_some_and(|text| !text.is_empty() && text == vector.source_text)
    });
    if !valid {
        return Err(SemanticRevisionError::invalid_value(
            &vector.artifact_id,
            "vector matching a current artifact, canonical owner and nonempty searchable text",
        ));
    }
    validate_vector_payload(&vector.artifact_id, &vector.vector)
}

fn encode(
    record: &SemanticRecordRef,
    payload: VectorPayload,
) -> SemanticRevisionResult<StorageRow> {
    let vectors = match &payload {
        VectorPayload::Element(rows) => rows.iter().map(|row| &row.vector).collect::<Vec<_>>(),
        VectorPayload::Artifact(rows) => rows.iter().map(|row| &row.vector).collect::<Vec<_>>(),
    };
    let key = key(record)?;
    let indexes = vectors
        .into_iter()
        .map(|vector| {
            Ok(super::indexes::StorageIndex {
                name: key.kind.clone(),
                value: engine_key(&vector.engine_id, vector.model.as_deref())?,
            })
        })
        .collect::<SemanticRevisionResult<_>>()?;
    Ok(StorageRow {
        key,
        metadata_json: serde_json::to_string(record)?,
        body_json: Some(serde_json::to_string(&payload)?),
        indexes,
    })
}

pub(super) fn read(
    state: &SemanticRevisionState,
    input: &str,
    rows: &[StorageRow],
) -> SemanticRevisionResult<StorageResult> {
    let super::read::ReadIntent::Vectors {
        selection_id,
        artifact,
        engine_id,
        model,
    } = serde_json::from_str(input)?
    else {
        unreachable!()
    };
    let model = model.as_deref().unwrap_or("");
    let selected = state.selection(&selection_id).ok_or_else(|| {
        SemanticRevisionError::invalid_value(&selection_id, "an available vector selection")
    })?;
    let mut names = Vec::new();
    let mut artifacts = Vec::new();
    for row in rows.iter().filter(|row| is_vector(row)) {
        let record = owner(row)?;
        if !selected.records.contains(&record) {
            continue;
        }
        let body = row.body_json.as_deref().ok_or_else(|| {
            SemanticRevisionError::invalid_value(&record.record_id, "available exact vector body")
        })?;
        match serde_json::from_str::<VectorPayload>(body)? {
            VectorPayload::Artifact(values)
                if artifact && record.kind == SemanticRecordKind::Artifact =>
            {
                for value in values {
                    if value.artifact_id != record.record_id {
                        return Err(SemanticRevisionError::invalid_value(
                            value.artifact_id,
                            "vector matching stored owner",
                        ));
                    }
                    validate_vector_payload(&value.artifact_id, &value.vector)?;
                    if value.vector.engine_id == engine_id
                        && value.vector.model.as_deref().unwrap_or("") == model
                    {
                        artifacts.push(value);
                    }
                }
            }
            VectorPayload::Element(values)
                if !artifact && record.kind == SemanticRecordKind::Element =>
            {
                for mut value in values {
                    if value.semantic_element_id != record.record_id {
                        return Err(SemanticRevisionError::invalid_value(
                            value.semantic_element_id,
                            "vector matching stored owner",
                        ));
                    }
                    validate_vector_payload(&value.semantic_element_id, &value.vector)?;
                    value.project_root = state.project_root.clone();
                    if value.vector.engine_id == engine_id
                        && value.vector.model.as_deref().unwrap_or("") == model
                    {
                        names.push(value);
                    }
                }
            }
            _ => {
                return Err(SemanticRevisionError::invalid_value(
                    &record.record_id,
                    "vector payload matching its requested kind",
                ));
            }
        }
    }
    if artifact {
        encoded("artifact_vectors", &artifacts)
    } else {
        encoded("element_vectors", &names)
    }
}
