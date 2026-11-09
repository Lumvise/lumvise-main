//! Source-preserving range selection; the owning engine supplies its real tokenizer.
use crate::text2vector::TextChunkRange;
use crate::{NeuralError, Result};

pub(super) struct TokenMeasurement {
    pub count: usize,
    pub source_ends: Vec<usize>,
}

pub(super) fn plan_ranges(
    text: &str,
    budget: usize,
    measure: impl Fn(&str) -> Result<TokenMeasurement>,
) -> Result<Vec<TextChunkRange>> {
    require_budget(budget, measure("")?.count)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let range = next_source_range(text, start, budget, &measure)?;
        start = range.end_byte;
        ranges.push(range);
    }
    Ok(ranges)
}

fn next_source_range(
    text: &str,
    start: usize,
    budget: usize,
    measure: &impl Fn(&str) -> Result<TokenMeasurement>,
) -> Result<TextChunkRange> {
    let (length, token_count) = next_range(&text[start..], budget, measure)?;
    Ok(TextChunkRange {
        start_byte: start,
        end_byte: start + length,
        token_count,
    })
}

fn next_range(
    text: &str,
    budget: usize,
    measure: &impl Fn(&str) -> Result<TokenMeasurement>,
) -> Result<(usize, usize)> {
    let full = measure(text)?;
    if full.count <= budget {
        return Ok((text.len(), full.count));
    }
    verified_prefix(text, candidate_end(text, &full, budget), budget, measure)
}

fn candidate_end(text: &str, measured: &TokenMeasurement, budget: usize) -> usize {
    // Reserve the final special token; candidate slices are verified again below.
    let candidate = measured
        .source_ends
        .iter()
        .take(budget - 1)
        .copied()
        .max()
        .unwrap_or(0);
    let first_end = text.chars().next().map(char::len_utf8).unwrap_or(0);
    candidate.max(first_end).min(text.len())
}

fn verified_prefix(
    text: &str,
    end: usize,
    budget: usize,
    measure: &impl Fn(&str) -> Result<TokenMeasurement>,
) -> Result<(usize, usize)> {
    for end in (1..=end).rev().filter(|end| text.is_char_boundary(*end)) {
        let count = measure(&text[..end])?.count;
        if count <= budget {
            return Ok((end, count));
        }
    }
    Err(NeuralError::InvalidValue {
        value: budget.to_string(),
        expected:
            "token budget sufficient for the task prefix, special tokens and one source codepoint"
                .into(),
    })
}

fn require_budget(budget: usize, prefix_tokens: usize) -> Result<()> {
    if budget > prefix_tokens {
        return Ok(());
    }
    Err(NeuralError::InvalidValue {
        value: budget.to_string(),
        expected: format!(
            "max_tokens greater than task-prefix and special-token count {prefix_tokens}"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnicodeTokenMeter;
    impl UnicodeTokenMeter {
        fn measure(&self, text: &str) -> Result<TokenMeasurement> {
            let mut source_ends = vec![0];
            source_ends.extend(
                text.char_indices()
                    .map(|(start, value)| start + value.len_utf8()),
            );
            source_ends.push(0);
            Ok(TokenMeasurement {
                count: source_ends.len(),
                source_ends,
            })
        }
    }

    #[test]
    fn ranges_preserve_multibyte_source_and_special_token_budget() {
        let ranges = plan_ranges("a€bc", 4, |text| UnicodeTokenMeter.measure(text)).unwrap();
        assert_eq!(
            ranges,
            vec![
                TextChunkRange {
                    start_byte: 0,
                    end_byte: 4,
                    token_count: 4
                },
                TextChunkRange {
                    start_byte: 4,
                    end_byte: 6,
                    token_count: 4
                },
            ]
        );
    }

    #[test]
    fn whitespace_has_no_ranges_and_insufficient_budget_is_rejected() {
        assert!(
            plan_ranges(" \n\t", 4, |text| UnicodeTokenMeter.measure(text))
                .unwrap()
                .is_empty()
        );
        let error =
            plan_ranges("private source", 2, |text| UnicodeTokenMeter.measure(text)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("task-prefix and special-token count 2")
        );
        assert!(!error.to_string().contains("private source"));
    }

    struct BoundaryTokenMeter;
    impl BoundaryTokenMeter {
        fn measure(&self, text: &str) -> Result<TokenMeasurement> {
            let mut measured = UnicodeTokenMeter.measure(text)?;
            if text.len() == 2 {
                measured.count += 1;
            }
            Ok(measured)
        }
    }

    #[test]
    fn boundary_retokenization_can_shorten_a_candidate() {
        let ranges = plan_ranges("abcd", 4, |text| BoundaryTokenMeter.measure(text)).unwrap();
        assert_eq!(
            ranges
                .iter()
                .map(|range| range.end_byte)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(ranges.iter().all(|range| range.token_count == 3));
    }
}
