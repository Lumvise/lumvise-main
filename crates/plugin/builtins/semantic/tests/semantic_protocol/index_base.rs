//! Producers explicitly capture scoped revision facts through Semantic's public read boundary.
use super::*;
use lumvise_contracts::{SemanticIndexObservation, SemanticRecordRef};
use lumvise_plugin_package::ExportSurface;
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest, PluginSystem};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::BTreeSet;

struct IndexBaseFixture {
    runtime: PluginSystem,
    broker: Arc<MemoryCapabilityBroker>,
    _workspace: tempfile::TempDir,
}

impl IndexBaseFixture {
    fn new() -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[128; 32]));
        let broker = Arc::new(MemoryCapabilityBroker::default());
        let runtime = system(broker.clone());
        runtime.install(&installed).unwrap();
        runtime.start(PLUGIN_ID).unwrap();
        Self {
            runtime,
            broker,
            _workspace: workspace,
        }
    }

    fn seed_sources(&self) {
        for batch in [index_batch(), foreign_batch()] {
            assert_eq!(
                success(
                    self.runtime
                        .invoke(
                            PLUGIN_ID,
                            INGEST_EXPORT_ID,
                            self.broker.observe_fixture(batch)
                        )
                        .unwrap()
                )["accepted"],
                true
            );
        }
        self.broker.reset_project_snapshot_calls();
        self.broker.reset_renderer_graph_calls();
    }

    fn observe(&self, scope: Value) -> Value {
        success(
            self.runtime
                .invoke(PLUGIN_ID, "observe_index_base", scope)
                .unwrap(),
        )
    }

    fn reject_empty_field(&self, field: &str, value: Value) {
        let mut scope = json!({"project_root":"/work/demo", "provider_instance_id":"files", "paths":[], "artifact_ids":[]});
        scope[field] = value;
        let error = failure(
            self.runtime
                .invoke(PLUGIN_ID, "observe_index_base", scope)
                .unwrap(),
        );
        assert_eq!(error.code, "invalid_semantic_input");
        assert!(error.message.contains("expected"));
    }
}

impl Drop for IndexBaseFixture {
    fn drop(&mut self) {
        self.runtime
            .stop(PLUGIN_ID)
            .expect("stop isolated Semantic fixture");
    }
}

struct NativeIndexBaseHost<'a>(&'a MemoryCapabilityBroker);

impl HostCallTransport for NativeIndexBaseHost<'_> {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.0
            .invoke(HostCapabilityRequest {
                plugin_id: PLUGIN_ID.into(),
                invocation_id: "native-index-base".into(),
                call_id: "native-base-read".into(),
                capability_id: capability.into(),
                required_version: "^1.7.0".into(),
                input,
            })
            .map_err(|error| PluginError::new("native_index_base_host", error.to_string(), false))
    }
}

#[test]
fn native_index_base_matches_the_signed_read_through_real_host_storage() {
    let fixture = IndexBaseFixture::new();
    fixture.seed_sources();
    let scope = json!({"project_root":"/work/demo", "provider_instance_id":"rust-indexer",
        "paths":["src/parser.rs"], "artifact_ids":["foreign-note"]});
    let signed = fixture.observe(scope.clone());
    let mut host = NativeIndexBaseHost(&fixture.broker);
    let native = lumvise_plugin_semantic::SemanticPlugin
        .dispatch(
            "observe_index_base",
            scope,
            &mut PluginContext::for_test(&mut host),
        )
        .unwrap();
    assert_eq!(native, signed);
}

#[test]
fn signed_index_base_contains_owned_paths_and_explicit_artifacts_without_graph_bodies() {
    let fixture = IndexBaseFixture::new();
    fixture.seed_sources();
    let base = fixture.observe(
        json!({"project_root":"/work/demo", "provider_instance_id":"rust-indexer",
        "paths":["src/parser.rs"], "artifact_ids":["foreign-note"]}),
    );
    assert_owned_scope(serde_json::from_value(base).unwrap());
    assert_eq!(
        (
            fixture.broker.project_snapshot_calls(),
            fixture.broker.renderer_graph_calls()
        ),
        (0, 0)
    );
}

fn assert_owned_scope(base: SemanticIndexObservation) {
    let expected = BTreeSet::from([
        SemanticRecordRef::artifact("foreign-note"),
        SemanticRecordRef::element("file:parser"),
        SemanticRecordRef::element("fn:parse"),
    ]);
    assert_eq!(
        base.expected.records.into_iter().collect::<BTreeSet<_>>(),
        expected
    );
    assert!(!base.expected.connection_ids.is_empty());
    assert_eq!(base.selection_id, "working");
    assert_eq!(base.source_context, "local");
    assert!(base.producer_base.is_none());
}

