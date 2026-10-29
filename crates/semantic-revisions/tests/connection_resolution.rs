//! Connection identities remain exact metadata when source bodies and selections advance.
use lumvise_semantic_revisions::*;
use serde_json::json;

fn edge(id: &str, kind: &str) -> SemanticRevisionConnection {
    SemanticRevisionConnection {
        connection_id: id.into(),
        relationship: SemanticRelationship {
            project_root: "/repo".into(),
            source_element_id: "e17".into(),
            target_element_id: "e4".into(),
            relationship_kind: kind.into(),
            label: "contains".into(),
            metadata: json!({"counter":u64::MAX}),
        },
    }
}

fn state() -> SemanticRevisionState {
    SemanticRevisionState {
        project_root: "/repo".into(),
        project_id: "lineage".into(),
        connections: vec![edge("opaque/%_2", "calls"), edge("opaque/%_1", "calls")],
        ..Default::default()
    }
}

fn query(ids: &[&str]) -> SemanticRevisionConnectionQuery {
    SemanticRevisionConnectionQuery {
        project_root: "/repo".into(),
        project_id: "lineage".into(),
        connection_ids: ids.iter().map(|id| (*id).into()).collect(),
    }
}

#[test]
fn exact_connections_need_no_current_membership_or_available_endpoint_body() {
    let state = state();
    let definitions = state
        .resolve_connections(&query(&["opaque/%_1", "opaque/%_2"]))
        .unwrap();
    assert_eq!(
        definitions,
        vec![state.connections[1].clone(), state.connections[0].clone()]
    );
    assert!(state.resolve_connections(&query(&[])).unwrap().is_empty());
    let input = serde_json::to_vec(
        &json!({"operation":"connections", "state":state, "query":query(&["opaque/%_1"])}),
    )
    .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&evaluate_revision_policy(&input)).unwrap();
    assert_eq!(value["status"], "accepted");
    assert_eq!(
        value["result"]["value"][0]["relationship"]["metadata"]["counter"],
        u64::MAX
    );
}

#[test]
fn missing_or_ambiguous_connection_facts_and_wrong_lineages_fail_explicitly() {
    for ids in [vec!["missing"], vec![""], vec!["opaque/%_1", "opaque/%_1"]] {
        assert!(state().resolve_connections(&query(&ids)).is_err());
    }
    let mut wrong = query(&["opaque/%_1"]);
    wrong.project_id = "different-lineage".into();
    assert!(
        state()
            .resolve_connections(&wrong)
            .unwrap_err()
            .to_string()
            .contains("lineage")
    );
    wrong.project_id = "lineage".into();
    wrong.project_root = "/other".into();
    assert!(state().resolve_connections(&wrong).is_err());
    let mut ambiguous = state();
    ambiguous.connections.push(ambiguous.connections[0].clone());
    assert!(
        ambiguous
            .resolve_connections(&query(&["opaque/%_1"]))
            .is_err()
    );
}

#[test]
fn source_scope_ownership_is_shared_and_containment_also_belongs_to_the_child() {
    let call = edge("call", "calls");
    let contains = edge("contains", "contains");
    assert_eq!(
        call.relationship.owned_element_ids().collect::<Vec<_>>(),
        ["e17"]
    );
    assert_eq!(
        contains
            .relationship
            .owned_element_ids()
            .collect::<Vec<_>>(),
        ["e17", "e4"]
    );
}
