//! Named host/source/tokenizer fake around the real native persistence boundary.
use super::super::*;
use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
use lumvise_plugin_sdk::HostCallTransport;
use lumvise_resource_routing::InvocationControl;
use std::sync::Arc;

pub(super) const ROOT: &str = "/rebuild-scenario";
pub(super) const PROJECT_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
pub(super) const PROJECT_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

pub(super) struct ScenarioHost {
    persistence: Arc<LocalPersistence>,
    broker: AppCoreHostCapabilityBroker,
    pub page: String,
    pub profile: String,
    pub project_id: String,
    pub deny_scope: bool,
    pub capability_order: Vec<String>,
    pub embed_calls: Vec<String>,
    pub lose_commit_ack: bool,
    pub lose_stage_ack: bool,
    pub reject_embedding: bool,
    pub commit_calls: usize,
    pub storage_payloads: Vec<Value>,
    pub begin_next_chunks: Vec<u32>,
    pub lost_stage_ack_observed: bool,
}

impl ScenarioHost {
    pub fn new() -> Self {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let words = (0..30)
            .map(|index| format!("w{index:02} "))
            .collect::<String>();
        let page = format!("<h2>Heading</h2><p>{words}TAILFACT TAILFACT TAILFACT</p>");
        let fingerprint = lumvise_project_indexer::html_section_texts(&page).unwrap()[0]
            .content_fingerprint
            .clone();
        let elements = vec![
            element("a-section", "html_section", "Heading", Some(fingerprint)),
            element("z-file", "file", "page.html", None),
        ];
        persistence
            .execute(
                SemanticOperation::SyncStructure {
                    project_root: ROOT.into(),
                    elements,
                    relationships: vec![],
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
        let broker = AppCoreHostCapabilityBroker::new(persistence.clone(), persistence.clone());
        Self {
            persistence,
            broker,
            page,
            profile: "original-profile".into(),
            project_id: PROJECT_A.into(),
            deny_scope: false,
            capability_order: Vec::new(),
            embed_calls: Vec::new(),
            lose_commit_ack: false,
            lose_stage_ack: false,
            reject_embedding: false,
            commit_calls: 0,
            storage_payloads: Vec::new(),
            begin_next_chunks: Vec::new(),
            lost_stage_ack_observed: false,
        }
    }

    pub fn call(&mut self, input: Value, units: usize) -> Result<Value, PluginError> {
        let mut remaining = units;
        let mut context = PluginContext::for_test(self);
        super::super::invoke_with_budget(input, &mut context, || {
            let allowed = remaining > 0;
            remaining = remaining.saturating_sub(1);
            allowed
        })
    }

    pub fn visible_text_hits(&self) -> usize {
        match self
            .persistence
            .execute(
                SemanticOperation::SearchElementTextVectors {
                    project_root: ROOT.into(),
                    query: vec![1.0, 0.0],
                    k: 10,
                    engine_id: "fake-engine".into(),
                    model: Some("fake-model".into()),
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap()
        {
            SemanticResult::ElementVectorSearch(results) => results.len(),
            other => panic!("unexpected search result {other:?}"),
        }
    }

    pub fn persisted_run(&self, reply: &Value) -> Value {
        self.broker
            .invoke(request(
                "storage.plugin",
                json!({"operation":"get_row","table_name":storage::SEARCH_INDEX_STATE,
            "row_key":state::run_key(ROOT, &self.project_id, reply["run_id"].as_str().unwrap())}),
            ))
            .unwrap()
    }

    fn identity(&self) -> Value {
        json!({"engine_id":"fake-engine","model":"fake-model","profile_fingerprint":self.profile})
    }

    fn plan(&self, input: &Value) -> Result<Value, PluginError> {
        let text = input["text"].as_str().unwrap();
        let mut ranges = Vec::new();
        let mut start = 0;
        if !text.trim().is_empty() {
            while start < text.len() {
                let end = text[start..]
                    .char_indices()
                    .nth(24)
                    .map_or(text.len(), |(offset, _)| start + offset);
                ranges.push(json!({"start_byte":start,"end_byte":end,"token_count":text[start..end].chars().count()+2}));
                start = end;
            }
        }
        let mut identity = self.identity();
        identity["ranges"] = json!(ranges);
        Ok(identity)
    }

    fn embed(&mut self, input: &Value) -> Result<Value, PluginError> {
        if self.reject_embedding {
            return Err(PluginError::new(
                "embedding_rejected",
                "private_section_body leaked by upstream provider",
                false,
            ));
        }
        if input["expected_profile_fingerprint"].as_str() != Some(self.profile.as_str()) {
            return Err(failure("embedding_profile_conflict", "profile changed"));
        }
        let mut vectors = Vec::new();
        for text in input["texts"].as_array().unwrap() {
            let text = text.as_str().unwrap();
            self.embed_calls.push(text.into());
            vectors.push(vec![if text.contains("TAILFACT") { 1.0 } else { 0.0 }, 1.0]);
        }
        let mut identity = self.identity();
        identity["vectors"] = json!(vectors);
        Ok(identity)
    }
}

impl HostCallTransport for ScenarioHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.capability_order.push(format!(
            "{capability}:{}",
            input["operation"].as_str().unwrap_or_default()
        ));
        if capability == "storage.semantic"
            && input["operation"] == "project_snapshot"
            && self.deny_scope
        {
            return Err(failure(
                "source_scope_denied",
                "source scope is not authorized",
            ));
        }
        if capability == "storage.semantic" && input["operation"] == "project_identity" {
            if input["project_root"] != ROOT {
                return Err(failure(
                    "source_scope_denied",
                    "identity root is not authorized",
                ));
            }
            return Ok(json!({"identity":{"project_id":self.project_id}}));
        }
        match capability {
            "neural.chunk_text" => return self.plan(&input),
            "neural.embed" => return self.embed(&input),
            "project.source" => {
                if input["project_root"] != ROOT || input["path"] != "page.html" {
                    return Err(failure("source_scope_denied", "source scope mismatch"));
                }
                return Ok(json!({"text":self.page}));
            }
            _ => {}
        }
        if capability == "storage.plugin"
            && matches!(
                input["operation"].as_str(),
                Some("put_row" | "stage_mutations")
            )
        {
            self.storage_payloads.push(input.clone());
        }
        let operation = input["operation"].as_str().unwrap_or_default().to_owned();
        let result = self
            .broker
            .invoke(request(capability, input))
            .map_err(|error| PluginError::new("scenario_host_failure", error.to_string(), false))?;
        if operation == "begin_element_text_vector_generation" {
            self.begin_next_chunks
                .push(result["next_chunk"].as_u64().unwrap() as u32);
        }
        if operation == "commit_element_text_vector_generation" {
            self.commit_calls += 1;
        }
        let lose = match operation.as_str() {
            "commit_element_text_vector_generation" => std::mem::take(&mut self.lose_commit_ack),
            "stage_element_text_vector_chunk" => std::mem::take(&mut self.lose_stage_ack),
            _ => false,
        };
        if lose {
            if operation == "stage_element_text_vector_chunk" {
                self.lost_stage_ack_observed = true;
            }
            return Err(failure(
                "acknowledgement_lost",
                "simulated lost acknowledgement",
            ));
        }
        Ok(result)
    }
}

fn request(capability: &str, input: Value) -> HostCapabilityRequest {
    HostCapabilityRequest {
        plugin_id: "builtin.semantic".into(),
        invocation_id: "scenario-invocation".into(),
        call_id: "scenario-call".into(),
        capability_id: capability.into(),
        required_version: "1".into(),
        input,
    }
}

fn element(
    id: &str,
    kind: &str,
    name: &str,
    fingerprint: Option<String>,
) -> lumvise_db_core::SemanticElement {
    lumvise_db_core::SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "page".into(),
        path: "page.html".into(),
        element_kind: kind.into(),
        name: name.into(),
        parent_element_id: None,
        content_fingerprint: fingerprint,
        start_line: Some(1),
        end_line: Some(1),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
        revision_dependencies: vec![],
    }
}
