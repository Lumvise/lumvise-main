use super::*;
use crate::components::one_hop_external_ids;
use crate::semantic_context::SemanticRelationship;
use serde_json::json;

struct InteractingScope {
    elements: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
    target: SemanticElement,
}

fn element(id: &str, kind: &str, name: &str, path: &str, parent: Option<&str>) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "fixture".into(),
        path: path.into(),
        element_kind: kind.into(),
        name: name.into(),
        parent_element_id: parent.map(str::to_owned),
        content_fingerprint: Some("fingerprint".into()),
        start_line: Some(1),
        end_line: Some(10),
        lifecycle: "active".into(),
        metadata: json!({}),
    }
}

fn relationship(source: &str, target: &str) -> SemanticRelationship {
    SemanticRelationship {
        project_root: "/repo".into(),
        source_element_id: source.into(),
        target_element_id: target.into(),
        relationship_kind: "semantic".into(),
        label: "calls".into(),
        lifecycle: "active".into(),
        metadata: json!({}),
    }
}

fn nested_method(
    id: &str,
    path: &str,
    name: &str,
    file_id: &str,
) -> (SemanticElement, SemanticElement) {
    let owner = element(
        &format!("fs:h:{path}:class:Owner:"),
        "class",
        "Owner",
        path,
        Some(file_id),
    );
    let method = element(id, "method", name, path, Some(&owner.semantic_element_id));
    (owner, method)
}

fn interacting_scope() -> InteractingScope {
    let target = element("fs:h:src:folder:src:", "folder", "src", "src", None);
    let source_file = element(
        "fs:h:src/a.rs:file:a.rs:",
        "file",
        "a.rs",
        "src/a.rs",
        Some(&target.semantic_element_id),
    );
    let source_function = element(
        "fs:h:src/a.rs:function:caller:",
        "function",
        "caller",
        "src/a.rs",
        Some(&source_file.semantic_element_id),
    );
    let inbound_file = element(
        "fs:h:lib/c.rs:file:c.rs:",
        "file",
        "c.rs",
        "lib/c.rs",
        Some("fs:h:lib:folder:lib:"),
    );
    let (inbound_class, inbound_method) = nested_method(
        "fs:h:lib/c.rs:method:incoming:",
        "lib/c.rs",
        "incoming",
        &inbound_file.semantic_element_id,
    );
    let outbound_file = element(
        "fs:h:lib/b.rs:file:b.rs:",
        "file",
        "b.rs",
        "lib/b.rs",
        Some("fs:h:lib:folder:lib:"),
    );
    let (outbound_class, outbound_method) = nested_method(
        "fs:h:lib/b.rs:method:outgoing:",
        "lib/b.rs",
        "outgoing",
        &outbound_file.semantic_element_id,
    );
    let second_hop_file = element(
        "fs:h:lib/deep.rs:file:deep.rs:",
        "file",
        "deep.rs",
        "lib/deep.rs",
        Some("fs:h:lib:folder:lib:"),
    );
    let (_, second_hop_method) = nested_method(
        "fs:h:lib/deep.rs:method:deep:",
        "lib/deep.rs",
        "deep",
        &second_hop_file.semantic_element_id,
    );
    let unrelated_file = element(
        "fs:h:lib/unrelated.rs:file:unrelated.rs:",
        "file",
        "unrelated.rs",
        "lib/unrelated.rs",
        Some("fs:h:lib:folder:lib:"),
    );
    let elements = vec![
        target.clone(),
        source_file.clone(),
        source_function.clone(),
        inbound_file,
        inbound_class,
        inbound_method.clone(),
        outbound_file,
        outbound_class,
        outbound_method.clone(),
        second_hop_file,
        second_hop_method,
        unrelated_file,
    ];
    let relationships = vec![
        relationship(
            &source_function.semantic_element_id,
            &outbound_method.semantic_element_id,
        ),
        relationship(
            &inbound_method.semantic_element_id,
            &source_function.semantic_element_id,
        ),
        relationship(
            &outbound_method.semantic_element_id,
            "fs:h:lib/deep.rs:method:deep:",
        ),
    ];
    InteractingScope {
        elements,
        relationships,
        target,
    }
}

fn bounded_interacting_scope() -> InteractingScope {
    let target = element("fs:h:src:folder:src:", "folder", "src", "src", None);
    let source_file = element(
        "fs:h:src/a.rs:file:a.rs:",
        "file",
        "a.rs",
        "src/a.rs",
        Some(&target.semantic_element_id),
    );
    let mut elements = vec![target.clone(), source_file.clone()];
    for index in 0..MAX_C4_ELEMENTS {
        elements.push(element(
            &format!("fs:h:src/a.rs:function:noise_{index:03}:"),
            "function",
            &format!("noise_{index:03}"),
            "src/a.rs",
            Some(&source_file.semantic_element_id),
        ));
    }
    let source_function = element(
        "fs:h:src/a.rs:function:zzzz_caller:",
        "function",
        "zzzz_caller",
        "src/a.rs",
        Some(&source_file.semantic_element_id),
    );
    let outside_file = element(
        "fs:h:lib/b.rs:file:b.rs:",
        "file",
        "b.rs",
        "lib/b.rs",
        Some("fs:h:lib:folder:lib:"),
    );
    let (outside_class, outside_method) = nested_method(
        "fs:h:lib/b.rs:method:callee:",
        "lib/b.rs",
        "callee",
        &outside_file.semantic_element_id,
    );
    elements.extend([
        source_function.clone(),
        outside_file,
        outside_class,
        outside_method.clone(),
    ]);
    InteractingScope {
        elements,
        relationships: vec![relationship(
            &source_function.semantic_element_id,
            &outside_method.semantic_element_id,
        )],
        target,
    }
}

