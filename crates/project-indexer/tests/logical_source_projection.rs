//! Checkout paths route source reads; an injected logical source owns shared IDs.
use lumvise_contracts::{IndexBatchRequest, SemanticSourceUpsert};
use lumvise_project_indexer::{
    FilesystemProjectSource, ProjectIndexer, ScanScope, SemanticIndexProjection,
    TreeSitterProjectParser,
};
use std::{fs, path::Path};

fn logical_source() -> SemanticSourceUpsert {
    SemanticSourceUpsert {
        semantic_source_id: "filesystem:established-opaque-identity".into(),
        kind: "filesystem".into(),
        name: "Shared project source".into(),
        root_path: None,
        root_uri: "lumvise://lineage/source/main".into(),
    }
}

fn projected_at(root: &Path) -> IndexBatchRequest {
    fs::write(
        root.join("main.rs"),
        "fn first() { second(); } fn second() {}\n",
    )
    .unwrap();
    let source = FilesystemProjectSource::open(root).unwrap();
    let mut projection =
        SemanticIndexProjection::with_source(source.root(), "shared-provider", logical_source())
            .unwrap();
    let scan = ProjectIndexer::new(source, TreeSitterProjectParser::default())
        .prepare(ScanScope::Full)
        .unwrap();
    projection.project(&scan).unwrap()
}

#[test]
fn different_checkout_routes_preserve_source_records_parents_and_call_endpoints() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let left = projected_at(a.path());
    let right = projected_at(b.path());
    assert_ne!(left.project_root, right.project_root);
    assert_eq!(left.semantic_sources, right.semantic_sources);
    assert_eq!(left.semantic_elements, right.semantic_elements);
    assert_eq!(left.semantic_relationships, right.semantic_relationships);
    assert!(
        left.semantic_elements
            .iter()
            .all(|element| element.semantic_source_id == logical_source().semantic_source_id)
    );
    assert!(
        left.semantic_relationships
            .iter()
            .any(|edge| edge.relationship_label == "calls")
    );
}

#[test]
fn malformed_logical_source_is_rejected_before_projection() {
    for field in ["id", "kind", "uri"] {
        let mut source = logical_source();
        match field {
            "id" => source.semantic_source_id = " ".into(),
            "kind" => source.kind.clear(),
            _ => source.root_uri.clear(),
        }
        assert!(
            SemanticIndexProjection::with_source(
                Path::new("/workspace/project"),
                "shared-provider",
                source
            )
            .is_err()
        );
    }
}
