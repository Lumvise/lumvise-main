#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFingerprintParts {
    pub simhash: Option<u64>,
    pub exact_hash: String,
}

impl ContentFingerprintParts {
    /// Parses the portable fp1 identity evidence. Example: `ContentFingerprintParts::parse("fp1:0000000000000001:a")`.
    pub fn parse(value: &str) -> Option<Self> {
        parse_content_fingerprint(value)
    }

    /// Tests exact content evidence independently of the simhash. Example: `left.matches_exact(&right)`.
    pub fn matches_exact(&self, other: &Self) -> bool {
        self.exact_hash == other.exact_hash
    }

    /// Measures similarity when both values carry a simhash. Example: `left.hamming_distance(&right)`.
    pub fn hamming_distance(&self, other: &Self) -> Option<u32> {
        Some((self.simhash? ^ other.simhash?).count_ones())
    }
}

pub(crate) fn fingerprint_algorithm(element: &crate::SemanticElement) -> Option<&str> {
    element
        .metadata
        .get("fingerprint_algorithm")
        .and_then(serde_json::Value::as_str)
}

/// Parses a versioned `fp1:<simhash>:<hash>` content fingerprint.
///
/// # Example
///
/// ```
/// let parts = lumvise_db_core::ContentFingerprintParts::parse("fp1:0000000000000001:a").unwrap();
/// assert_eq!(parts.simhash, Some(1));
/// ```
pub fn parse_content_fingerprint(fingerprint: &str) -> Option<ContentFingerprintParts> {
    if fingerprint.is_empty() {
        return None;
    }
    parse_fp1_fingerprint(fingerprint)
}

/// Returns true when two fingerprints share the same exact hash portion.
///
/// # Example
///
/// ```
/// let left = lumvise_db_core::ContentFingerprintParts::parse("fp1:0000000000000001:a").unwrap();
/// let right = lumvise_db_core::ContentFingerprintParts::parse("fp1:0000000000000002:a").unwrap();
/// assert!(left.matches_exact(&right));
pub fn fingerprints_match_exactly(left: &str, right: &str) -> bool {
    parse_content_fingerprint(left)
        .zip(parse_content_fingerprint(right))
        .is_some_and(|(lhs, rhs)| lhs.exact_hash == rhs.exact_hash)
}

/// Returns the simhash Hamming distance when both fingerprints expose simhash.
///
/// # Example
///
/// ```
/// let left = lumvise_db_core::ContentFingerprintParts::parse("fp1:0000000000000001:a").unwrap();
/// let right = lumvise_db_core::ContentFingerprintParts::parse("fp1:0000000000000003:b").unwrap();
/// let distance = left.hamming_distance(&right);
/// assert_eq!(distance, Some(1));
/// ```
pub fn fingerprint_hamming_distance(left: &str, right: &str) -> Option<u32> {
    let left = parse_content_fingerprint(left)?.simhash?;
    let right = parse_content_fingerprint(right)?.simhash?;
    Some((left ^ right).count_ones())
}

fn parse_fp1_fingerprint(fingerprint: &str) -> Option<ContentFingerprintParts> {
    let rest = fingerprint.strip_prefix("fp1:")?;
    let (simhash_hex, exact_hash) = rest.split_once(':')?;
    if simhash_hex.len() != 16 || exact_hash.is_empty() {
        return None;
    }
    Some(ContentFingerprintParts {
        simhash: Some(u64::from_str_radix(simhash_hex, 16).ok()?),
        exact_hash: exact_hash.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_content_fingerprint;

    #[test]
    fn unversioned_fingerprint_is_rejected() {
        assert!(parse_content_fingerprint("unversioned-hash").is_none());
    }
}
