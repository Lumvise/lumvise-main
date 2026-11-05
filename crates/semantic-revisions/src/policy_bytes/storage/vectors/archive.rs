//! Archive adapters validate available historical owners with the same vector admission predicates.
use super::*;

pub(in crate::policy_bytes::storage) fn canonical(
    row: &StorageRow,
    root: &str,
) -> SemanticRevisionResult<StorageRow> {
    let record = owner(row)?;
    let body = row.body_json.as_deref().ok_or_else(|| {
        SemanticRevisionError::invalid_value(&row.key.identity, "available vector bytes")
    })?;
    let mut payload: VectorPayload = serde_json::from_str(body)?;
    match &mut payload {
        VectorPayload::Element(values) if record.kind == SemanticRecordKind::Element => {
            for value in values {
                if value.semantic_element_id != record.record_id {
                    return Err(SemanticRevisionError::invalid_value(
                        &value.semantic_element_id,
                        "the exact vector row owner",
                    ));
                }
                validate_vector_payload(&record.record_id, &value.vector)?;
                value.project_root = root.into();
            }
        }
        VectorPayload::Artifact(values) if record.kind == SemanticRecordKind::Artifact => {
            for value in values {
                if value.artifact_id != record.record_id {
                    return Err(SemanticRevisionError::invalid_value(
                        &value.artifact_id,
                        "the exact vector row owner",
                    ));
                }
                validate_vector_payload(&record.record_id, &value.vector)?;
            }
        }
        _ => {
            return Err(SemanticRevisionError::invalid_value(
                &row.key.identity,
                "matching archived vector kind",
            ));
        }
    }
    encode(&record, payload)
}

pub(in crate::policy_bytes::storage) fn validate_row(
    row: &StorageRow,
    stored: SemanticStoredRevision,
    root: &str,
) -> SemanticRevisionResult<()> {
    let body = row.body_json.as_deref().ok_or_else(|| {
        SemanticRevisionError::invalid_value(&row.key.identity, "available vector bytes")
    })?;
    let (names, artifacts) = match serde_json::from_str::<VectorPayload>(body)? {
        VectorPayload::Element(values) => (values, vec![]),
        VectorPayload::Artifact(values) => (vec![], values),
    };
    let state = SemanticRevisionState {
        project_root: root.into(),
        records: vec![stored],
        selections: vec![SemanticRevisionSelectionState {
            selection: SemanticRevisionSelection {
                project_root: root.into(),
                selection_id: "working".into(),
                source_context: "local".into(),
                mode: SemanticSelectionMode::Pinned,
                generation: 1,
                source_anchor: None,
            },
            records: vec![],
            connection_ids: vec![],
        }],
        ..Default::default()
    };
    import(&state, &names, &artifacts, &[]).map(|_| ())
}

pub(in crate::policy_bytes::storage) fn merge_row(
    row: &StorageRow,
    previous: Option<&StorageRow>,
) -> SemanticRevisionResult<StorageRow> {
    let owner = owner(row)?;
    let body = row.body_json.as_deref().ok_or_else(|| {
        SemanticRevisionError::invalid_value(&row.key.identity, "available vector bytes")
    })?;
    let groups = BTreeMap::from([(owner, serde_json::from_str::<VectorPayload>(body)?)]);
    merge::rows(
        groups,
        previous.map(std::slice::from_ref).unwrap_or_default(),
    )
    .map(|mut rows| rows.remove(0))
}

pub(in crate::policy_bytes::storage) fn import(
    state: &SemanticRevisionState,
    names: &[StoredSemanticElementNameVector],
    artifacts: &[StoredArtifactTextVector],
    loaded: &[StorageRow],
) -> SemanticRevisionResult<Vec<StorageRow>> {
    state.selection("working").ok_or_else(|| {
        SemanticRevisionError::invalid_value("working", "an existing archive selection")
    })?;
    let mut eligible_elements = Vec::new();
    let mut eligible_artifacts = Vec::new();
    for stored in &state.records {
        match &stored.body {
            SemanticRevisionBody::Element(element) => {
                let mut element = element.clone();
                element.lifecycle = "active".into();
                eligible_elements.push(element);
            }
            SemanticRevisionBody::Artifact(artifact) => eligible_artifacts.push(artifact.clone()),
            SemanticRevisionBody::Pruned | SemanticRevisionBody::Unavailable(_) => {}
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
        let (_, groups, _) = validated_groups(&eligible_elements, &eligible_artifacts, request)?;
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
        if !records
            .iter()
            .any(|stored| stored.record == record && stored.body.is_available())
        {
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
