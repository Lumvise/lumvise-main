//! Parent containment lookup preserves explicit edge content and duplicate occurrences.
mod common;
use common::*;
use lumvise_semantic_revisions::*;
use serde_json::json;

fn parented_elements(count: usize) -> Vec<SemanticElement> {
    let mut elements = vec![element("parent", "a.rs", "parent")];
    for index in 0..count {
        let mut child = element(&format!("child-{index}"), "a.rs", "child");
        child.parent_element_id = Some("parent".into());
        elements.push(child);
    }
    elements
}

fn explicit_edge(child: usize, kind: &str) -> SemanticRelationship {
    SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: "parent".into(),
        target_element_id: format!("child-{child}"),
        relationship_kind: kind.into(),
        label: "parser edge".into(),
        metadata: json!({"origin":"parser", "ordinal":child}),
    }
}

fn mixed_containment_edges() -> Vec<SemanticRelationship> {
    vec![
        explicit_edge(0, "contains"),
        explicit_edge(0, "contains"),
        explicit_edge(1, "calls"),
    ]
}

#[test]
fn explicit_containment_duplicates_and_other_edge_kinds_remain_exact() {
    let project = ProjectFixture::new();
    let mut request = project.edit("mixed-containment", parented_elements(2));
    let explicit = mixed_containment_edges();
    request.structure.as_mut().unwrap().relationships = explicit.clone();
    let plan = project.plan(&request);
    let edges = plan
        .connections
        .iter()
        .map(|edge| edge.relationship.clone())
        .collect::<Vec<_>>();
    assert_eq!(&edges[..explicit.len()], explicit.as_slice());
    assert_eq!(edges.len(), 4);
    assert_eq!(edges[3].target_element_id, "child-1");
    assert_eq!(edges[3].relationship_kind, "contains");
    assert_eq!(
        edges[3].metadata,
        json!({"derived_from":"parent_element_id"})
    );
}

#[test]
fn large_parented_publication_preserves_connections_on_unchanged_reindex() {
    let mut project = ProjectFixture::new();
    let mut request = project.edit("large-containment", parented_elements(5000));
    request.structure.as_mut().unwrap().relationships = (0..5000)
        .step_by(2)
        .map(|child| explicit_edge(child, "contains"))
        .collect();
    let receipt = project.publish(&request);
    assert_eq!(receipt.accepted_scope.connection_ids.len(), 5000);
    let expected = project.selected().connections;
    let mut unchanged = project.edit(
        "unchanged-containment",
        request.structure.as_ref().unwrap().elements.clone(),
    );
    unchanged.structure.as_mut().unwrap().relationships = request.structure.unwrap().relationships;
    project.publish(&unchanged);
    assert_eq!(project.selected().connections, expected);
}
