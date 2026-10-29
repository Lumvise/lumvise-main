//! Body-free display fields let manuscript discovery avoid reading converted PDF payloads.
use super::*;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ElementHeader {
    pub semantic_element_id: String,
    pub name: String,
    pub lifecycle: String,
    path: String,
    parent_element_id: Option<String>,
}

pub(super) fn encode(element: &SemanticElement) -> SemanticRevisionResult<String> {
    Ok(serde_json::to_string(&ElementHeader {
        semantic_element_id: element.semantic_element_id.clone(),
        name: element.name.clone(),
        lifecycle: element.lifecycle.clone(),
        path: element.path.clone(),
        parent_element_id: element.parent_element_id.clone(),
    })?)
}

pub(super) fn project(rows: Vec<StorageRow>) -> SemanticRevisionResult<StorageResult> {
    let mut elements = Vec::new();
    let mut selection = None;
    for row in rows {
        let metadata: SemanticRevisionMetadata = serde_json::from_str(&row.metadata_json)?;
        if row.key.kind != metadata.kind() || row.key.identity != metadata.identity()? {
            return Err(SemanticRevisionError::invalid_value(
                row.key.identity,
                "matching indexed header identity",
            ));
        }
        match metadata {
            SemanticRevisionMetadata::Record(record) => elements.push(decode(&row, &record)?),
            SemanticRevisionMetadata::Selection(head) if selection.is_none() => {
                selection = Some(head)
            }
            _ => {
                return Err(SemanticRevisionError::invalid_value(
                    row.key.kind,
                    "element headers and one selected head",
                ));
            }
        }
    }
    encoded(
        "element_headers",
        &serde_json::json!({"elements":elements,"selection":selection}),
    )
}

pub(super) fn decode(
    row: &StorageRow,
    record: &SemanticRevisionRecordHeader,
) -> SemanticRevisionResult<ElementHeader> {
    let value = row
        .indexes
        .iter()
        .find(|index| index.name == "element_header")
        .ok_or_else(|| {
            SemanticRevisionError::invalid_value(
                &row.key.identity,
                "indexed element display fields",
            )
        })?;
    let header: ElementHeader = serde_json::from_str(&value.value)?;
    if record.record != SemanticRecordRef::element(&header.semantic_element_id)
        || record
            .element_scope
            .as_ref()
            .is_none_or(|scope| scope.path != header.path)
        || record.pruned_at_ms.is_some()
    {
        return Err(SemanticRevisionError::invalid_value(
            &row.key.identity,
            "available matching element header",
        ));
    }
    Ok(header)
}