#[test]
fn signed_index_base_rejects_empty_scope_fields_before_native_reads() {
    let fixture = IndexBaseFixture::new();
    for (field, value) in [
        ("project_root", json!("")),
        ("provider_instance_id", json!(" ")),
        ("paths", json!([""])),
        ("artifact_ids", json!([" "])),
    ] {
        fixture.reject_empty_field(field, value);
    }
    assert_eq!(
        fixture.broker.semantic_operation_count("project_identity"),
        0
    );
    assert_eq!(
        fixture
            .broker
            .semantic_operation_count("revision_selection_members"),
        0
    );
}

fn foreign_batch() -> Value {
    json!({"project_root":"/work/demo", "provider_instance_id":"foreign",
        "semantic_sources":[{"semantic_source_id":"foreign", "kind":"repository",
            "name":"Foreign", "root_uri":"file:///work/demo"}],
        "semantic_elements":[{"semantic_source_id":"foreign", "semantic_element_id":"foreign-file",
            "path":"src/parser.rs", "semantic_element_type":"file", "semantic_element_name":"Foreign"}],
        "semantic_relationships":[], "semantic_artifacts":[{"artifact_id":"foreign-note",
            "semantic_element_id":"foreign-file", "artifact_kind":"annotation", "title":"Note", "content":"Exact body"}]})
}

#[test]
fn base_capture_uses_the_existing_global_read_boundary() {
    let manifest = package_manifest_source("test-target", &"a".repeat(64));
    let export = manifest
        .exports
        .iter()
        .find(|export| export.id == "observe_index_base")
        .expect("explicit producer base read");
    assert_eq!(export.surface, ExportSurface::McpTool);
}

fn family_scope() -> Value {
    json!({"project_root":"/work/demo","selection_id":"working",
        "records":[{"kind":"element","record_id":"fn:parse"},
            {"kind":"artifact","record_id":"foreign-note"},
            {"kind":"element","record_id":"not-indexed"}]})
}

fn resolve_families(fixture: &IndexBaseFixture, scope: Value) -> Value {
    success(
        fixture
            .runtime
            .invoke(PLUGIN_ID, "resolve_revision_families", scope)
            .unwrap(),
    )
}

fn advance_parser(fixture: &IndexBaseFixture) -> String {
    let mut batch = index_batch();
    batch["semantic_elements"][1]["semantic_element_name"] = json!("updated parser");
    let accepted = success(
        fixture
            .runtime
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                fixture.broker.observe_fixture(batch),
            )
            .unwrap(),
    );
    accepted_element_id(&accepted, "fn:parse")
}

#[test]
fn signed_family_reads_resolve_successors_without_hydrating_or_rewriting_the_graph() {
    let fixture = IndexBaseFixture::new();
    fixture.seed_sources();
    let current = advance_parser(&fixture);
    assert_ne!(current, "fn:parse");
    fixture.broker.reset_project_snapshot_calls();
    let signed = resolve_families(&fixture, family_scope());
    let mut host = NativeIndexBaseHost(&fixture.broker);
    let native = lumvise_plugin_semantic::SemanticPlugin
        .dispatch(
            "resolve_revision_families",
            family_scope(),
            &mut PluginContext::for_test(&mut host),
        )
        .unwrap();
    assert_eq!(native, signed);
    let remaps: lumvise_contracts::SemanticRevisionFamilyRead =
        serde_json::from_value(signed).unwrap();
    assert_eq!(remaps.remaps.len(), 2);
    assert!(remaps.remaps.iter().any(|remap| remap.incoming
        == SemanticRecordRef::element("fn:parse")
        && remap.accepted == SemanticRecordRef::element(&current)));
    assert!(remaps.remaps.iter().any(|remap| remap.incoming
        == SemanticRecordRef::artifact("foreign-note")
        && remap.accepted == remap.incoming));
    assert_eq!(fixture.broker.project_snapshot_calls(), 0);
}

#[test]
fn signed_family_reads_refuse_a_changed_commit_and_do_not_cross_project_scope() {
    let fixture = IndexBaseFixture::new();
    fixture.seed_sources();
    let before = resolve_families(&fixture, family_scope());
    advance_parser(&fixture);
    let mut stale = family_scope();
    stale["expected_commit_version"] = before["commit_version"].clone();
    stale["records"] = json!([]);
    let error = failure(
        fixture
            .runtime
            .invoke(PLUGIN_ID, "resolve_revision_families", stale)
            .unwrap(),
    );
    assert!(error.message.contains("captured read commit"), "{error:?}");
    let mut foreign = family_scope();
    foreign["project_root"] = json!("/unrelated");
    assert_eq!(resolve_families(&fixture, foreign)["remaps"], json!([]));
}
