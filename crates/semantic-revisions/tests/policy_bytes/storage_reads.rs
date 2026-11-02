//! A named indexed storage fake executes only Rust-requested reads, mirroring the gateway boundary.
use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) struct IndexedGatewayStorage {
    rows: Vec<Value>,
    pub(super) requests: Vec<Vec<u8>>,
    pub(super) queries: Vec<Value>,
    /// The rows the last read fetched, as the gateway would hand them to a later execute call.
    pub(super) loaded: Vec<Value>,
}

impl IndexedGatewayStorage {
    pub(super) fn new(state: &SemanticRevisionState) -> Self {
        Self::with_rows(
            storage(json!({"action":"rows","state_json":serde_json::to_string(state).unwrap()}))
                .as_array()
                .unwrap()
                .clone(),
        )
    }
    /// Serves rows that already carry attached content, which `rows` regenerated from a state lacks.
    pub(super) fn with_rows(rows: Vec<Value>) -> Self {
        Self {
            rows,
            requests: vec![],
            queries: vec![],
            loaded: vec![],
        }
    }
    pub(super) fn read(
        &mut self,
        state: &SemanticRevisionState,
        intent: Value,
    ) -> SemanticRevisionState {
        let mut loaded = BTreeMap::<String, Value>::new();
        loop {
            let request = json!({"action":"read","context":{"project_root":ROOT,"project_id":PROJECT_ID,"commit_version":state.commit_version.to_string()},"intent_json":intent.to_string(),"rows":loaded.values().collect::<Vec<_>>(),"completed":self.queries});
            self.requests.push(
                serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap(),
            );
            let result = storage(request);
            if let Some(state) = result["state_json"].as_str() {
                self.loaded = loaded.into_values().collect();
                return serde_json::from_str(state).unwrap();
            }
            for query in result["queries"].as_array().unwrap() {
                assert!(
                    !self.queries.contains(query),
                    "storage repeated an acknowledged lookup: {query}"
                );
                for row in self.rows.iter().filter(|row| self.matches(row, query)) {
                    loaded.insert(row["key"].to_string(), row.clone());
                }
                self.queries.push(query.clone());
            }
        }
    }
    fn matches(&self, row: &Value, query: &Value) -> bool {
        match query["query"].as_str().unwrap() {
            "key" => row["key"] == query["key"],
            "index" => {
                has_index(
                    row,
                    query["name"].as_str().unwrap(),
                    query["value"].as_str().unwrap(),
                ) && query["selection_id"]
                    .as_str()
                    .is_none_or(|id| self.selected(row, id))
            }
            "path" => self.path_matches(row, query),
            "prefix" => {
                row["indexes"].as_array().unwrap().iter().any(|index| {
                    index["name"] == "record_path"
                        && index["value"]
                            .as_str()
                            .unwrap()
                            .starts_with(query["prefix"].as_str().unwrap())
                }) && self.selected(row, query["selection_id"].as_str().unwrap())
            }
            // The page boundary belongs to the SQL adapter; this fake answers with every match, the unpaged answer.
            "index_page" => {
                let names = query["names"].as_array().unwrap();
                let values = query["values"].as_array().unwrap();
                names.iter().any(|name| {
                    values.iter().any(|value| {
                        has_index(row, name.as_str().unwrap(), value.as_str().unwrap())
                    })
                }) && self.selected(row, query["selection_id"].as_str().unwrap())
            }
            "all" => true,
            other => panic!("unexpected storage query {other}"),
        }
    }
    fn path_matches(&self, row: &Value, query: &Value) -> bool {
        let path = query["path"].as_str().unwrap();
        row["indexes"].as_array().unwrap().iter().any(|index| {
            index["name"] == "record_path"
                && index["value"].as_str().is_some_and(|value| {
                    value == path || value.starts_with(&format!("{}/", path.trim_end_matches('/')))
                })
        }) && query["provider"]
            .as_str()
            .is_none_or(|provider| has_index(row, "record_provider", provider))
            && self.selected(row, query["selection_id"].as_str().unwrap())
    }
    fn selected(&self, row: &Value, selection: &str) -> bool {
        if row["key"]["kind"] == "association" {
            return ["association_record", "association_owner"]
                .iter()
                .all(|index| {
                    let reference = row["indexes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| entry["name"] == *index)
                        .unwrap()["value"]
                        .as_str()
                        .unwrap();
                    self.rows.iter().any(|member| {
                        has_index(member, "selection_member", selection)
                            && has_index(member, "member_record", reference)
                    })
                });
        }
        let name = match row["key"]["kind"].as_str().unwrap() {
            "record" => "member_record",
            "connection" => "member_connection",
            _ => return false,
        };
        self.rows.iter().any(|member| {
            has_index(member, "selection_member", selection)
                && has_index(member, name, row["key"]["identity"].as_str().unwrap())
        })
    }
}

fn has_index(row: &Value, name: &str, value: &str) -> bool {
    row["indexes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|index| index["name"] == name && index["value"] == value)
}

fn relationship(source: &str, target: &str) -> SemanticRelationship {
    SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: source.into(),
        target_element_id: target.into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    }
}

#[test]
fn unavailable_headers_refuse_empty_selection_ids_before_storage_reads() {
    for selection_id in ["", " "] {
        let response = super::storage_envelopes::response(json!({
            "action":"prepare", "authenticated_actor":"member",
            "request_json":json!({"operation":"unavailable", "selection_id":selection_id}).to_string()
        }));
        assert_eq!(response["status"], "rejected", "{response}");
        assert!(response["message"].as_str().unwrap().contains("nonempty"));
    }
}

fn connected_project() -> ProjectFixture {
    let mut project = ProjectFixture::new();
    let mut edit = project.edit(
        "initial",
        ["a", "b", "c", "d"]
            .into_iter()
            .map(|id| element(id, &format!("{id}.rs"), "one"))
            .collect(),
    );
    edit.structure.as_mut().unwrap().relationships = vec![
        relationship("a", "b"),
        relationship("b", "c"),
        relationship("c", "d"),
    ];
    project.publish(&edit);
    project
}

fn scoped_edit(project: &ProjectFixture) -> SemanticRevisionPublication {
    let partition = SemanticPartition {
        project_root: ROOT.into(),
        replace_paths: vec!["a.rs".into()],
        provider_instance_id: None,
    };
    let query = SemanticRevisionSelectionQuery {
        project_root: ROOT.into(),
        selection_id: "working".into(),
        structure_coverage: SemanticRevisionStructureCoverage::Partition(partition.clone()),
        artifact_ids: vec![],
    };
    let mut edit = project.edit("scoped", vec![element("a", "a.rs", "two")]);
    edit.expected = project
        .state
        .selection_members(&query)
        .unwrap()
        .unwrap()
        .expected;
    edit.structure.as_mut().unwrap().partition = Some(partition);
    edit.structure.as_mut().unwrap().relationships = vec![relationship("a", "b")];
    edit
}

#[test]
fn scoped_fetches_load_owned_connections_but_do_not_walk_the_whole_code_graph() {
    let project = connected_project();
    let edit = scoped_edit(&project);
    let mut store = IndexedGatewayStorage::new(&project.state);
    let facts = store.read(
        &project.state,
        json!({"operation":"publication","request":edit}),
    );
    assert!(
        !store
            .queries
            .iter()
            .any(|query| query["query"] == "all" || query["name"] == "selection_member")
    );
    assert_eq!(
        facts
            .records
            .iter()
            .map(|record| record.record.record_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(facts.connections.len(), 1);
    let basis = SemanticRevisionPublicationBasis {
        structure_coverage: structure_coverage(&edit),
        artifact_ids: requested_artifact_ids(&edit),
        facts,
    };
    let scoped = basis
        .plan_publication(&edit, &timestamp(project.now), 2)
        .unwrap();
    let full = project.plan(&edit);
    assert_eq!(scoped.outcome, full.outcome);
    assert_eq!(scoped.selection_deltas, full.selection_deltas);
}

#[test]
fn accepted_operation_is_fetched_before_any_mutable_body_or_selection() {
    let mut project = connected_project();
    let edit = scoped_edit(&project);
    project.publish(&edit);
    let mut store = IndexedGatewayStorage::new(&project.state);
    for row in &mut store.rows {
        if row["key"]["kind"] == "record" {
            row["body_json"] = Value::Null;
        }
    }
    let facts = store.read(
        &project.state,
        json!({"operation":"publication","request":edit}),
    );
    assert!(facts.records.is_empty());
    assert!(store.queries.iter().all(|query| {
        query["query"] == "key"
            && ["project", "operation", "retention", "recovery"]
                .contains(&query["key"]["kind"].as_str().unwrap())
    }));
    let basis = SemanticRevisionPublicationBasis {
        structure_coverage: structure_coverage(&edit),
        artifact_ids: requested_artifact_ids(&edit),
        facts,
    };
    assert!(matches!(
        basis
            .plan_publication(&edit, &timestamp(project.now), 3)
            .unwrap()
            .outcome,
        SemanticRevisionPublicationOutcome::Accepted(_)
    ));
}

#[test]
fn pin_planning_fetches_dependency_closure_and_intersecting_existing_pins() {
    let project = connected_project();
    let request = SemanticRevisionSelectionUpdate {
        operation_id: "pin".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "public".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        artifact_edits: BTreeMap::new(),
        mode: SemanticSelectionMode::Pinned,
        records: vec![SemanticRecordRef::element("a")],
        connection_ids: vec![],
        source_anchor: None,
    };
    let mut store = IndexedGatewayStorage::new(&project.state);
    let facts = store.read(
        &project.state,
        json!({"operation":"selection","request":request}),
    );
    let planned = facts
        .plan_selection(&request, &timestamp(project.now), 2)
        .unwrap();
    let full = project
        .state
        .plan_selection(&request, &timestamp(project.now), 2)
        .unwrap();
    assert_eq!(planned, full);
    assert_eq!(facts.records.len(), 1);
    assert_eq!(facts.selection("working").unwrap().records.len(), 1);
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let project = connected_project();
    let mut store = IndexedGatewayStorage::new(&project.state);
    store.read(
        &project.state,
        json!({"operation":"publication","request":scoped_edit(&project)}),
    );
    let mut cases = store.requests;
    cases.extend(unavailable_parity_cases());
    cases
}

fn unavailable_parity_cases() -> Vec<Vec<u8>> {
    let mut project = super::citations::acknowledged_loss_fixture();
    let (before_prepare, before_store, before_facts) = unavailable_read(&project.state);
    let mut cases = vec![storage_case(before_prepare)];
    cases.extend(before_store.requests.clone());
    cases.push(storage_case(unavailable_execute_request(
        &before_store,
        &before_facts,
    )));

    super::citations::publish_unavailable_successor(&mut project);
    let (after_prepare, after_store, after_facts) = unavailable_read(&project.state);
    cases.push(storage_case(after_prepare));
    cases.extend(after_store.requests.clone());
    cases.push(storage_case(unavailable_execute_request(
        &after_store,
        &after_facts,
    )));
    cases
}

fn unavailable_read(
    state: &SemanticRevisionState,
) -> (Value, IndexedGatewayStorage, SemanticRevisionState) {
    let prepare = unavailable_prepare_request();
    let prepared = storage(prepare.clone());
    assert_eq!(prepared["kind"], "intent");
    let intent: Value = serde_json::from_str(prepared["value_json"].as_str().unwrap()).unwrap();
    let mut store = IndexedGatewayStorage::new(state);
    let facts = store.read(state, intent);
    (prepare, store, facts)
}

fn unavailable_prepare_request() -> Value {
    json!({
        "action":"prepare",
        "request_json":serde_json::to_string(&json!({
            "operation":"unavailable",
            "selection_id":"working"
        })).unwrap(),
        "authenticated_actor":null
    })
}

fn unavailable_execute_request(
    store: &IndexedGatewayStorage,
    facts: &SemanticRevisionState,
) -> Value {
    let now = timestamp(4000);
    json!({
        "action":"execute",
        "request_json":serde_json::to_string(&json!({
            "operation":"unavailable",
            "selection_id":"working"
        })).unwrap(),
        "state_json":serde_json::to_string(facts).unwrap(),
        "rows":store.loaded.clone(),
        "authenticated_actor":null,
        "now_ms":now.unix_ms.to_string(),
        "published_at":now.rfc3339,
        "next_commit":(facts.commit_version + 1).to_string()
    })
}

fn storage_case(request: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
}

fn assert_unavailable_reads_are_metadata_only(store: &IndexedGatewayStorage) {
    assert!(store.queries.iter().all(|query| {
        matches!(query["query"].as_str(), Some("key" | "index"))
            && if query["query"] == "index" {
                query["name"] == "record_unavailable"
            } else {
                ["project", "selection", "record"].contains(&query["key"]["kind"].as_str().unwrap())
            }
    }));
    assert!(
        store
            .loaded
            .iter()
            .all(|row| { row["key"]["kind"] != "record" || row["body_json"].is_null() })
    );
}

#[test]
fn unavailable_read_fetches_only_selected_headers_and_stops_after_successor() {
    let mut project = super::citations::acknowledged_loss_fixture();
    let unavailable = SemanticRecordRef::element("evidence");
    let selected = project.state.selection("working").unwrap();
    assert!(selected.records.contains(&unavailable));
    let expected_header = project
        .state
        .records
        .iter()
        .find(|record| record.record == unavailable)
        .map(SemanticRevisionRecordHeader::from)
        .unwrap();

    let (_, store, facts) = unavailable_read(&project.state);
    assert_unavailable_reads_are_metadata_only(&store);
    let unavailable_key = serde_json::to_string(&unavailable).unwrap();
    assert!(store.loaded.iter().any(|row| {
        row["key"]["kind"] == "record"
            && row["key"]["identity"] == unavailable_key
            && row["body_json"].is_null()
    }));
    let healthy_key = serde_json::to_string(&SemanticRecordRef::element("healthy")).unwrap();
    assert!(
        store
            .loaded
            .iter()
            .all(|row| row["key"]["identity"] != healthy_key)
    );

    let encoded = storage(unavailable_execute_request(&store, &facts));
    assert_eq!(encoded["kind"], "unavailable");
    let headers: Vec<SemanticRevisionRecordHeader> =
        serde_json::from_str(encoded["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(headers, vec![expected_header]);
    assert_eq!(headers[0].record, unavailable);
    assert_eq!(
        headers[0]
            .unavailable
            .as_ref()
            .unwrap()
            .recovery_operation_id,
        "acknowledge-loss"
    );

    let successor_id = super::citations::publish_unavailable_successor(&mut project);
    let (_, successor_store, successor_facts) = unavailable_read(&project.state);
    let selected = project.state.selection("working").unwrap();
    assert!(!selected.records.contains(&unavailable));
    assert!(
        selected
            .records
            .contains(&SemanticRecordRef::element(&successor_id))
    );
    assert_unavailable_reads_are_metadata_only(&successor_store);
    let headers: Vec<SemanticRevisionRecordHeader> = serde_json::from_str(
        storage(unavailable_execute_request(
            &successor_store,
            &successor_facts,
        ))["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(headers.is_empty());
    for id in ["healthy", successor_id.as_str()] {
        let key = serde_json::to_string(&SemanticRecordRef::element(id)).unwrap();
        assert!(
            successor_store
                .loaded
                .iter()
                .all(|row| row["key"]["identity"] != key)
        );
    }
}

#[test]
fn citation_fetches_only_owned_receipts_before_loading_evidence() {
    let mut project = connected_project();
    let request = SemanticCitationRetentionAcquire {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        source_context: "git:main".into(),
        intent: SemanticCitationRetentionIntent {
            space_id: "space".into(),
            document_id: "doc".into(),
            publication_id: "write".into(),
        },
        records: vec![SemanticRecordRef::element("a")],
        artifact_edits: BTreeMap::new(),
    };
    let mut store = IndexedGatewayStorage::new(&project.state);
    let facts = store.read(
        &project.state,
        json!({"operation":"citation_acquisition","request":request,"authenticated_actor":"actor"}),
    );
    let scoped = facts
        .plan_citation_acquisition(&request, "actor", &timestamp(2000), 2)
        .unwrap();
    let full = project
        .state
        .plan_citation_acquisition(&request, "actor", &timestamp(2000), 2)
        .unwrap();
    assert_eq!(scoped, full);
    project.state.apply_publication(scoped).unwrap();
    let release = SemanticCitationRetentionRelease {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        intent: request.intent,
        expected_generation: 1,
    };
    let mut stranger = IndexedGatewayStorage::new(&project.state);
    let facts = stranger.read(
        &project.state,
        json!({"operation":"citation_release","request":release,"authenticated_actor":"stranger"}),
    );
    assert!(facts.records.is_empty());
    assert!(
        facts
            .plan_citation_release(&release, "stranger", &timestamp(3000), 3)
            .is_err()
    );
    assert!(stranger.queries.iter().all(|query| query["query"] == "key"));
}
