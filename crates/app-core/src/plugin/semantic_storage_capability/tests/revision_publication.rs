//! Exercises conditional writes through the actual compiled-plugin host capability.
use super::*;
mod controls;
use lumvise_db_core::{
    SemanticRevisionPublication, SemanticRevisionPublicationOutcome, SemanticRevisionStructure,
};

fn call(broker: &AppCoreHostCapabilityBroker, plugin: &str, input: Value) -> Value {
    broker.invoke(request(plugin, input)).unwrap()
}

fn observe(broker: &AppCoreHostCapabilityBroker, artifact_ids: Vec<String>) -> Value {
    call(
        broker,
        SEMANTIC_PLUGIN_ID,
        json!({
            "operation":"revision_selection_members", "query":{
                "project_root":"/repo", "selection_id":"working",
                "structure_coverage":{"kind":"project"}, "artifact_ids":artifact_ids
            }
        }),
    )
}

fn edit(
    broker: &AppCoreHostCapabilityBroker,
    operation_id: &str,
    membership: &Value,
    name: &str,
) -> SemanticRevisionPublication {
    let identity = call(
        broker,
        SEMANTIC_PLUGIN_ID,
        json!({"operation":"project_identity", "project_root":"/repo"}),
    );
    let head: Option<lumvise_db_core::SemanticRevisionSelectionObservation> =
        serde_json::from_value(membership["membership"].clone()).unwrap();
    let mut element: SemanticElement = serde_json::from_value(element()).unwrap();
    element.name = name.into();
    SemanticRevisionPublication {
        operation_id: operation_id.into(),
        project_root: "/repo".into(),
        project_id: identity["identity"]["project_id"].as_str().unwrap().into(),
        selection_id: "working".into(),
        source_context: "local".into(),
        producer_base: None,
        expected: head.map_or_else(Default::default, |head| head.expected),
        structure: Some(SemanticRevisionStructure {
            partition: None,
            elements: vec![element],
            relationships: vec![],
        }),
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    }
}

fn publish(broker: &AppCoreHostCapabilityBroker, edit: &SemanticRevisionPublication) -> Value {
    call(
        broker,
        SEMANTIC_PLUGIN_ID,
        json!({"operation":"publish_revisions", "publication":edit}),
    )
}

#[test]
fn conditional_host_publication_preserves_exact_bodies_and_replays_lost_acknowledgements() {
    let db = persistence();
    let broker = broker_for(db.clone());
    let first = edit(&broker, "first", &observe(&broker, vec![]), "first body");
    let accepted = publish(&broker, &first);
    assert!(matches!(
        serde_json::from_value::<SemanticRevisionPublicationOutcome>(
            accepted["publication"].clone()
        )
        .unwrap(),
        SemanticRevisionPublicationOutcome::Accepted(_)
    ));
    let original = active_project_elements(&db).remove(0);
    let next = edit(&broker, "next", &observe(&broker, vec![]), "second body");
    let result = publish(&broker, &next);
    assert_eq!(publish(&broker, &next), result);
    assert_eq!(publish(&broker, &first), accepted);
    let SemanticResult::Element(Some(retained)) = semantic_result(
        &db,
        SemanticOperation::Element {
            semantic_element_id: original.semantic_element_id.clone(),
        },
    ) else {
        panic!("expected exact first body")
    };
    assert_eq!(retained, original);
    let current = active_project_elements(&db).remove(0);
    assert_ne!(current.semantic_element_id, original.semantic_element_id);
    assert_eq!(current.name, "second body");
}