fn rendered_report(fixture: &InteractingScope, scoped: &[SemanticElement]) -> String {
    let relationships = scoped_relationships(&fixture.relationships, scoped, &fixture.target);
    crate::c4_report::report(
        "/repo",
        &fixture.target,
        scoped,
        &relationships,
        &[],
        "fixture-fingerprint",
    )
    .content
}

#[test]
fn interactive_scope_renders_nested_first_hop_owners_without_neighbors() {
    let fixture = interacting_scope();
    let scoped = scoped_elements(&fixture.elements, &fixture.relationships, &fixture.target);
    let scoped_ids = scoped
        .iter()
        .map(|item| item.semantic_element_id.as_str())
        .collect::<std::collections::HashSet<_>>();

    for id in ["fs:h:lib/b.rs:file:b.rs:", "fs:h:lib/c.rs:file:c.rs:"] {
        assert!(scoped_ids.contains(id), "first-hop owner missing: {id}");
    }
    for id in [
        "fs:h:lib/b.rs:class:Owner:",
        "fs:h:lib/b.rs:method:outgoing:",
        "fs:h:lib/c.rs:class:Owner:",
        "fs:h:lib/c.rs:method:incoming:",
        "fs:h:lib/deep.rs:file:deep.rs:",
        "fs:h:lib/unrelated.rs:file:unrelated.rs:",
    ] {
        assert!(
            !scoped_ids.contains(id),
            "out-of-scope element included: {id}"
        );
    }

    let generation_targets = artifact_generation::missing_element_ids(&scoped, &[]);
    assert!(generation_targets.contains(&"fs:h:lib/b.rs:file:b.rs:".into()));
    assert!(generation_targets.contains(&"fs:h:lib/c.rs:file:c.rs:".into()));

    let report = rendered_report(&fixture, &scoped);
    assert!(report.contains("b.rs"), "{report}");
    assert!(report.contains("c.rs"));
    assert!(!report.contains("deep.rs"));
    assert!(!report.contains("unrelated.rs"));
    assert!(report.contains("c0 -->|calls x1| c1"));
    assert!(report.contains("c2 -->|calls x1| c0"));
}

#[test]
fn interaction_renders_when_the_internal_endpoint_is_cut_by_the_scope_budget() {
    let fixture = bounded_interacting_scope();
    let scoped = scoped_elements(&fixture.elements, &fixture.relationships, &fixture.target);
    let scoped_ids = scoped
        .iter()
        .map(|item| item.semantic_element_id.as_str())
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(scoped.len(), MAX_C4_ELEMENTS);
    assert!(!scoped_ids.contains("fs:h:src/a.rs:function:zzzz_caller:"));
    assert!(scoped_ids.contains("fs:h:lib/b.rs:file:b.rs:"));
    assert!(!scoped_ids.contains("fs:h:lib/b.rs:class:Owner:"));
    assert!(!scoped_ids.contains("fs:h:lib/b.rs:method:callee:"));

    let report = rendered_report(&fixture, &scoped);
    assert!(report.contains("b.rs"), "{report}");
    assert!(report.contains("c0 -->|calls x1| c1"));
}

#[test]
fn inline_module_keeps_same_file_sibling_calls_as_external() {
    let file = element("fs:h:f.rs:file:f.rs:", "file", "f.rs", "f.rs", None);
    let target = element(
        "fs:h:f.rs:module:tests:",
        "module",
        "tests",
        "f.rs",
        Some(&file.semantic_element_id),
    );
    let child = element(
        "fs:h:f.rs:function:a:",
        "function",
        "a",
        "f.rs",
        Some(&target.semantic_element_id),
    );
    let sibling = element(
        "fs:h:f.rs:function:b:",
        "function",
        "b",
        "f.rs",
        Some(&file.semantic_element_id),
    );
    let elements = vec![file.clone(), target.clone(), child.clone(), sibling];
    let external = one_hop_external_ids(
        &elements,
        &[relationship(
            &child.semantic_element_id,
            "fs:h:f.rs:function:b:",
        )],
        &[child.semantic_element_id.clone()].into_iter().collect(),
        &[child.semantic_element_id.clone()].into_iter().collect(),
        &target.semantic_element_id,
    );

    assert!(external.contains(&file.semantic_element_id));
}
