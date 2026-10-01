use super::*;
use crate::AppCore;
use lumvise_db_core::{
    ArtifactTextVector, LocalPersistence, RelationalPersistence, Result as DbResult,
    SemanticPersistence,
};
use std::sync::Arc;

fn request(plugin_id: &str, capability_id: &str, input: Value) -> HostCapabilityRequest {
    HostCapabilityRequest {
        plugin_id: plugin_id.into(),
        invocation_id: "invocation-1".into(),
        call_id: "call-1".into(),
        capability_id: capability_id.into(),
        required_version: "1".into(),
        input,
    }
}

fn app_with_default_broker() -> AppCore {
    AppCore::in_memory().expect("in-memory App Core")
}

fn broker_with_local_persistence() -> AppCoreHostCapabilityBroker {
    let persistence = Arc::new(LocalPersistence::in_memory().expect("selected local persistence"));
    let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn RelationalPersistence> = persistence;
    AppCoreHostCapabilityBroker::new(semantic, relational)
}

#[test]
fn default_broker_and_app_commands_share_one_snapshot_service() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    assert!(broker.shares_semantic_snapshot_service(&app.semantic_snapshots));
}

#[test]
fn invoke_denies_unknown_capability() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    let error = broker
        .invoke(request("plugin.a", "storage.other", json!({})))
        .expect_err("unknown capability must be denied");

    assert!(error.to_string().contains("unknown_host_capability"));
}

#[test]
fn invoke_put_get_and_list_round_trip_neutral_rows() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    broker
            .invoke(request(
                "plugin.a",
                PLUGIN_STORAGE_CAPABILITY,
                json!({"operation": "ensure_table", "table_name": "notes", "schema": {"type": "object"}}),
            ))
            .expect("ensure plugin table");
    broker
            .invoke(request(
                "plugin.a",
                PLUGIN_STORAGE_CAPABILITY,
                json!({"operation": "put_row", "table_name": "notes", "row_key": "first", "value": {"title": "One"}}),
            ))
            .expect("put plugin row");

    let get = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "get_row", "table_name": "notes", "row_key": "first"}),
        ))
        .expect("get plugin row");
    let list = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "list_rows", "table_name": "notes"}),
        ))
        .expect("list plugin rows");

    assert_eq!(
        get,
        json!({"row": {"row_key": "first", "value": {"title": "One"}}})
    );
    assert_eq!(
        list,
        json!({"rows": [{"row_key": "first", "value": {"title": "One"}}], "next_after_key": null})
    );
}

#[test]
fn invoke_isolates_rows_by_request_plugin_identity() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    for plugin_id in ["plugin.a", "plugin.b"] {
        broker
            .invoke(request(
                plugin_id,
                PLUGIN_STORAGE_CAPABILITY,
                json!({"operation": "ensure_table", "table_name": "notes", "schema": {}}),
            ))
            .expect("ensure isolated plugin table");
    }
    broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "put_row", "table_name": "notes", "row_key": "secret", "value": 7}),
        ))
        .expect("put plugin A row");

    let plugin_b = broker
        .invoke(request(
            "plugin.b",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "list_rows", "table_name": "notes"}),
        ))
        .expect("list plugin B rows");

    assert_eq!(plugin_b, json!({"rows": [], "next_after_key": null}));
}

#[test]
fn invoke_lists_literal_prefix_in_bounded_cursor_pages() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "ensure_table", "table_name": "records", "schema": {}}),
        ))
        .expect("table");
    for key in ["element:a", "element:b", "element:c", "relation:a"] {
        broker
                .invoke(request(
                    "plugin.a",
                    PLUGIN_STORAGE_CAPABILITY,
                    json!({"operation": "put_row", "table_name": "records", "row_key": key, "value": key}),
                ))
                .expect("row");
    }

    let first = broker
            .invoke(request(
                "plugin.a",
                PLUGIN_STORAGE_CAPABILITY,
                json!({"operation": "list_rows", "table_name": "records", "key_prefix": "element:", "limit": 2}),
            ))
            .expect("first page");
    let second = broker
            .invoke(request(
                "plugin.a",
                PLUGIN_STORAGE_CAPABILITY,
                json!({"operation": "list_rows", "table_name": "records", "key_prefix": "element:", "after_key": first["next_after_key"], "limit": 2}),
            ))
            .expect("second page");

    assert_eq!(first["rows"].as_array().expect("rows").len(), 2);
    assert_eq!(first["next_after_key"], "element:b");
    assert_eq!(second["rows"][0]["row_key"], "element:c");
    assert!(second["next_after_key"].is_null());
}

