//! Wire authorization uses real persistence so denied calls cannot hide accidental writes.
use super::*;

const BEGIN: &str = "begin_element_text_vector_generation";
const STAGE: &str = "stage_element_text_vector_chunk";
const COMMIT: &str = "commit_element_text_vector_generation";
const ABORT: &str = "abort_element_text_vector_generation";
const OPERATIONS: [&str; 4] = [BEGIN, STAGE, COMMIT, ABORT];

fn generation_key() -> Value {
    json!({"project_root":"/repo","semantic_element_id":"section","generation_id":"generation"})
}

fn generation_wire(operation: &str) -> Value {
    match operation {
        BEGIN => json!({"operation":BEGIN,"spec":{"key":generation_key(),
            "content_fingerprint":"fp1:000000000000000a:000000000000000a","engine_id":"fixture",
            "model":"fixture-model","profile_fingerprint":"profile","dimensions":2,
            "chunk_count":1,"chunk_plan_fingerprint":"plan"}}),
        STAGE => json!({"operation":STAGE,"key":generation_key(),"chunk":0,"vector":[1.0,0.0]}),
        operation => json!({"operation":operation,"key":generation_key()}),
    }
}

struct GenerationWireFixture {
    persistence: Arc<LocalPersistence>,
    broker: AppCoreHostCapabilityBroker,
}

impl GenerationWireFixture {
    fn new(operation: &str) -> Self {
        let persistence = persistence();
        let broker = broker_for(persistence.clone());
        let mut section = element_for("/repo", "section");
        section["element_kind"] = json!("html_section");
        section["content_fingerprint"] = json!("fp1:000000000000000a:000000000000000a");
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation":"sync_structure",
            "project_root":"/repo","elements":[section],"relationships":[]}),
            ))
            .unwrap();
        let fixture = Self {
            persistence,
            broker,
        };
        fixture.prepare(operation);
        fixture
    }

    fn prepare(&self, operation: &str) {
        if operation == BEGIN {
            return;
        }
        self.call(SEMANTIC_PLUGIN_ID, generation_wire(BEGIN))
            .unwrap();
        if operation == COMMIT {
            self.call(SEMANTIC_PLUGIN_ID, generation_wire(STAGE))
                .unwrap();
        }
    }

    fn call(
        &self,
        plugin: &str,
        input: Value,
    ) -> Result<Value, lumvise_plugin_runtime::HostCapabilityError> {
        self.broker.invoke(request(plugin, input))
    }

    fn revision(&self) -> i64 {
        match semantic_result(&self.persistence, SemanticOperation::SemanticRevision) {
            SemanticResult::SemanticRevision { commit_version } => commit_version,
            other => panic!("{other:?}; expected revision"),
        }
    }

    fn reject_without_publication(&self, plugin: &str, input: Value) -> String {
        let before = self.revision();
        let error = self.call(plugin, input).unwrap_err().to_string();
        assert_eq!(
            self.revision(),
            before,
            "denied generation request published a write"
        );
        error
    }
}

#[test]
fn generation_wire_writes_require_semantic_identity_without_mutating_on_denial() {
    for operation in OPERATIONS {
        let fixture = GenerationWireFixture::new(operation);
        for plugin in [KNOWLEDGE_PLUGIN_ID, NUCLEUS_PLUGIN_ID, "third.party"] {
            let error = fixture.reject_without_publication(plugin, generation_wire(operation));
            assert!(
                error.contains("cannot perform semantic storage"),
                "{operation}, {plugin}: {error}"
            );
        }
        let result = fixture
            .call(SEMANTIC_PLUGIN_ID, generation_wire(operation))
            .unwrap();
        assert_eq!(result["key"], generation_key());
        let expected = match operation {
            COMMIT => "committed",
            ABORT => "aborted",
            _ => "open",
        };
        assert_eq!(result["state"], expected);
        assert_eq!(
            result["published_count"],
            if operation == COMMIT { 1 } else { 0 }
        );
    }
}

#[test]
fn generation_wire_rejects_unknown_outer_and_nested_fields_before_publication() {
    for operation in OPERATIONS {
        let fixture = GenerationWireFixture::new(operation);
        let mut outer = generation_wire(operation);
        outer["unexpected"] = json!(true);
        let error = fixture.reject_without_publication(SEMANTIC_PLUGIN_ID, outer);
        assert!(error.contains("unknown field"), "{error}");
        let mut nested = generation_wire(operation);
        if operation == BEGIN {
            nested["spec"]["unexpected"] = json!(true);
        } else {
            nested["key"]["unexpected"] = json!(true);
        }
        let error = fixture.reject_without_publication(SEMANTIC_PLUGIN_ID, nested);
        assert!(error.contains("unknown field"), "{error}");
    }
    let fixture = GenerationWireFixture::new(BEGIN);
    let mut nested_key = generation_wire(BEGIN);
    nested_key["spec"]["key"]["unexpected"] = json!(true);
    let error = fixture.reject_without_publication(SEMANTIC_PLUGIN_ID, nested_key);
    assert!(error.contains("unknown field"), "{error}");
}

#[test]
fn generation_wire_rejects_missing_fields_bad_dimensions_and_foreign_identity() {
    for operation in OPERATIONS {
        let fixture = GenerationWireFixture::new(operation);
        let mut incomplete = generation_wire(operation);
        incomplete
            .as_object_mut()
            .unwrap()
            .remove(if operation == BEGIN { "spec" } else { "key" });
        assert!(
            fixture
                .reject_without_publication(SEMANTIC_PLUGIN_ID, incomplete)
                .contains("missing field")
        );
        let mut foreign = generation_wire(operation);
        let key = if operation == BEGIN {
            &mut foreign["spec"]["key"]
        } else {
            &mut foreign["key"]
        };
        key["project_root"] = json!(if operation == ABORT { "" } else { "/foreign" });
        assert!(
            !fixture
                .reject_without_publication(SEMANTIC_PLUGIN_ID, foreign)
                .is_empty()
        );
    }
    let fixture = GenerationWireFixture::new(STAGE);
    let mut bad_dimension = generation_wire(STAGE);
    bad_dimension["vector"] = json!([1.0]);
    let error = fixture.reject_without_publication(SEMANTIC_PLUGIN_ID, bad_dimension);
    assert!(
        error.contains("1 dimensions") && error.contains("2 finite dimensions"),
        "{error}"
    );
    let mut bad_key = generation_wire(STAGE);
    bad_key["key"]["generation_id"] = json!("unknown-generation");
    let error = fixture.reject_without_publication(SEMANTIC_PLUGIN_ID, bad_key);
    assert!(
        error.contains("unknown-generation") && error.contains("existing generation key"),
        "{error}"
    );
}
