//! Current views honor selection membership independently of a body's lifecycle.
mod common;
use common::RevisionTestClient;
use lumvise_db_core::*;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

const ROOT: &str = "/revision-views";

fn execute(local: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    SemanticPersistence::execute(local, operation, &InvocationControl::sixty_seconds()).unwrap()
}

fn source(name: &str, lifecycle: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: ROOT.into(),
        semantic_element_id: "source".into(),
        semantic_source_id: "test".into(),
        path: "src/main.rs".into(),
        element_kind: "function".into(),
        name: name.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: lifecycle.into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn publish(local: &LocalPersistence, operation_id: &str, element: SemanticElement) -> String {
    let request =
        RevisionTestClient::new(local, ROOT, "local").publication(operation_id, vec![element]);
    let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(receipt)) =
        execute(local, SemanticOperation::PublishRevisions(request))
    else {
        panic!("expected an accepted source revision");
    };
    receipt.remaps[0].accepted.record_id.clone()
}

fn current_sources(local: &LocalPersistence, include_inactive: bool) -> Vec<SemanticElement> {
    let SemanticResult::ScopedGraph(graph) = execute(
        local,
        SemanticOperation::ScopedRead(ScopedSemanticRead::PathPrefix {
            project_root: ROOT.into(),
            prefix: "src/".into(),
            include_inactive,
        }),
    ) else {
        panic!("expected the selected source view");
    };
    graph.elements
}

#[test]
fn selecting_an_inactive_body_preserves_its_canonical_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project");
    let expected = source("inactive", "inactive");
    {
        let local = LocalPersistence::open(&path).unwrap();
        publish(&local, "inactive", expected.clone());
        assert_eq!(current_sources(&local, true), vec![expected.clone()]);
        assert!(current_sources(&local, false).is_empty());
    }
    let reopened = LocalPersistence::open(&path).unwrap();
    assert_eq!(current_sources(&reopened, true), vec![expected]);
    assert!(current_sources(&reopened, false).is_empty());
}

fn assert_current_counts(local: &LocalPersistence) {
    let SemanticResult::ProjectElementCounts { total_elements, .. } = execute(
        local,
        SemanticOperation::ProjectElementCounts {
            project_root: ROOT.into(),
        },
    ) else {
        panic!("expected current project counts");
    };
    assert_eq!(
        total_elements, 1,
        "retained history must not inflate current counts"
    );
    let SemanticResult::ScopedGraph(graph) = execute(
        local,
        SemanticOperation::ScopedRead(ScopedSemanticRead::Dependency {
            semantic_element_id: current_sources(local, true)[0].semantic_element_id.clone(),
            direction: SemanticDependencyDirection::Both,
            max_depth: 1,
            include_descendants: false,
            include_inactive: true,
        }),
    ) else {
        panic!("expected current dependency scope");
    };
    assert_eq!(graph.scope_element_count, Some(1));
}

#[test]
fn current_counts_exclude_retained_bodies_before_and_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project");
    {
        let local = LocalPersistence::open(&path).unwrap();
        publish(&local, "before", source("before", "active"));
        publish(&local, "after", source("after", "active"));
        assert_eq!(current_sources(&local, true).len(), 1);
        assert_current_counts(&local);
    }
    assert_current_counts(&LocalPersistence::open(&path).unwrap());
}