#[test]
fn invoke_rejects_unbounded_row_page_limit_as_invalid_input() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");

    let error = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "list_rows", "table_name": "records", "limit": 1001}),
        ))
        .expect_err("unbounded limit rejected");

    assert!(error.to_string().contains("invalid_host_capability_input"));
    assert!(error.to_string().contains("1 through 1000"));
}

#[test]
fn invoke_rejects_input_plugin_id_override() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    let error = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "list_rows", "table_name": "notes", "plugin_id": "plugin.b"}),
        ))
        .expect_err("input plugin identity must be rejected");

    assert!(error.to_string().contains("plugin_id"));
}

#[test]
fn staged_mutations_commit_atomically_across_tables_for_owner() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().expect("broker");
    for table in ["elements", "relationships"] {
        broker
            .invoke(request(
                "plugin.a",
                PLUGIN_STORAGE_CAPABILITY,
                json!({"operation": "ensure_table", "table_name": table, "schema": {}}),
            ))
            .expect("table");
    }
    let begin = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "begin_mutations"}),
        ))
        .expect("begin");
    let transaction_id = begin["transaction_id"].clone();
    broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({
                "operation": "stage_mutations", "transaction_id": transaction_id,
                "mutations": [
                    {"operation": "put", "table_name": "elements", "row_key": "e1", "value": 1},
                    {"operation": "put", "table_name": "relationships", "row_key": "r1", "value": 2}
                ]
            }),
        ))
        .expect("stage");
    let cross_plugin = broker
        .invoke(request(
            "plugin.b",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "commit_mutations", "transaction_id": transaction_id}),
        ))
        .expect_err("other plugin cannot commit");
    assert!(
        cross_plugin
            .to_string()
            .contains("owned by invoking plugin")
    );
    broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "commit_mutations", "transaction_id": transaction_id}),
        ))
        .expect("commit");
    let row = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "get_row", "table_name": "relationships", "row_key": "r1"}),
        ))
        .expect("read committed row");
    assert_eq!(row["row"]["value"], 2);
}

#[test]
fn oversized_stage_is_rejected_before_staging() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().expect("broker");
    let begin = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "begin_mutations"}),
        ))
        .expect("begin");
    let mutations = (0..=MAX_STAGED_CHANGES)
        .map(|index| {
            json!({
                "operation": "delete", "table_name": "records", "row_key": format!("row-{index}")
            })
        })
        .collect::<Vec<_>>();
    let error = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({
                "operation": "stage_mutations", "transaction_id": begin["transaction_id"],
                "mutations": mutations
            }),
        ))
        .expect_err("oversized stage");
    assert!(error.to_string().contains("host_capability_quota_exceeded"));
}

#[test]
fn begin_rejects_global_active_transaction_quota() {
    let broker = broker_with_local_persistence();
    let mut transactions = broker.storage_transactions.lock().expect("transactions");
    for index in 0..MAX_GLOBAL_STORAGE_TRANSACTIONS {
        insert_staged_transaction(&mut transactions, &format!("plugin-{index}"), index, 0);
    }
    drop(transactions);

    let error = broker
        .invoke(request(
            "plugin-overflow",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "begin_mutations"}),
        ))
        .expect_err("global active quota");

    assert!(
        error
            .to_string()
            .contains("global active transaction count")
    );
}

#[test]
fn stage_discards_current_transaction_when_per_plugin_aggregate_exceeds_quota() {
    let broker = broker_with_local_persistence();
    let begin = broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "begin_mutations"}),
        ))
        .expect("begin");
    let transaction_id = begin["transaction_id"].as_str().expect("id").to_owned();
    insert_staged_transaction(
        &mut broker.storage_transactions.lock().expect("transactions"),
        "plugin.a",
        99,
        MAX_PLUGIN_DATA_MUTATION_BYTES,
    );

    broker
        .invoke(request(
            "plugin.a",
            PLUGIN_STORAGE_CAPABILITY,
            json!({
                "operation": "stage_mutations", "transaction_id": transaction_id,
                "mutations": [{"operation": "delete", "table_name": "records", "row_key": "one"}]
            }),
        ))
        .expect_err("aggregate bytes quota");

    assert!(
        !broker
            .storage_transactions
            .lock()
            .expect("transactions")
            .contains_key(&transaction_id)
    );
}

