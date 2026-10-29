//! Real byte-boundary conformance, including native/WebAssembly parity without JavaScript number conversion.
#[path = "policy_bytes/archives.rs"]
mod archives;
#[path = "policy_bytes/artifact_edits.rs"]
mod artifact_edits;
#[path = "policy_bytes/attachment_retention.rs"]
mod attachment_retention;
#[path = "policy_bytes/binary_content.rs"]
mod binary_content;
#[path = "policy_bytes/citations.rs"]
mod citations;
mod common;
#[path = "policy_bytes/content.rs"]
mod content;
#[path = "policy_bytes/contributions.rs"]
mod contributions;
#[path = "policy_bytes/names.rs"]
mod names;
#[path = "policy_bytes/selected_reads.rs"]
mod selected_reads;
#[path = "policy_bytes/storage_envelopes.rs"]
mod storage_envelopes;
#[path = "policy_bytes/storage_reads.rs"]
mod storage_reads;
#[path = "policy_bytes/stored_requests.rs"]
mod stored_requests;
#[path = "policy_bytes/vectors.rs"]
mod vectors;
use common::*;
use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn byte_requests_cover_publication_selection_history_and_collection_without_losing_i64() {
    for input in accepted_cases() {
        let output = evaluate_revision_policy(&input);
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            response["status"],
            "accepted",
            "{}",
            String::from_utf8_lossy(&output)
        );
    }
    let output = String::from_utf8(evaluate_revision_policy(&large_integer_publication())).unwrap();
    assert!(output.contains("9007199254741007"));
    assert!(output.contains("9007199254740993"));
    for input in rejected_cases() {
        let response: Value = serde_json::from_slice(&evaluate_revision_policy(&input)).unwrap();
        assert_eq!(response["status"], "rejected");
        assert!(!response["message"].as_str().unwrap().is_empty());
    }
}

