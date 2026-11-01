//! Name search asks the adapter for the best-scoring elements, then re-scores them from compact headers before requesting only winning exact bodies.
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

/// Splits the name from the ID in the one search entry an active element writes.
const SEPARATOR: char = '\u{1f}';

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

/// One entry per active element: its name and ID, ASCII-lowercased like the shared score, split by `SEPARATOR`.
pub(super) fn indexes(element: &SemanticElement) -> Vec<indexes::StorageIndex> {
    if element.lifecycle == "inactive" {
        return vec![];
    }
    vec![indexes::StorageIndex {
        name: "element_search_text".into(),
        value: format!(
            "{}{SEPARATOR}{}",
            search_text(&element.name),
            search_text(&element.semantic_element_id)
        ),
    }]
}

/// PostgreSQL text holds no NUL and the separator must stay unique, so a name or ID loses its NULs and its
/// separators become spaces. The query is cleaned the same way, so a substring of the real text stays a substring.
fn search_text(text: &str) -> String {
    text.chars()
        .filter(|ch| *ch != '\0')
        .map(|ch| {
            if ch == SEPARATOR {
                ' '
            } else {
                ch.to_ascii_lowercase()
            }
        })
        .collect()
}

fn like_pattern(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// A single query: the adapter scores, filters by selection, ranks and limits, and Rust re-scores what it returns.
pub(super) fn queries(input: &str) -> SemanticRevisionResult<StorageResult> {
    let request = request(input)?;
    let query = SemanticElementNameQuery::new(&request.query);
    let normalized = search_text(query.normalized());
    let queries = if request.limit == 0 || normalized.is_empty() {
        vec![]
    } else {
        // Tokens hold only alphanumerics, so their patterns need no escaping.
        let tokens = query
            .tokens()
            .iter()
            .map(|token| format!("%{token}%"))
            .collect::<Vec<_>>();
        // Any row that scores above zero contains the whole query or a token, so these cover every winner.
        let patterns = std::iter::once(like_pattern(&normalized))
            .chain(tokens.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        vec![StorageQuery::Search {
            selection_id: request.selection_id,
            normalized,
            tokens,
            patterns,
            limit: request.limit,
        }]
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
