pub const DEFAULT_FINGERPRINT_MAX_DISTANCE: u32 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFingerprintParts {
    pub simhash: Option<u64>,
    pub exact_hash: String,
}

pub fn parse_content_fingerprint(fingerprint: &str) -> Option<ContentFingerprintParts> {
    if fingerprint.is_empty() {
        return None;
    }

    if let Some(rest) = fingerprint.strip_prefix("fp1:") {
        let (simhash_hex, exact_hash) = rest.split_once(':')?;
        if simhash_hex.len() != 16 || exact_hash.is_empty() {
            return None;
        }
        let simhash = u64::from_str_radix(simhash_hex, 16).ok()?;
        return Some(ContentFingerprintParts {
            simhash: Some(simhash),
            exact_hash: exact_hash.to_string(),
        });
    }

    Some(ContentFingerprintParts {
        simhash: None,
        exact_hash: fingerprint.to_string(),
    })
}

pub fn fingerprints_match_exactly(left: &str, right: &str) -> bool {
    parse_content_fingerprint(left)
        .zip(parse_content_fingerprint(right))
        .map(|(lhs, rhs)| lhs.exact_hash == rhs.exact_hash)
        .unwrap_or(false)
}

pub fn fingerprint_hamming_distance(left: &str, right: &str) -> Option<u32> {
    let lhs = parse_content_fingerprint(left)?.simhash?;
    let rhs = parse_content_fingerprint(right)?.simhash?;
    Some((lhs ^ rhs).count_ones())
}

pub fn fingerprints_are_similar(left: &str, right: &str, max_distance: u32) -> bool {
    fingerprints_match_exactly(left, right)
        || fingerprint_hamming_distance(left, right)
            .is_some_and(|distance| distance <= max_distance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fp1_fingerprints_expose_simhash_and_exact_hash() {
        let parsed = parse_content_fingerprint("fp1:000000000000000a:exact-hash")
            .expect("parsed fingerprint");

        assert_eq!(parsed.simhash, Some(10));
        assert_eq!(parsed.exact_hash, "exact-hash");
    }

    #[test]
    fn legacy_exact_only_fingerprints_are_safe_to_compare() {
        let parsed = parse_content_fingerprint("legacy-exact").expect("parsed fingerprint");

        assert_eq!(parsed.simhash, None);
        assert_eq!(parsed.exact_hash, "legacy-exact");
        assert!(fingerprints_match_exactly(
            "legacy-exact",
            "fp1:000000000000ffff:legacy-exact"
        ));
        assert_eq!(
            fingerprint_hamming_distance("legacy-exact", "fp1:000000000000ffff:legacy-exact"),
            None
        );
    }
}
