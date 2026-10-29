use super::*;

#[test]
fn only_explicit_null_membership_establishes_an_empty_base() {
    assert_eq!(
        decode_index_base_membership(&json!({"membership":null})).unwrap(),
        SemanticSelectionExpectation::default()
    );
    assert!(decode_index_base_membership(&json!({})).is_err());
    assert!(decode_index_base_membership(&json!({"membership":false})).is_err());
}

#[test]
fn malformed_or_different_selection_headers_never_authorize_a_write() {
    for members in [
        json!({"selection":{"selection_id":"working", "source_context":"another-branch", "generation":1}, "records":[], "connection_ids":[]}),
        json!({"selection":{"selection_id":"public", "source_context":"local", "generation":1}, "records":[], "connection_ids":[]}),
        json!({"selection":{"selection_id":"working", "source_context":"local", "generation":"1"}, "records":[], "connection_ids":[]}),
        json!({"selection":{"selection_id":"working", "source_context":"local", "generation":1}, "records":[], "connection_ids":null}),
    ] {
        let error = decode_index_base_membership(&json!({"membership":members})).unwrap_err();
        assert_eq!(error.code, "invalid_semantic_storage");
        assert!(error.message.contains("expected"));
    }
}