#[test]
#[ignore = "cargo rustc -p lumvise-semantic-revisions --target wasm32-unknown-unknown --release --lib --crate-type cdylib; set LUMVISE_REVISION_WASM and run this parity check"]
fn wasm_and_native_return_identical_bytes_for_policy_and_large_integer_cases() {
    let module = std::env::var("LUMVISE_REVISION_WASM")
        .expect("LUMVISE_REVISION_WASM must name the built policy module");
    let cases = accepted_cases()
        .into_iter()
        .chain(rejected_cases())
        .map(|input| {
            json!({
                "input": String::from_utf8(input.clone()).unwrap(),
                "expected": String::from_utf8(evaluate_revision_policy(&input)).unwrap(),
            })
        })
        .collect::<Vec<_>>();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wasm_parity.mjs");
    let mut child = Command::new("node")
        .arg(script)
        .arg(module)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn accepted_cases() -> Vec<Vec<u8>> {
    let mut project = ProjectFixture::new();
    let mut initial = project.edit("initial", vec![element("a", "a.rs", "old")]);
    initial.source_anchor = Some(SemanticSourceAnchor {
        revision: "commit-1".into(),
        canonical_remote: None,
        dirty: false,
    });
    project.publish(&initial);
    let changed = project.edit("advance", vec![element("a", "a.rs", "new")]);
    project.publish(&changed);
    let state = &project.state;
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = state.plan_retention("sweep", &policy, 20_000).unwrap();
    let mut cases = history_cases(state);
    cases.push(selected_families_case(state));
    cases.extend(count_retention_cases(state));
    cases.push(serde_json::to_vec(&json!({"operation":"retention_plan", "state":state, "operation_id":"sweep", "policy":policy, "now_ms":20_000})).unwrap());
    cases.push(serde_json::to_vec(&json!({"operation":"collect", "state":state, "plan":plan, "policy":policy, "now_ms":20_000, "next_commit":state.commit_version+1})).unwrap());
    cases.push(selection_case(state));
    cases.push(adoption_case());
    cases.push(large_integer_publication());
    cases.push(required_snapshot_case());
    cases.push(element_payload_case());
    cases.push(connection_definition_case());
    cases.push(producer_publication_case());
    cases.extend(artifact_edits::cases());
    cases.extend(storage_envelopes::cases());
    cases.extend(storage_reads::cases());
    cases.extend(selected_reads::cases());
    cases.extend(citations::cases());
    cases.extend(contributions::cases());
    cases.extend(
        content::cases()
            .into_iter()
            .chain(archives::cases())
            .chain(binary_content::cases())
            .chain(attachment_retention::cases()),
    );
    cases.extend(stored_requests::cases());
    cases.extend(vectors::cases());
    cases.extend(names::cases());
    cases.push(
        serde_json::to_vec(
            &json!({"operation":"archive_merge", "state":state, "archived":state, "now_ms":30_000}),
        )
        .unwrap(),
    );
    cases
}

fn selected_families_case(state: &SemanticRevisionState) -> Vec<u8> {
    let selected = state.selection("working").unwrap();
    let basis = SemanticRevisionFamilyBasis {
        project_root: state.project_root.clone(),
        commit_version: state.commit_version,
        selection: Some(selected.selection.clone()),
        selected_records: selected.records.clone(),
        links: state.revision_links.clone(),
    };
    serde_json::to_vec(&json!({"operation":"families", "basis":basis, "query":{
        "project_root":ROOT, "selection_id":"working", "expected_commit_version":state.commit_version,
        "records":[{"kind":"element","record_id":"a"},{"kind":"artifact","record_id":"unknown"}]
    }})).unwrap()
}

fn producer_publication_case() -> Vec<u8> {
    let project = ProjectFixture::new();
    let mut request = project.edit("producer-first", vec![element("a", "a.rs", "A1")]);
    request.producer_base = Some(SemanticProducerBase {
        selection_id: "producer:checkout".into(),
        expected_generation: 0,
    });
    let basis = SemanticRevisionPublicationBasis {
        structure_coverage: SemanticRevisionStructureCoverage::Project,
        artifact_ids: vec![],
        facts: project.state,
    };
    serde_json::to_vec(&json!({"operation":"publication", "basis":basis, "request":request, "timestamp":timestamp(1000), "next_commit":1})).unwrap()
}

fn element_payload_case() -> Vec<u8> {
    let mut project = ProjectFixture::new();
    let mut locator = element("document", "note.md", "locator");
    locator.revision_dependencies = vec![SemanticRecordRef::artifact("payload")];
    let mut initial = project.edit("document", vec![locator]);
    initial.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("payload", "document", "Exact document body"),
        media_type: "text/plain".into(),
    });
    project.publish(&initial);
    let mut detach = project.edit("detach-body", vec![]);
    detach.structure = None;
    detach.removed_artifact_ids = vec!["payload".into()];
    project.publish(&detach);
    serde_json::to_vec(&json!({"operation":"snapshot", "state":project.state,
        "selection_id":"working", "published_at":"time"}))
    .unwrap()
}

#[test]
fn byte_snapshot_preserves_element_payload_dependencies_outside_membership() {
    let response: Value =
        serde_json::from_slice(&evaluate_revision_policy(&element_payload_case())).unwrap();
    let selected = &response["result"]["value"];
    assert_eq!(
        selected["elements"][0]["revision_dependencies"],
        json!([SemanticRecordRef::artifact("payload")])
    );
    assert_eq!(selected["artifacts"], json!([]));
    assert_eq!(
        selected["required_records"][0]["record"],
        json!(SemanticRecordRef::artifact("payload"))
    );
}

#[test]
fn byte_publication_returns_the_producer_pin_and_shared_head_in_one_plan() {
    let response: Value =
        serde_json::from_slice(&evaluate_revision_policy(&producer_publication_case())).unwrap();
    let plan = &response["result"]["value"];
    assert_eq!(
        plan["outcome"]["value"]["producer_base"]["selection_id"],
        "producer:checkout"
    );
    assert_eq!(plan["outcome"]["value"]["producer_base"]["generation"], 1);
    assert_eq!(plan["selections"].as_array().unwrap().len(), 2);
    assert_eq!(plan["selection_deltas"].as_array().unwrap().len(), 2);
}

fn count_retention_cases(state: &SemanticRevisionState) -> Vec<Vec<u8>> {
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let plan = state
        .plan_retention("count-sweep", &policy, 20_000)
        .unwrap();
    [
        json!({"operation":"retention_plan", "state":state, "operation_id":"count-sweep", "policy":policy, "now_ms":20_000}),
        json!({"operation":"collect", "state":state, "plan":plan, "policy":policy, "now_ms":20_000, "next_commit":state.commit_version+1}),
    ].iter().map(|request| serde_json::to_vec(request).unwrap()).collect()
}

