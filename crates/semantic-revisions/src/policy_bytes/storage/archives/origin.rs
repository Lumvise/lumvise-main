//! Archive origin hints are informational; only selection anchors assert exact current source.
use super::*;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Origin {
    pub display_name: Option<String>,
    pub source_revision: Option<String>,
    pub canonical_remote: Option<String>,
}

pub(super) fn is_origin(row: &StorageRow) -> bool {
    row.key.kind == "archive_origin"
}

pub(super) fn row(archive: &ArchivePayload) -> SemanticRevisionResult<StorageRow> {
    let value = Origin {
        display_name: archive.display_name.clone(),
        source_revision: archive.source_revision.clone(),
        canonical_remote: archive.canonical_remote.clone(),
    };
    Ok(StorageRow {
        key: StorageRowKey {
            kind: "archive_origin".into(),
            identity: String::new(),
        },
        metadata_json: serde_json::to_string(&value)?,
        body_json: None,
        indexes: vec![],
    })
}

pub(super) fn read(rows: &[StorageRow]) -> SemanticRevisionResult<Origin> {
    let Some(row) = rows.iter().find(|row| is_origin(row)) else {
        return Ok(Origin::default());
    };
    if !row.key.identity.is_empty() || row.body_json.is_some() {
        return Err(SemanticRevisionError::invalid_value(
            &row.key.identity,
            "body-free singleton archive origin",
        ));
    }
    Ok(serde_json::from_str(&row.metadata_json)?)
}
