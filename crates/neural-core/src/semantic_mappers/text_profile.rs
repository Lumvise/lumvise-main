use std::collections::{HashMap, HashSet};

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "that", "the", "to", "with",
];

#[derive(Debug, Clone)]
pub struct TextProfile {
    pub normalized: String,
    pub terms: Vec<String>,
    pub unique_terms: HashSet<String>,
    pub term_counts: HashMap<String, u32>,
    pub bigrams: HashSet<String>,
    pub trigrams: HashSet<String>,
}

impl TextProfile {
    /// Builds normalized lexical features for semantic ranking.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let profile = TextProfile::new("semantic mapper policy");
    /// ```
    pub fn new(text: &str) -> Self {
        let normalized = normalize_text(text);
        let terms = terms(&normalized);
        let unique_terms = terms.iter().cloned().collect::<HashSet<_>>();
        let term_counts = term_counts(&terms);
        Self {
            normalized,
            bigrams: ngrams(&terms, 2),
            trigrams: ngrams(&terms, 3),
            terms,
            unique_terms,
            term_counts,
        }
    }
}

/// Normalizes a model-spec alias into one comparable token.
///
/// # Example
///
/// ```ignore
/// let alias = normalize_alias("Governance Policy");
/// ```
pub fn normalize_alias(value: &str) -> String {
    normalize_text(value)
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Scores a candidate text against a precomputed source profile.
///
/// # Example
///
/// ```ignore
/// let score = lexical_score(&query, "candidate content");
/// ```
pub fn lexical_score(query: &TextProfile, text: &str) -> u32 {
    if query.terms.is_empty() {
        return 0;
    }
    let candidate = TextProfile::new(text);
    let mut score = exact_phrase_score(query, &candidate);
    score += term_match_score(query, &candidate);
    score += ngram_score(query, &candidate);
    score + coverage_score(query, &candidate)
}

fn term_counts(terms: &[String]) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    terms.iter().for_each(|term| {
        *counts.entry(term.clone()).or_insert(0) += 1;
    });
    counts
}

fn exact_phrase_score(query: &TextProfile, candidate: &TextProfile) -> u32 {
    if !query.normalized.is_empty() && candidate.normalized.contains(&query.normalized) {
        return 5_000;
    }
    0
}

fn term_match_score(query: &TextProfile, candidate: &TextProfile) -> u32 {
    query
        .unique_terms
        .iter()
        .map(|term| single_term_score(term, candidate))
        .sum()
}

fn single_term_score(term: &str, candidate: &TextProfile) -> u32 {
    if let Some(count) = candidate.term_counts.get(term) {
        return 1_000 + count.saturating_sub(1).min(3) * 100;
    }
    if candidate
        .unique_terms
        .iter()
        .any(|candidate_term| stem_matches(term, candidate_term))
    {
        return 550;
    }
    if term.len() >= 5
        && candidate
            .unique_terms
            .iter()
            .any(|candidate_term| edit_distance_within_one(term, candidate_term))
    {
        return 250;
    }
    0
}

fn ngram_score(query: &TextProfile, candidate: &TextProfile) -> u32 {
    let bigrams = matching_ngrams(&query.bigrams, &candidate.bigrams) * 1_500;
    let trigrams = matching_ngrams(&query.trigrams, &candidate.trigrams) * 2_500;
    bigrams + trigrams
}

fn matching_ngrams(left: &HashSet<String>, right: &HashSet<String>) -> u32 {
    left.iter().filter(|ngram| right.contains(*ngram)).count() as u32
}

fn coverage_score(query: &TextProfile, candidate: &TextProfile) -> u32 {
    let matched = query
        .unique_terms
        .iter()
        .filter(|term| term_matches_candidate(term, candidate))
        .count() as u32;
    if matched == 0 {
        return 0;
    }
    let full_match = matched as usize == query.unique_terms.len();
    matched * 100 + u32::from(full_match) * 750
}

fn term_matches_candidate(term: &str, candidate: &TextProfile) -> bool {
    candidate.unique_terms.contains(term)
        || candidate
            .unique_terms
            .iter()
            .any(|candidate_term| stem_matches(term, candidate_term))
        || (term.len() >= 5
            && candidate
                .unique_terms
                .iter()
                .any(|candidate_term| edit_distance_within_one(term, candidate_term)))
}

fn normalize_text(text: &str) -> String {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn terms(text: &str) -> Vec<String> {
    text.split_ascii_whitespace()
        .filter(|term| term.len() >= 2)
        .filter(|term| !is_stop_word(term))
        .map(str::to_lowercase)
        .collect()
}

fn ngrams(terms: &[String], width: usize) -> HashSet<String> {
    terms
        .windows(width)
        .map(|window| window.join(" "))
        .collect::<HashSet<_>>()
}

fn stem(term: &str) -> String {
    let mut value = term.to_string();
    for suffix in ["ing", "ers", "er", "ed", "es", "s"] {
        if value.len() > suffix.len() + 3 && value.ends_with(suffix) {
            value.truncate(value.len() - suffix.len());
            break;
        }
    }
    value
}

fn stem_matches(left: &str, right: &str) -> bool {
    let left = stem(left);
    let right = stem(right);
    left == right
        || (left.len() >= 4 && right.starts_with(&left))
        || (right.len() >= 4 && left.starts_with(&right))
}

fn edit_distance_within_one(left: &str, right: &str) -> bool {
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    if left_chars.len().abs_diff(right_chars.len()) > 1 {
        return false;
    }
    one_edit_or_less(&left_chars, &right_chars)
}

fn one_edit_or_less(left: &[char], right: &[char]) -> bool {
    let mut mismatches = 0;
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        if left[left_index] == right[right_index] {
            left_index += 1;
            right_index += 1;
            continue;
        }
        mismatches += 1;
        if mismatches > 1 {
            return false;
        }
        advance_after_mismatch(left, right, &mut left_index, &mut right_index);
    }
    true
}

fn advance_after_mismatch(
    left: &[char],
    right: &[char],
    left_index: &mut usize,
    right_index: &mut usize,
) {
    match left.len().cmp(&right.len()) {
        std::cmp::Ordering::Greater => *left_index += 1,
        std::cmp::Ordering::Less => *right_index += 1,
        std::cmp::Ordering::Equal => {
            *left_index += 1;
            *right_index += 1;
        }
    }
}

fn is_stop_word(term: &str) -> bool {
    STOP_WORDS.contains(&term)
}
