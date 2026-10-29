//! Upsert one engine/model without dropping other derived representations of the same exact body.
use super::*;

pub(super) fn rows(
    groups: BTreeMap<SemanticRecordRef, VectorPayload>,
    loaded: &[StorageRow],
) -> SemanticRevisionResult<Vec<StorageRow>> {
    groups
        .into_iter()
        .map(|(record, payload)| {
            let key = key(&record)?;
            let previous = loaded.iter().find(|row| row.key == key);
            encode(&record, combine(previous, payload)?)
        })
        .collect()
}

fn combine(
    previous: Option<&StorageRow>,
    incoming: VectorPayload,
) -> SemanticRevisionResult<VectorPayload> {
    let Some(previous) = previous else {
        return Ok(incoming);
    };
    owner(previous)?;
    let body = previous.body_json.as_deref().ok_or_else(|| {
        SemanticRevisionError::invalid_value(&previous.key.identity, "available vector bytes")
    })?;
    match (serde_json::from_str::<VectorPayload>(body)?, incoming) {
        (VectorPayload::Element(old), VectorPayload::Element(new)) => {
            Ok(VectorPayload::Element(by_model(old, new, |row| {
                &row.vector
            })?))
        }
        (VectorPayload::Artifact(old), VectorPayload::Artifact(new)) => {
            Ok(VectorPayload::Artifact(by_model(old, new, |row| {
                &row.vector
            })?))
        }
        _ => Err(SemanticRevisionError::invalid_value(
            &previous.key.identity,
            "matching vector payload kinds",
        )),
    }
}

fn by_model<T>(
    old: Vec<T>,
    new: Vec<T>,
    vector: impl Fn(&T) -> &lumvise_contracts::ArtifactTextVector,
) -> SemanticRevisionResult<Vec<T>> {
    let mut indexed = BTreeMap::new();
    for row in old.into_iter().chain(new) {
        let value = vector(&row);
        indexed.insert(engine_key(&value.engine_id, value.model.as_deref())?, row);
    }
    Ok(indexed.into_values().collect())
}
