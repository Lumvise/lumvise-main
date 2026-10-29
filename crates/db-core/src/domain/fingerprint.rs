pub use lumvise_semantic_revisions::ContentFingerprintParts;

pub(crate) fn fingerprint_algorithm(element: &crate::SemanticElement) -> Option<&str> {
    element.fingerprint_algorithm()
}
