#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFingerprintParts {
    pub simhash: Option<u64>,
    pub exact_hash: String,
}

/// Parses a versioned `fp1:<simhash>:<hash>` content fingerprint.
///
/// # Example
///
/// ```
/// let parts = lumvise_db_core::parse_content_fingerprint("fp1:0000000000000001:a").unwrap();
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
/// assert!(lumvise_db_core::fingerprints_match_exactly(
///     "fp1:0000000000000001:a",
///     "fp1:0000000000000002:a",
/// ));
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
/// let distance = lumvise_db_core::fingerprint_hamming_distance(
///     "fp1:0000000000000001:a",
///     "fp1:0000000000000003:b",
/// );
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
