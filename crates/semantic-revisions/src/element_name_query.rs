//! Shared name matching policy. Adapters own candidate indexes and hydrate only ranked results.

pub struct SemanticElementNameQuery {
    normalized: String,
    tokens: Vec<String>,
}

impl SemanticElementNameQuery {
    /// Builds the existing ASCII-normalized name query. Example: `Self::new("parse-file")`.
    pub fn new(query: &str) -> Self {
        let normalized = query.trim().to_ascii_lowercase();
        let tokens = normalized
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect();
        Self { normalized, tokens }
    }

    /// Returns the complete normalized query. Example: `query.normalized().is_empty()`.
    pub fn normalized(&self) -> &str {
        &self.normalized
    }

    /// Returns name-match tokens. Example: `query.tokens().iter()`.
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    /// Scores already ASCII-normalized names and IDs. Example: `query.score("parse", "file:parse")`.
    pub fn score(&self, normalized_name: &str, normalized_id: &str) -> u8 {
        if normalized_name == self.normalized {
            return 4;
        }
        if normalized_name.contains(&self.normalized) {
            return 3;
        }
        if self
            .tokens
            .iter()
            .any(|token| normalized_name.contains(token))
        {
            return 2;
        }
        u8::from(normalized_id.contains(&self.normalized))
    }
}