fn connection_definition_case() -> Vec<u8> {
    let state = SemanticRevisionState {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        connections: vec![SemanticRevisionConnection {
            connection_id: "externally-assigned:%_1".into(),
            relationship: SemanticRelationship {
                project_root: ROOT.into(),
                source_element_id: "old-source".into(),
                target_element_id: "old-target".into(),
                relationship_kind: "calls".into(),
                label: "calls".into(),
                metadata: json!({"counter":u64::MAX}),
            },
        }],
        ..Default::default()
    };
    serde_json::to_vec(&json!({"operation":"connections", "state":state,
        "query":{"project_root":ROOT,"project_id":PROJECT_ID,"connection_ids":["externally-assigned:%_1"]}})).unwrap()
}

fn history_cases(state: &SemanticRevisionState) -> Vec<Vec<u8>> {
    [
        json!({"operation":"membership", "state":state, "query":{"project_root":ROOT,"selection_id":"working","structure_coverage":{"kind":"project"},"artifact_ids":[]}}),
        json!({"operation":"selection_at", "state":state, "selection_id":"working","generation":1}),
        json!({"operation":"source_anchor", "state":state,"selection_id":"working","anchor":{"revision":"commit-1","canonical_remote":null,"dirty":false}}),
        json!({"operation":"snapshot", "state":state,"selection_id":"working","published_at":"time"}),
    ].iter().map(|request| serde_json::to_vec(request).unwrap()).collect()
}

fn required_snapshot_case() -> Vec<u8> {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut note = artifact("note", "a", "Original evidence");
    note.dependencies.push(ArtifactDependency {
        target: ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "a".into(),
        },
    });
    let mut request = project.edit(
        "note",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: note,
        media_type: "text/plain".into(),
    });
    project.publish(&request);
    let request = project.edit(
        "advance",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    project.publish(&request);
    serde_json::to_vec(&json!({"operation":"snapshot", "state":project.state,"selection_id":"working","published_at":"time"})).unwrap()
}

fn selection_case(state: &SemanticRevisionState) -> Vec<u8> {
    let request = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "pin".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "public".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::Pinned,
        records: vec![SemanticRecordRef::element("a")],
        connection_ids: vec![],
        source_anchor: None,
    };
    serde_json::to_vec(&json!({"operation":"selection","state":state,"request":request,"timestamp":timestamp(3000),"next_commit":state.commit_version+1})).unwrap()
}

fn adoption_case() -> Vec<u8> {
    serde_json::to_vec(&json!({"operation":"adopt","project_root":ROOT,"project_id":PROJECT_ID,"selection_id":"working","source_context":"git:main",
        "elements":[element("a","a.rs","old")],"artifacts":[],"relationships":[],"commit_version":1,"now_ms":1000})).unwrap()
}

fn large_integer_publication() -> Vec<u8> {
    let mut project = ProjectFixture::new();
    project.state.commit_version = 9_007_199_254_740_993;
    let mut source = element("a", "a.rs", "body");
    source.metadata = serde_json::from_str(r#"{"z":9007199254740993,"a":{"y":4,"b":3}}"#).unwrap();
    let request = project.edit("large-integers", vec![source]);
    let basis = SemanticRevisionPublicationBasis {
        structure_coverage: structure_coverage(&request),
        artifact_ids: vec![],
        facts: project.state,
    };
    serde_json::to_vec(&json!({"operation":"publication","basis":basis,"request":request,
        "timestamp":{"unix_ms":9_007_199_254_740_993_i64,"rfc3339":"time"},"next_commit":9_007_199_254_741_007_i64})).unwrap()
}

fn rejected_cases() -> Vec<Vec<u8>> {
    let mut cases = vec![
        b"{".to_vec(),
        br#"{"operation":"unknown"}"#.to_vec(),
        br#"{"operation":"adopt","unexpected":true}"#.to_vec(),
    ];
    cases.extend(vectors::rejected_cases());
    cases
}
