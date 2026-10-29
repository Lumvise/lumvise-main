//! Snapshot name searches delegate matching to the shared revision policy.
use crate::SemanticElementNameQuery;

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
        let query = SemanticElementNameQuery::new(query);
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
