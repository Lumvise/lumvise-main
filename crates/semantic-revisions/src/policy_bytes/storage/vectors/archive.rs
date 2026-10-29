//! Archive adapters validate available historical owners with the same vector admission predicates.
use super::*;

pub(in crate::policy_bytes::storage) fn import(
    state: &SemanticRevisionState,
    names: &[StoredSemanticElementNameVector],
    artifacts: &[StoredArtifactTextVector],
    loaded: &[StorageRow],
) -> SemanticRevisionResult<Vec<StorageRow>> {
    let mut selected = state.snapshot("working", "")?.ok_or_else(|| {
        SemanticRevisionError::invalid_value("working", "an available archive selection")
    })?;
    selected.elements.clear();
    selected.artifacts.clear();
    for stored in &state.records {
        match &stored.body {
            SemanticRevisionBody::Element(element) => {
                let mut element = element.clone();
                element.lifecycle = "active".into();
                selected.elements.push(element);
            }
            SemanticRevisionBody::Artifact(artifact) => selected.artifacts.push(artifact.clone()),
            SemanticRevisionBody::Pruned => {}
        }
    }
    let mut names = names.to_vec();
    for name in &mut names {
        name.project_root = state.project_root.clone();
    }
    let mut rows = Vec::new();
    for request in [
        VectorWrite::StoreElementNameVectors {
            project_root: state.project_root.clone(),
            vectors: names,
        },
        VectorWrite::StoreArtifactTextVectors {
            project_root: state.project_root.clone(),
            vectors: artifacts.to_vec(),
        },
    ] {
        let (_, groups, _) = validated_groups(&selected, request)?;
        rows.extend(merge::rows(groups, loaded)?);
    }
    Ok(rows)
}

pub(in crate::policy_bytes::storage) fn export(
    rows: &[StorageRow],
    records: &[SemanticStoredRevision],
) -> SemanticRevisionResult<(
    Vec<StoredArtifactTextVector>,
    Vec<StoredSemanticElementNameVector>,
)> {
    let mut artifacts = Vec::new();
    let mut names = Vec::new();
    for row in rows.iter().filter(|row| is_vector(row)) {
        let record = owner(row)?;
        if !records.iter().any(|stored| {
            stored.record == record && !matches!(stored.body, SemanticRevisionBody::Pruned)
        }) {
            continue;
        }
        let body = row.body_json.as_deref().ok_or_else(|| {
            SemanticRevisionError::invalid_value(
                &row.key.identity,
                "available archived vector bytes",
            )
        })?;
        match serde_json::from_str::<VectorPayload>(body)? {
            VectorPayload::Artifact(values) if record.kind == SemanticRecordKind::Artifact => {
                artifacts.extend(values)
            }
            VectorPayload::Element(values) if record.kind == SemanticRecordKind::Element => {
                names.extend(values)
            }
            _ => {
                return Err(SemanticRevisionError::invalid_value(
                    &row.key.identity,
                    "matching archived vector kind",
                ));
            }
        }
    }
    Ok((artifacts, names))
}
