use super::*;

#[test]
fn cold_path_lookup_does_not_build_name_search_postings() {
    let graph = GrafeoDB::new_in_memory();
    let node = graph.create_node_with_props(
        &["SemanticElement"],
        [
            ("project_root", Value::from("/a")),
            ("semantic_element_id", Value::from("child")),
            ("semantic_source_id", Value::from("source")),
            ("element_kind", Value::from("file")),
            ("name", Value::from("child")),
            ("path", Value::from(" ./src/child.rs ")),
        ],
    );
    let mut lookup = SemanticElementLookup::default();
    assert_eq!(
        lookup.select_path(&graph, 0, "/a", "src/child.rs", 1, false),
        [node]
    );
    assert!(
        lookup.index.entries.is_empty(),
        "path reads must not build the global name index"
    );
    assert_eq!(lookup.select(&graph, 0, Some("/a"), "child", 1), [node]);
    graph.set_node_property(node, "path", Value::from("src/moved.rs"));
    assert!(
        lookup
            .select_path(&graph, 1, "/a", "src/child.rs", 1, false)
            .is_empty()
    );
    assert_eq!(
        lookup.select_path(&graph, 1, "/a", "src/moved.rs", 1, false),
        [node]
    );
}

fn indexed_names(names: &[(&str, &str, &str)]) -> SemanticElementIndex {
    let mut index = SemanticElementIndex::default();
    for (ordinal, (root, id, name)) in names.iter().enumerate() {
        index.insert(IndexedSemanticElement {
            node_id: NodeId::new(ordinal as u64),
            project_root: (*root).into(),
            semantic_id: (*id).into(),
            normalized_id: id.to_ascii_lowercase(),
            normalized_name: name.to_ascii_lowercase(),
            normalized_path: format!("src/{id}.rs"),
            lifecycle: "active".into(),
            start_line: None,
            end_line: None,
        });
    }
    index
}

#[test]
fn candidate_index_preserves_exact_substring_token_and_identifier_ranking() {
    let index = indexed_names(&[
        ("/a", "z", "Run_Task"),
        ("/a", "a", "before_run_task"),
        ("/a", "b", "run"),
        ("/a", "run_task:id", "unrelated"),
        ("/a", "c", "task"),
    ]);
    assert_eq!(
        index.search(Some("/a"), &ElementNameQuery::new(" RUN_TASK "), 4),
        [0, 1, 2, 4].map(NodeId::new)
    );
    assert_eq!(
        index.search(None, &ElementNameQuery::new("run_task"), 10),
        [0, 1, 2, 4, 3].map(NodeId::new)
    );
}

#[test]
fn candidate_index_preserves_short_unicode_punctuation_and_scope_matches() {
    let index = indexed_names(&[
        ("/a", "1", "é"),
        ("/b", "2", "é"),
        ("/a", "3", "__"),
        ("/a", "4", "漢字語"),
        ("/a", "5", "UPPER"),
    ]);
    for (query, expected) in [("é", 0), ("__", 2), ("字語", 3), ("per", 4)] {
        assert_eq!(
            index.search(Some("/a"), &ElementNameQuery::new(query), 10),
            [NodeId::new(expected)]
        );
    }
    assert!(
        index
            .search(None, &ElementNameQuery::new("missing"), 10)
            .is_empty()
    );
    assert!(
        index
            .search(None, &ElementNameQuery::new("é"), 0)
            .is_empty()
    );
}

#[test]
fn substring_index_verifies_false_positive_grams_and_deduplicates_repeated_grams() {
    let index = indexed_names(&[
        ("/a", "1", "abc_bcd_cde"),
        ("/a", "2", "abcde"),
        ("/a", "3", "aaaaaa"),
        ("/a", "4", "ABCDEF"),
    ]);
    assert_eq!(
        index.search(None, &ElementNameQuery::new("abcde"), 10),
        [1, 3].map(NodeId::new)
    );
    assert_eq!(
        index.search(None, &ElementNameQuery::new("aaaa"), 10),
        [NodeId::new(2)]
    );
}

#[test]
fn selective_queries_do_not_score_every_indexed_name() {
    let mut index = indexed_names(&[("/a", "needle", "unique_needlexyz")]);
    for ordinal in 1..10_000 {
        index.insert(IndexedSemanticElement {
            node_id: NodeId::new(ordinal),
            project_root: "/a".into(),
            semantic_id: format!("ordinary-{ordinal}"),
            normalized_id: format!("ordinary-{ordinal}"),
            normalized_name: format!("ordinary-{ordinal}"),
            normalized_path: format!("src/{ordinal}.rs"),
            lifecycle: "active".into(),
            start_line: None,
            end_line: None,
        });
    }
    assert_eq!(
        index.candidate_indices(&ElementNameQuery::new("needlexyz")),
        HashSet::from([0])
    );
}
