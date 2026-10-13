//! Shared element-name matching; adapters may index candidates before scoring.
pub(crate) struct ElementNameQuery {
    pub(crate) normalized: String,
    pub(crate) tokens: Vec<String>,
}

impl ElementNameQuery {
    pub(crate) fn new(query: &str) -> Self {
        let normalized = query.trim().to_ascii_lowercase();
        let tokens = normalized
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect();
        Self { normalized, tokens }
    }

    pub(crate) fn score(&self, name: &str, id: &str) -> u8 {
        if name == self.normalized {
            return 4;
        }
        if name.contains(&self.normalized) {
            return 3;
        }
        if self.tokens.iter().any(|token| name.contains(token)) {
            return 2;
        }
        u8::from(id.contains(&self.normalized))
    }
}

impl crate::SemanticProjectSnapshot {
    /// Finds active element candidates with the same ranking as local persistence.
    /// Example: `snapshot.search_element_candidates("parser", 20)`.
    pub fn search_element_candidates(
        &self,
        query: &str,
        limit: usize,
    ) -> crate::Result<Vec<crate::SemanticElement>> {
        if query.trim().is_empty() {
            return Err(crate::DbError::invalid_value(
                query,
                "non-empty semantic element search query",
            ));
        }
        let query = ElementNameQuery::new(query);
        let mut ranked = self
            .elements
            .iter()
            .filter(|element| element.lifecycle != "inactive")
            .filter_map(|element| {
                let score = query.score(
                    &element.name.to_ascii_lowercase(),
                    &element.semantic_element_id.to_ascii_lowercase(),
                );
                (score > 0).then_some((
                    std::cmp::Reverse(score),
                    &element.semantic_element_id,
                    element,
                ))
            })
            .collect::<Vec<_>>();
        ranked.sort_unstable_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        Ok(ranked
            .into_iter()
            .take(limit)
            .map(|(_, _, element)| element.clone())
            .collect())
    }
}
