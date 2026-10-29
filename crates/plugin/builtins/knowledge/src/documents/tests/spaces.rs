use super::*;
use lumvise_contracts::{
    KnowledgeAccessContext, KnowledgePrincipalKind, KnowledgeProjectGrant,
    KnowledgeProjectIdentity, KnowledgeSpaceDescriptor, KnowledgeSpaceKind,
};

pub(super) fn fixture_scope(root: &str, kind: KnowledgeSpaceKind) -> KnowledgeAccessContext {
    KnowledgeAccessContext {
        principal_kind: match kind {
            KnowledgeSpaceKind::Assistant => KnowledgePrincipalKind::Assistant,
            KnowledgeSpaceKind::Agent => KnowledgePrincipalKind::Agent,
            KnowledgeSpaceKind::McpProject => KnowledgePrincipalKind::Mcp,
        },
        principal_id: "fixture-owner".into(),
        security_domain: "fixture-domain".into(),
        storage_authority: "fixture-store".into(),
        project_grants: Vec::new(),
        active_project: None,
        source_projects: Vec::new(),
        space: Some(KnowledgeSpaceDescriptor {
            space_id: format!("space:{root}"),
            space_kind: kind,
            project_root: root.into(),
            principal_id: "fixture-owner".into(),
            security_domain: "fixture-domain".into(),
            storage_authority: "fixture-store".into(),
            shared_project: None,
            revision: 1,
        }),
    }
}

#[test]
fn resolving_again_and_switching_conversation_keeps_one_space_and_its_documents() {
    let mut graph = FakeDocumentGraph::default();
    let scope = fixture_scope("assistant-space://profile", KnowledgeSpaceKind::Assistant);
    graph.memory_scope = Some(scope.clone());
    let first = graph
        .call("document.space", json!({"purpose":"self"}))
        .unwrap();
    let root = first["space"]["project_root"].as_str().unwrap();
    let saved = graph
        .call("document.create", draft("preferences", root))
        .unwrap();
    graph.memory_scope = None;
    assert!(
        graph
            .call("get_document", identity("preferences", root))
            .is_err()
    );
    graph.memory_scope = Some(scope);
    let second = graph
        .call("resolve_document_space", json!({"purpose":"self"}))
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        graph
            .call("document.get", identity("preferences", root))
            .unwrap(),
        saved
    );
    assert_eq!(
        graph
            .elements
            .values()
            .filter(|e| e["element_kind"] == "virtual_document_space")
            .count(),
        1
    );
    assert_eq!(graph.call("list_documents", json!({})).unwrap()["total"], 0);
}

#[test]
fn supplied_roots_and_owner_fields_do_not_establish_authority() {
    let mut graph = FakeDocumentGraph::default();
    assert!(
        graph
            .call(
                "resolve_document_space",
                json!({"purpose":"self", "principal_id":"other"})
            )
            .is_err()
    );
    graph.memory_scope = Some(fixture_scope(
        "assistant-space://mine",
        KnowledgeSpaceKind::Assistant,
    ));
    assert!(
        graph
            .call(
                "create_document",
                draft("secret", "assistant-space://other")
            )
            .is_err()
    );
    assert!(
        graph
            .call(
                "resolve_document_space",
                json!({"purpose":"current_project"})
            )
            .is_err()
    );
    assert_eq!(graph.writes, 0);
}

#[test]
fn revoked_project_memory_is_filtered_before_body_reads_but_independent_preferences_remain() {
    let mut graph = FakeDocumentGraph::default();
    let root = "assistant-space://profile";
    let mut scope = fixture_scope(root, KnowledgeSpaceKind::Assistant);
    graph.memory_scope = Some(scope.clone());
    graph
        .call("create_document", draft("preferences", root))
        .unwrap();
    let project = KnowledgeProjectIdentity {
        project_id: "project-a".into(),
        project_root: "/source".into(),
        storage_authority: "fixture-store".into(),
    };
    scope.project_grants.push(KnowledgeProjectGrant {
        project: project.clone(),
        writable: false,
    });
    scope.source_projects.push(project);
    graph.memory_scope = Some(scope.clone());
    let saved = graph
        .call("create_document", draft("decision", root))
        .unwrap();
    let edited = graph
        .call(
            "update_document",
            revision_request(&saved["document"], json!({"metadata":{}, "references":[]})),
        )
        .unwrap();
    assert_eq!(edited["document"]["access"], saved["document"]["access"]);
    scope.project_grants.clear();
    graph.memory_scope = Some(scope);
    let reads = graph.artifact_reads;
    assert!(
        graph
            .call("get_document", identity("decision", root))
            .is_err()
    );
    assert_eq!(
        graph.artifact_reads, reads,
        "denied metadata must not trigger a body read"
    );
    let found = graph
        .call(
            "search_documents",
            json!({"project_root":root, "query":"pipeline"}),
        )
        .unwrap();
    assert_eq!(found["total"], 1);
    assert_eq!(found["documents"][0]["document_id"], "preferences");
}

