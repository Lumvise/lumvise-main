use super::*;

fn selection() -> SemanticRevisionSelection {
    SemanticRevisionSelection {
        project_root: "/project".into(),
        selection_id: "working".into(),
        source_context: "main".into(),
        mode: SemanticSelectionMode::FollowAccepted,
        generation: 8,
        source_anchor: None,
    }
}

fn link(older: &str, newer: &str, context: &str) -> SemanticRevisionLink {
    SemanticRevisionLink {
        project_root: "/project".into(),
        source_context: context.into(),
        older: SemanticRecordRef::element(older),
        newer: SemanticRecordRef::element(newer),
        created_at_ms: 1,
        commit_version: 2,
    }
}

fn query(records: Vec<SemanticRecordRef>) -> SemanticRevisionFamilyQuery {
    SemanticRevisionFamilyQuery {
        project_root: "/project".into(),
        selection_id: "working".into(),
        expected_commit_version: None,
        records,
    }
}

fn basis(selected: &[&str], links: Vec<SemanticRevisionLink>) -> SemanticRevisionFamilyBasis {
    SemanticRevisionFamilyBasis {
        project_root: "/project".into(),
        commit_version: 10,
        selection: Some(selection()),
        selected_records: selected
            .iter()
            .map(|id| SemanticRecordRef::element(*id))
            .collect(),
        links,
    }
}

#[test]
fn a_changed_read_view_requires_reloading_the_snapshot() {
    let facts = basis(&["e23"], vec![link("e17", "e23", "main")]);
    let mut request = query(vec![SemanticRecordRef::element("e17")]);
    request.expected_commit_version = Some(9);
    assert!(
        facts
            .resolve(&request)
            .unwrap_err()
            .to_string()
            .contains("captured read commit")
    );
    request.expected_commit_version = Some(10);
    assert_eq!(
        facts.resolve(&request).unwrap()[0].accepted,
        SemanticRecordRef::element("e23")
    );
}

#[test]
fn resolves_exact_chosen_members_without_any_revision_bodies() {
    let mut facts = basis(
        &["e23", "untouched"],
        vec![link("e17", "e20", "main"), link("e20", "e23", "main")],
    );
    facts
        .selected_records
        .push(SemanticRecordRef::artifact("a17"));
    let request = query(vec![
        SemanticRecordRef::element("e17"),
        SemanticRecordRef::artifact("a17"),
        SemanticRecordRef::element("e17"),
        SemanticRecordRef::element("missing"),
    ]);
    let remaps = facts.resolve(&request).unwrap();
    assert_eq!(remaps.len(), 2);
    assert!(remaps.contains(&SemanticRevisionRemap {
        incoming: SemanticRecordRef::element("e17"),
        accepted: SemanticRecordRef::element("e23")
    }));
    assert!(remaps.contains(&SemanticRevisionRemap {
        incoming: SemanticRecordRef::artifact("a17"),
        accepted: SemanticRecordRef::artifact("a17")
    }));
}

#[test]
fn source_context_and_pins_choose_revisions_instead_of_recency() {
    let request = query(vec![SemanticRecordRef::element("e17")]);
    let links = vec![link("e17", "e23", "main"), link("e17", "e99", "other")];
    let mut facts = basis(&["e17", "e99"], links);
    facts.selection.as_mut().unwrap().mode = SemanticSelectionMode::Pinned;
    assert_eq!(
        facts.resolve(&request).unwrap()[0].accepted,
        SemanticRecordRef::element("e17")
    );
    facts.selected_records = vec![SemanticRecordRef::element("e99")];
    assert!(facts.resolve(&request).unwrap().is_empty());
}

#[test]
fn two_selected_successors_require_an_explicit_fork_choice() {
    let facts = basis(
        &["left", "right"],
        vec![link("e17", "left", "main"), link("e17", "right", "main")],
    );
    assert!(
        facts
            .resolve(&query(vec![SemanticRecordRef::element("e17")]))
            .unwrap_err()
            .to_string()
            .contains("fork")
    );
}

#[test]
fn withdrawn_or_missing_selections_do_not_return_historical_bodies() {
    let mut facts = basis(&[], vec![link("e17", "e23", "main")]);
    let request = query(vec![SemanticRecordRef::element("e17")]);
    assert!(facts.resolve(&request).unwrap().is_empty());
    facts.selection = None;
    assert!(facts.resolve(&request).unwrap().is_empty());
}

#[test]
fn rejects_wrong_project_selection_and_empty_references() {
    let facts = basis(&["e17"], vec![]);
    let mut request = query(vec![SemanticRecordRef::element("e17")]);
    request.project_root = "/another".into();
    assert!(facts.resolve(&request).is_err());
    request.project_root = "/project".into();
    request.selection_id = "public".into();
    assert!(facts.resolve(&request).is_err());
    assert!(
        facts
            .resolve(&query(vec![SemanticRecordRef::artifact("")]))
            .is_err()
    );
}

#[test]
fn byte_policy_uses_the_identical_family_resolution() {
    let facts = basis(&["e23"], vec![link("e17", "e23", "main")]);
    let request = query(vec![SemanticRecordRef::element("e17")]);
    let bytes = serde_json::to_vec(
        &serde_json::json!({"operation":"families", "basis":facts, "query":request}),
    )
    .unwrap();
    let result: serde_json::Value =
        serde_json::from_slice(&evaluate_revision_policy(&bytes)).unwrap();
    assert_eq!(result["status"], "accepted", "{result}");
    assert_eq!(
        result["result"]["value"],
        serde_json::json!(facts.resolve(&request).unwrap())
    );
}
