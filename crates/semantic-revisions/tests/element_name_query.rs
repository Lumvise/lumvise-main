use lumvise_semantic_revisions::SemanticElementNameQuery;

#[test]
fn name_query_preserves_normalization_tokens_and_score_precedence() {
    let query = SemanticElementNameQuery::new("  Parse-FILE  ");
    assert_eq!(query.normalized(), "parse-file");
    assert_eq!(query.tokens(), ["parse", "file"]);
    for (name, id, expected) in [
        ("parse-file", "id", 4),
        ("pre-parse-file-post", "id", 3),
        ("file reader", "id", 2),
        ("other", "id:parse-file", 1),
        ("other", "id", 0),
    ] {
        assert_eq!(query.score(name, id), expected);
    }
    assert_eq!(SemanticElementNameQuery::new("Ä A").normalized(), "Ä a");
}