#[test]
fn conditional_host_publication_returns_a_conflict_for_a_stale_observed_scope() {
    let db = persistence();
    let broker = broker_for(db.clone());
    let base = observe(&broker, vec![]);
    let first = edit(&broker, "first", &base, "first body");
    let stale = edit(&broker, "stale", &base, "stale body");
    publish(&broker, &first);
    let result = publish(&broker, &stale);
    assert!(matches!(
        serde_json::from_value::<SemanticRevisionPublicationOutcome>(result["publication"].clone())
            .unwrap(),
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    assert_eq!(active_project_elements(&db).remove(0).name, "first body");
}

#[test]
fn source_family_reads_use_the_shared_selection_and_existing_plugin_permissions() {
    let db = persistence();
    let broker = broker_for(db.clone());
    publish(
        &broker,
        &edit(&broker, "first", &observe(&broker, vec![]), "first"),
    );
    let original = active_project_elements(&db).remove(0);
    publish(
        &broker,
        &edit(&broker, "next", &observe(&broker, vec![]), "next"),
    );
    let current = active_project_elements(&db).remove(0);
    let input = json!({"operation":"revision_families", "query":{"project_root":"/repo", "selection_id":"working",
        "records":[{"kind":"element", "record_id":original.semantic_element_id}]}});
    for plugin in [SEMANTIC_PLUGIN_ID, KNOWLEDGE_PLUGIN_ID] {
        assert_eq!(
            call(&broker, plugin, input.clone())["remaps"][0]["accepted"]["record_id"],
            current.semantic_element_id
        );
    }
    for plugin in ["third.party", "builtin.assistant", NUCLEUS_PLUGIN_ID] {
        assert!(
            broker.invoke(request(plugin, input.clone())).is_err(),
            "{plugin}"
        );
    }
}

#[test]
fn exact_revision_reads_keep_the_original_body_scope_and_plugin_permissions() {
    let db = persistence();
    let broker = broker_for(db.clone());
    publish(
        &broker,
        &edit(&broker, "first", &observe(&broker, vec![]), "first body"),
    );
    let original = active_project_elements(&db).remove(0);
    publish(
        &broker,
        &edit(&broker, "next", &observe(&broker, vec![]), "next body"),
    );
    let mut input = json!({"operation":"resolve_revision", "project_root":"/repo",
        "record":{"kind":"element", "record_id":original.semantic_element_id}});
    for plugin in [SEMANTIC_PLUGIN_ID, KNOWLEDGE_PLUGIN_ID] {
        let exact = call(&broker, plugin, input.clone());
        assert_eq!(exact["revision"]["body"]["state"], "element");
        assert_eq!(exact["revision"]["body"]["record"], json!(original));
    }
    for plugin in ["third.party", "builtin.assistant", NUCLEUS_PLUGIN_ID] {
        assert!(
            broker.invoke(request(plugin, input.clone())).is_err(),
            "{plugin}"
        );
    }
    input["project_root"] = json!("/other");
    assert_eq!(
        call(&broker, KNOWLEDGE_PLUGIN_ID, input)["revision"],
        Value::Null
    );
}

#[test]
fn an_exact_revision_read_reports_collection_without_selecting_its_successor() {
    let db = persistence();
    let broker = broker_for(db.clone());
    publish(
        &broker,
        &edit(&broker, "first", &observe(&broker, vec![]), "first body"),
    );
    let original = active_project_elements(&db).remove(0);
    publish(
        &broker,
        &edit(&broker, "next", &observe(&broker, vec![]), "next body"),
    );
    let policy = lumvise_db_core::SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) = semantic_result(
        &db,
        SemanticOperation::PlanRevisionRetention {
            project_root: "/repo".into(),
            operation_id: "collect-first".into(),
            policy: policy.clone(),
        },
    ) else {
        panic!("expected a dependency-safe collection plan");
    };
    semantic_result(
        &db,
        SemanticOperation::CollectRevisionRetention { plan, policy },
    );
    let exact = call(
        &broker,
        KNOWLEDGE_PLUGIN_ID,
        json!({"operation":"resolve_revision", "project_root":"/repo",
        "record":{"kind":"element", "record_id":original.semantic_element_id}}),
    );
    assert_eq!(exact["revision"]["body"]["state"], "pruned");
    assert_eq!(
        exact["revision"]["record"]["record_id"],
        original.semantic_element_id
    );
    assert!(
        !exact["revision"]["successors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn conditional_storage_keeps_existing_plugin_write_ownership() {
    let broker = broker_for(persistence());
    let edit = edit(&broker, "owned", &observe(&broker, vec![]), "owned body");
    for plugin in ["third.party", NUCLEUS_PLUGIN_ID, KNOWLEDGE_PLUGIN_ID] {
        assert!(
            broker
                .invoke(request(
                    plugin,
                    json!({"operation":"publish_revisions", "publication":edit})
                ))
                .is_err(),
            "{plugin}"
        );
    }
    for plugin in ["third.party", "builtin.assistant"] {
        assert!(
            broker
                .invoke(request(
                    plugin,
                    json!({"operation":"project_identity", "project_root":"/repo"})
                ))
                .is_err()
        );
        assert!(
            broker
                .invoke(request(
                    plugin,
                    json!({"operation":"revision_selection_members", "query":{
                        "project_root":"/repo", "selection_id":"working",
                        "structure_coverage":{"kind":"project"}, "artifact_ids":[]
                    }})
                ))
                .is_err()
        );
    }
}

#[test]
fn knowledge_conditional_artifacts_coalesce_working_bodies_and_refuse_stale_updates() {
    let db = persistence();
    let broker = broker_for(db.clone());
    let mut publication = edit(&broker, "source", &observe(&broker, vec![]), "owner");
    publish(&broker, &publication);
    publication.operation_id = "artifact-create".into();
    publication.structure = None;
    publication.artifacts = vec![lumvise_db_core::SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: lumvise_db_core::SemanticArtifact {
            artifact_id: "artifact".into(),
            semantic_element_id: "element-1".into(),
            artifact_kind: "specification".into(),
            title: "specification".into(),
            content_ref: None,
            content: Some("first body".into()),
            searchable_text: None,
            content_size_bytes: None,
            dependencies: vec![],
            metadata: json!({}),
        },
        media_type: "text/markdown".into(),
    }];
    let result = call(
        &broker,
        KNOWLEDGE_PLUGIN_ID,
        json!({"operation":"publish_revisions", "publication":publication}),
    );
    let SemanticRevisionPublicationOutcome::Accepted(receipt) =
        serde_json::from_value(result["publication"].clone()).unwrap()
    else {
        panic!("expected Knowledge artifact acceptance")
    };
    publication.operation_id = "artifact-update".into();
    publication.expected = receipt.accepted_scope;
    publication.artifacts[0].artifact.content = Some("second body".into());
    let accepted = call(
        &broker,
        KNOWLEDGE_PLUGIN_ID,
        json!({"operation":"publish_revisions", "publication":publication}),
    );
    assert_eq!(
        call(
            &broker,
            KNOWLEDGE_PLUGIN_ID,
            json!({"operation":"publish_revisions", "publication":publication})
        ),
        accepted
    );
    publication.operation_id = "stale-artifact".into();
    let stale = call(
        &broker,
        KNOWLEDGE_PLUGIN_ID,
        json!({"operation":"publish_revisions", "publication":publication}),
    );
    assert!(matches!(
        serde_json::from_value::<SemanticRevisionPublicationOutcome>(stale["publication"].clone())
            .unwrap(),
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    assert_eq!(
        artifact(&db, "artifact").content.as_deref(),
        Some("second body")
    );
}