#[test]
fn stage_discards_current_transaction_when_cross_plugin_global_bytes_exceed_quota() {
    let broker = broker_with_local_persistence();
    let begin = broker
        .invoke(request(
            "plugin.current",
            PLUGIN_STORAGE_CAPABILITY,
            json!({"operation": "begin_mutations"}),
        ))
        .expect("begin");
    let transaction_id = begin["transaction_id"].as_str().expect("id").to_owned();
    let mut transactions = broker.storage_transactions.lock().expect("transactions");
    for index in 0..4 {
        insert_staged_transaction(
            &mut transactions,
            &format!("plugin-{index}"),
            index,
            MAX_PLUGIN_DATA_MUTATION_BYTES,
        );
    }
    drop(transactions);

    let error = broker
        .invoke(request(
            "plugin.current",
            PLUGIN_STORAGE_CAPABILITY,
            json!({
                "operation": "stage_mutations", "transaction_id": transaction_id,
                "mutations": [{"operation": "delete", "table_name": "records", "row_key": "one"}]
            }),
        ))
        .expect_err("global staged byte quota");

    assert!(error.to_string().contains("global staged bytes"));
}

fn insert_staged_transaction(
    transactions: &mut HashMap<String, StagedStorageTransaction>,
    plugin_id: &str,
    index: usize,
    serialized_bytes: usize,
) {
    transactions.insert(
        format!("fixture-{plugin_id}-{index}"),
        StagedStorageTransaction {
            plugin_id: plugin_id.into(),
            created_at: Instant::now(),
            mutations: Vec::new(),
            serialized_bytes,
        },
    );
}

#[test]
fn invoke_embeds_bounded_text_batch_in_order() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(NamedLengthVectorizer))
        .expect("install shared vectorizer");

    let output = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts": ["a", "four"]}),
        ))
        .expect("embed batch");

    assert_eq!(
        output,
        json!({"vectors": [[1.0, 1.0], [4.0, 1.0]], "engine_id": "test-vectorizer", "model": "deterministic"})
    );
}

#[test]
fn invoke_reports_embedder_absence_as_retryable_unavailable() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");

    let error = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts": ["query"]}),
        ))
        .expect_err("missing vectorizer");

    assert!(error.to_string().contains("host_capability_unavailable"));
}

#[test]
fn invoke_rejects_embed_batches_over_quota() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    let texts = (0..=MAX_EMBED_TEXTS).map(|_| "x").collect::<Vec<_>>();

    let error = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts": texts}),
        ))
        .expect_err("oversized batch");

    assert!(error.to_string().contains("invalid_host_capability_input"));
    assert!(error.to_string().contains(&MAX_EMBED_TEXTS.to_string()));
}

#[test]
fn invoke_rejects_inconsistent_vector_dimensions() {
    let app = app_with_default_broker();
    let broker = app
        .default_plugin_host_capability_broker()
        .expect("default Plugin System broker");
    *broker.vectorizer.lock().expect("vectorizer lock") = Some(Arc::new(InconsistentVectorizer));

    let error = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts": ["a", "bb"]}),
        ))
        .expect_err("inconsistent dimensions");

    assert!(error.to_string().contains("invalid vectorizer output"));
}

struct NamedLengthVectorizer;

impl ArtifactTextVectorizer for NamedLengthVectorizer {
    fn vectorize_artifact_text(&self, text: &str) -> DbResult<ArtifactTextVector> {
        Ok(test_vector(vec![text.len() as f32, 1.0]))
    }
}

struct InconsistentVectorizer;

impl ArtifactTextVectorizer for InconsistentVectorizer {
    fn vectorize_artifact_text(&self, text: &str) -> DbResult<ArtifactTextVector> {
        Ok(test_vector(vec![1.0; text.len()]))
    }
}

fn test_vector(vector: Vec<f32>) -> ArtifactTextVector {
    ArtifactTextVector {
        engine_id: "test-vectorizer".into(),
        model: Some("deterministic".into()),
        dimensions: vector.len(),
        vector,
        normalized: false,
        metadata: json!({}),
    }
}
