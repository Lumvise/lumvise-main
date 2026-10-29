use super::*;

fn pin(
    broker: &AppCoreHostCapabilityBroker,
    record: &str,
    operation: &str,
    generation: i64,
) -> Value {
    let identity = call(
        broker,
        SEMANTIC_PLUGIN_ID,
        json!({"operation":"project_identity", "project_root":"/repo"}),
    );
    json!({"operation":"select_revisions", "update":{
        "operation_id":operation, "project_root":"/repo", "project_id":identity["identity"]["project_id"],
        "selection_id":"review", "source_context":"local", "expected_generation":generation,
        "artifact_edits":{}, "mode":"pinned", "records":[{"kind":"element", "record_id":record}],
        "connection_ids":[], "source_anchor":null
    }})
}

fn preview(broker: &AppCoreHostCapabilityBroker, operation: &str) -> Value {
    call(
        broker,
        SEMANTIC_PLUGIN_ID,
        json!({"operation":"plan_revision_retention", "project_root":"/repo",
        "operation_id":operation, "policy":{"mode":"revision_count", "revisions":1}}),
    )["plan"]
        .clone()
}

#[test]
fn source_controls_pin_release_and_collect_with_durable_acknowledgements() {
    let db = persistence();
    let broker = broker_for(db.clone());
    publish(
        &broker,
        &edit(&broker, "first", &observe(&broker, vec![]), "first"),
    );
    let original = active_project_elements(&db).remove(0).semantic_element_id;
    let selection = call(
        &broker,
        SEMANTIC_PLUGIN_ID,
        json!({"operation":"revision_selection", "project_root":"/repo", "selection_id":"working"}),
    );
    assert_eq!(
        selection["selection"]["elements"][0]["semantic_element_id"],
        original
    );
    let pinned = pin(&broker, &original, "protect", 0);
    let receipt = call(&broker, SEMANTIC_PLUGIN_ID, pinned.clone());
    assert_eq!(receipt["publication"]["outcome"], "accepted");
    publish(
        &broker,
        &edit(&broker, "next", &observe(&broker, vec![]), "next"),
    );
    assert_eq!(preview(&broker, "protected")["records"], json!([]));
    let mut released = pin(&broker, &original, "release", 1);
    released["update"]["records"] = json!([]);
    assert_eq!(
        call(&broker, SEMANTIC_PLUGIN_ID, released)["publication"]["outcome"],
        "accepted"
    );
    let plan = preview(&broker, "collect");
    assert_eq!(
        plan["records"],
        json!([{"kind":"element", "record_id":original}])
    );
    let collection = json!({"operation":"collect_revision_retention", "plan":plan,
        "policy":{"mode":"revision_count", "revisions":1}});
    let collected = call(&broker, SEMANTIC_PLUGIN_ID, collection.clone());
    assert_eq!(call(&broker, SEMANTIC_PLUGIN_ID, collection), collected);
    assert_eq!(call(&broker, SEMANTIC_PLUGIN_ID, pinned), receipt);
    let exact = call(
        &broker,
        SEMANTIC_PLUGIN_ID,
        json!({"operation":"resolve_revision", "project_root":"/repo", "record":{"kind":"element", "record_id":original}}),
    );
    assert_eq!(exact["revision"]["body"]["state"], "pruned");
}

#[test]
fn newer_pins_invalidate_cleanup_and_other_plugins_cannot_mutate_source_controls() {
    let db = persistence();
    let broker = broker_for(db.clone());
    publish(
        &broker,
        &edit(&broker, "first", &observe(&broker, vec![]), "first"),
    );
    let original = active_project_elements(&db).remove(0).semantic_element_id;
    publish(
        &broker,
        &edit(&broker, "next", &observe(&broker, vec![]), "next"),
    );
    let plan = preview(&broker, "old-plan");
    let pinned = pin(&broker, &original, "protect", 0);
    call(&broker, SEMANTIC_PLUGIN_ID, pinned.clone());
    let stale = pin(&broker, &original, "stale-pin", 0);
    assert_eq!(
        call(&broker, SEMANTIC_PLUGIN_ID, stale)["publication"]["outcome"],
        "conflict"
    );
    let collection = json!({"operation":"collect_revision_retention", "plan":plan, "policy":{"mode":"revision_count", "revisions":1}});
    assert!(
        broker
            .invoke(request(SEMANTIC_PLUGIN_ID, collection.clone()))
            .is_err()
    );
    let read =
        json!({"operation":"revision_selection", "project_root":"/repo", "selection_id":"review"});
    let preview = json!({"operation":"plan_revision_retention", "project_root":"/repo", "operation_id":"preview", "policy":{"mode":"revision_count", "revisions":1}});
    for input in [pinned, collection, read, preview] {
        for plugin in [KNOWLEDGE_PLUGIN_ID, "builtin.assistant", "third.party"] {
            assert!(
                broker.invoke(request(plugin, input.clone())).is_err(),
                "{plugin}: {input}"
            );
        }
    }
}