#[test]
fn resolving_an_agent_space_reuses_its_descriptor_and_preserves_documents() {
    let mut graph = FakeDocumentGraph::default();
    let root = "agent-space://research";
    graph.memory_scope = Some(fixture_scope(root, KnowledgeSpaceKind::Agent));
    let saved = graph.call("create_document", draft("rules", root)).unwrap();
    let writes = graph.writes;
    graph
        .call("resolve_document_space", json!({"purpose":"self"}))
        .unwrap();
    assert_eq!(graph.writes, writes);
    assert_eq!(
        graph
            .elements
            .values()
            .filter(|row| super::super::spaces::is_descriptor(row))
            .count(),
        1
    );
    assert_eq!(
        graph.call("get_document", identity("rules", root)).unwrap(),
        saved
    );
}

#[test]
fn moving_a_project_route_keeps_shared_memory_but_a_copied_lineage_does_not_grant_access() {
    let mut graph = FakeDocumentGraph::default();
    let root = "mcp-space://profile/source";
    let project = KnowledgeProjectIdentity {
        project_id: "lineage".into(),
        project_root: "/original".into(),
        storage_authority: "store".into(),
    };
    let mut scope = fixture_scope(root, KnowledgeSpaceKind::McpProject);
    scope.space.as_mut().unwrap().shared_project = Some(project.clone());
    scope.source_projects.push(project.clone());
    scope.project_grants.push(KnowledgeProjectGrant {
        project,
        writable: true,
    });
    graph.memory_scope = Some(scope.clone());
    let saved = graph
        .call("create_document", draft("decision", root))
        .unwrap();
    scope.project_grants[0].project.project_root = "/moved".into();
    scope.source_projects = vec![scope.project_grants[0].project.clone()];
    scope.space.as_mut().unwrap().shared_project = Some(scope.project_grants[0].project.clone());
    graph.memory_scope = Some(scope.clone());
    graph
        .call(
            "resolve_document_space",
            json!({"purpose":"current_project"}),
        )
        .unwrap();
    assert_eq!(
        graph
            .call("get_document", identity("decision", root))
            .unwrap(),
        saved
    );
    let updated = graph
        .call(
            "update_document",
            revision_request(&saved["document"], json!({"content":"After move"})),
        )
        .unwrap();
    assert_eq!(
        updated["document"]["access"]["required_projects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    scope.project_grants[0].project.storage_authority = "copied-store".into();
    graph.memory_scope = Some(scope);
    let reads = graph.artifact_reads;
    assert!(
        graph
            .call("get_document", identity("decision", root))
            .is_err()
    );
    assert_eq!(graph.artifact_reads, reads);
}

#[test]
fn pending_memory_write_cannot_drop_context_supplied_after_its_original_base() {
    let mut graph = FakeDocumentGraph::default();
    let root = "assistant-space://profile";
    let mut scope = fixture_scope(root, KnowledgeSpaceKind::Assistant);
    graph.memory_scope = Some(scope.clone());
    let saved = graph
        .call("create_document", draft("preferences", root))
        .unwrap();
    let request = revision_request(&saved["document"], json!({"content":"Independent draft"}));
    graph.fail_after("publication", 0);
    assert_eq!(
        graph
            .call("update_document", request.clone())
            .unwrap_err()
            .code,
        INJECTED
    );
    let project = KnowledgeProjectIdentity {
        project_id: "source-id".into(),
        project_root: "/source".into(),
        storage_authority: "fixture-store".into(),
    };
    scope.source_projects.push(project.clone());
    scope.project_grants.push(KnowledgeProjectGrant {
        project,
        writable: false,
    });
    graph.memory_scope = Some(scope);
    assert_eq!(
        graph.call("update_document", request).unwrap_err().code,
        "knowledge_space_denied"
    );
    assert_eq!(
        graph
            .call("get_document", identity("preferences", root))
            .unwrap(),
        saved
    );
}
