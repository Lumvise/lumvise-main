//! Parent propagation preserves exact identities independently of parser order.
mod common;
use common::*;
use lumvise_semantic_revisions::*;

fn nested_sources() -> Vec<SemanticElement> {
    let mut elements = vec![element("parent", "a.rs", "parent 1")];
    for index in 0..64 {
        let id = format!("child-{index}");
        let parent = if index == 0 {
            "parent".into()
        } else {
            format!("child-{}", index - 1)
        };
        let mut child = element(&id, "a.rs", "unchanged child");
        child.parent_element_id = Some(parent);
        elements.push(child);
    }
    elements.push(element("unrelated", "b.rs", "unchanged sibling"));
    elements
}

fn parent_change(reverse: bool) -> SemanticRevisionPublicationPlan {
    let mut project = ProjectFixture::new();
    let mut elements = nested_sources();
    project.publish(&project.edit("initial-parents", elements.clone()));
    elements[0].metadata["body"] = "parent 2".into();
    if reverse {
        elements.reverse();
    }
    project.plan(&project.edit("advance-parent", elements))
}

#[test]
fn reverse_ordered_descendants_receive_the_same_exact_revisions() {
    let mut ordered = parent_change(false);
    let mut reversed = parent_change(true);
    ordered
        .connections
        .sort_by(|left, right| left.connection_id.cmp(&right.connection_id));
    reversed
        .connections
        .sort_by(|left, right| left.connection_id.cmp(&right.connection_id));
    assert_eq!(ordered.records, reversed.records);
    assert_eq!(ordered.connections, reversed.connections);
    assert_eq!(ordered.selections, reversed.selections);
    assert_exact_parents(&reversed.records);
}

fn assert_exact_parents(records: &[SemanticStoredRevision]) {
    for record in records {
        let SemanticRevisionBody::Element(element) = &record.body else {
            continue;
        };
        if record.unselected_at_ms.is_some() || element.name == "parent" {
            continue;
        }
        let parent = element.parent_element_id.as_ref().unwrap();
        assert!(
            parent.starts_with("revision:"),
            "child {} kept old parent {parent}",
            element.name
        );
    }
}

#[test]
fn parent_changes_leave_other_roots_and_unchanged_reindexes_alone() {
    let plan = parent_change(true);
    let SemanticRevisionPublicationOutcome::Accepted(receipt) = plan.outcome else {
        panic!("expected accepted parent change")
    };
    assert_eq!(accepted_id(&receipt, "unrelated"), "unrelated");
    assert_eq!(receipt.revision_links.len(), 65);
    let mut project = ProjectFixture::new();
    let elements = nested_sources();
    project.publish(&project.edit("initial-parents", elements.clone()));
    let unchanged = project.plan(&project.edit("unchanged-parents", elements));
    assert!(unchanged.records.is_empty());
    assert!(unchanged.revision_links.is_empty());
}
