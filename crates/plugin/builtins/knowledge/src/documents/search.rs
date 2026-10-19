//! Document retrieval reuses the owner's text embeddings and lexical fallback.

use std::collections::BTreeMap;

use lumvise_contracts::{KnowledgeDocument, ListKnowledgeDocumentsRequest};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use crate::search::KnowledgeSearchVectors;

pub(super) fn search(
    input: Value,
    context: &mut PluginContext<'_>,
    vectors: &KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    let query = crate::required_input(&input, "query")?;
    let mut request: ListKnowledgeDocumentsRequest =
        crate::parse(input, "document search filters")?;
    request.query = None;
    let mut documents = super::records::list(context, &request)?;
    let texts = documents
        .iter()
        .map(|document| {
            (
                document.semantic_element_id.clone(),
                searchable_text(document),
            )
        })
        .collect::<Vec<_>>();
    let (mode, scores) = match vectors.rank_texts(&query, &texts, context) {
        Some(scores) => ("vector", scores),
        None => {
            request.query = Some(query.clone());
            documents.retain(|document| super::records::matches(document, &request));
            ("lexical", BTreeMap::new())
        }
    };
    order_documents(&mut documents, &scores, &query);
    let scores = document_scores(&documents, &scores);
    Ok(json!({"mode": mode, "total": documents.len(), "documents": documents, "scores": scores}))
}

fn searchable_text(document: &KnowledgeDocument) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        document.title,
        document.path.as_deref().unwrap_or_default(),
        document.tags.join(" "),
        document.content
    )
}

fn order_documents(
    documents: &mut [KnowledgeDocument],
    scores: &BTreeMap<String, f32>,
    query: &str,
) {
    let query = query.to_lowercase();
    documents.sort_by(|left, right| {
        let literal = |document: &KnowledgeDocument| {
            searchable_text(document).to_lowercase().contains(&query)
        };
        let score = |document: &KnowledgeDocument| {
            scores
                .get(&document.semantic_element_id)
                .copied()
                .unwrap_or_default()
        };
        literal(right)
            .cmp(&literal(left))
            .then_with(|| score(right).total_cmp(&score(left)))
            .then(left.document_id.cmp(&right.document_id))
    });
}

fn document_scores(
    documents: &[KnowledgeDocument],
    scores: &BTreeMap<String, f32>,
) -> BTreeMap<String, f32> {
    documents
        .iter()
        .map(|document| {
            (
                document.document_id.clone(),
                scores
                    .get(&document.semantic_element_id)
                    .copied()
                    .unwrap_or(1.0),
            )
        })
        .collect()
}
