//! Name search reads compact selected headers before requesting only winning exact bodies.
use super::*;
use read::{ReadIntent, StorageQuery};
use std::collections::BTreeSet;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum SearchOperation {
    SearchElements,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NameSearch {
    operation: SearchOperation,
    selection_id: String,
    query: String,
    limit: usize,
}

fn request(input: &str) -> SemanticRevisionResult<NameSearch> {
    let request: NameSearch = serde_json::from_str(input)?;
    if request.query.trim().is_empty() {
        return Err(SemanticRevisionError::invalid_value(
            &request.query,
            "non-empty semantic element search query",
        ));
    }
    Ok(request)
}

pub(super) fn prepare(input: &str) -> SemanticRevisionResult<StorageResult> {
    encoded("intent", &request(input)?)
}

pub(super) fn indexes(element: &SemanticElement) -> Vec<indexes::StorageIndex> {
    if element.lifecycle == "inactive" {
        return vec![];
    }
    let mut entries = vec![indexes::StorageIndex {
        name: "element_search_active".into(),
        value: String::new(),
    }];
    let grams = [
        element.name.to_ascii_lowercase(),
        element.semantic_element_id.to_ascii_lowercase(),
    ]
    .iter()
    .flat_map(|text| text.as_bytes().windows(3).map(gram))
    .collect::<BTreeSet<_>>();
    entries.extend(grams.into_iter().map(|value| indexes::StorageIndex {
        name: "element_search_gram".into(),
        value,
    }));
    entries
}

fn gram(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn queries(input: &str) -> SemanticRevisionResult<StorageResult> {
    let request = request(input)?;
    let query = SemanticElementNameQuery::new(&request.query);
    let terms = std::iter::once(query.normalized())
        .chain(query.tokens().iter().map(String::as_str))
        .collect::<Vec<_>>();
    let indexes = if terms.iter().any(|term| term.len() < 3) {
        BTreeSet::from([("element_search_active", String::new())])
    } else {
        terms
            .into_iter()
            .map(|term| ("element_search_gram", gram(&term.as_bytes()[..3])))
            .collect()
    };
    let queries = if request.limit == 0 {
        vec![]
    } else {
        indexes
            .into_iter()
            .map(|(name, value)| StorageQuery::Index {
                name: name.into(),
                value,
                selection_id: Some(request.selection_id.clone()),
            })
            .collect()
    };
    Ok(StorageResult::Read {
        queries,
        state_json: None,
    })
}

pub(super) fn select(input: &str, rows: Vec<StorageRow>) -> SemanticRevisionResult<StorageResult> {
    let request = request(input)?;
    let query = SemanticElementNameQuery::new(&request.query);
    let mut ranked = Vec::new();
    for row in rows {
        let metadata: SemanticRevisionMetadata = serde_json::from_str(&row.metadata_json)?;
        let SemanticRevisionMetadata::Record(record) = metadata else {
            return Err(SemanticRevisionError::invalid_value(
                &row.key.kind,
                "an indexed element header",
            ));
        };
        if row.key.kind != "record" || row.key.identity != serde_json::to_string(&record.record)? {
            return Err(SemanticRevisionError::invalid_value(
                &row.key.identity,
                "the exact candidate record identity",
            ));
        }
        let header = headers::decode(&row, &record)?;
        let score = query.score(
            &header.name.to_ascii_lowercase(),
            &header.semantic_element_id.to_ascii_lowercase(),
        );
        if score > 0 && header.lifecycle != "inactive" {
            ranked.push((std::cmp::Reverse(score), header.semantic_element_id));
        }
    }
    ranked.sort_unstable();
    ranked.dedup();
    let records = ranked
        .into_iter()
        .take(request.limit)
        .map(|(_, id)| SemanticRecordRef::element(id))
        .collect();
    encoded(
        "intent",
        &ReadIntent::Selected {
            selection_id: request.selection_id,
            query: read::SelectedQuery::Records { records },
        },
    )
}
