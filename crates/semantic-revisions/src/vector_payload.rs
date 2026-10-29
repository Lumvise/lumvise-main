//! Stored vector shape validation shared by native and WASM adapters; ranking stays in DB Core.
use crate::{SemanticRevisionError, SemanticRevisionResult};
use lumvise_contracts::ArtifactTextVector;

/// Checks the existing nonzero dimension and finite-value contract before any storage mutation.
/// Example: `validate_vector_payload("artifact-id", &vector)?`.
pub fn validate_vector_payload(
    id: &str,
    vector: &ArtifactTextVector,
) -> SemanticRevisionResult<()> {
    if vector.dimensions == 0
        || vector.dimensions != vector.vector.len()
        || !vector.vector.iter().all(|value| value.is_finite())
    {
        return Err(SemanticRevisionError::invalid_value(
            id,
            "finite vector matching nonzero declared dimensions",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_vectors_require_matching_nonzero_dimensions_and_finite_values() {
        let mut vector = ArtifactTextVector {
            engine_id: "engine".into(),
            model: None,
            dimensions: 2,
            vector: vec![0.0, -1.0],
            normalized: false,
            metadata: serde_json::json!({}),
        };
        assert!(validate_vector_payload("record", &vector).is_ok());
        for dimensions in [0, 1, 3] {
            vector.dimensions = dimensions;
            assert!(validate_vector_payload("record", &vector).is_err());
        }
        vector.dimensions = 2;
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            vector.vector[0] = value;
            assert!(validate_vector_payload("record", &vector).is_err());
        }
    }
}
