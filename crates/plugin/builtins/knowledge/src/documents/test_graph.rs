//! Named fake I/O for the real document owner used by Knowledge's close tests.
//!
//! Writes use the real App Core, Semantic plugin and LocalPersistence. Cached read records
//! permit explicit malformed-I/O fixtures; no conflict, revision or retention policy lives here.

use crate::KnowledgePlugin;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use super::test_persistence::DocumentTestPersistence;

pub(crate) const INJECTED: &str = "injected_write_failure";

#[derive(Default)]
pub(crate) struct FakeDocumentGraph {
    pub(crate) memory_scope: Option<lumvise_contracts::KnowledgeAccessContext>,
    pub(crate) elements: BTreeMap<String, Value>,
    pub(crate) artifacts: BTreeMap<String, Value>,
    pub(crate) revision_artifact_overrides: BTreeMap<String, Option<Value>>,
    /// Stored relationships keyed by their canonical JSON identity.
    pub(crate) relationships: BTreeMap<String, Value>,
    /// Successful index publications (`ingest_index_batch`), each one atomic.
    pub(crate) writes: usize,
    /// Successful artifact upserts, including staged immutable document bodies.
    pub(crate) artifact_writes: usize,
    /// Artifact reads by identity; a captured-snapshot decode must leave this unchanged.
    pub(crate) artifact_reads: usize,
    /// Successful writes in order: `artifact:<id>` or `publish:<partition paths>`.
    pub(crate) operations: Vec<String>,
    pub(crate) published_document_ids: Vec<String>,
    pub(crate) embeddings: FakeDocumentEmbeddings,
    pub(crate) reject_unscoped_reads: bool,
    pub(crate) targeted_reads: Vec<Value>,
    /// Kinds of every `scoped_read` request, in order (`Structure`, `Location`, `PathPrefix`).
    pub(crate) scoped_read_kinds: Vec<String>,
    persistence: DocumentTestPersistence,
    fixture_seeded: bool,
    commit_version: i64,
    fault: Option<(&'static str, usize)>,
}

#[derive(Default)]
pub(crate) struct FakeDocumentEmbeddings {
    pub(crate) enabled: bool,
}

impl FakeDocumentEmbeddings {
    fn embed(&self, input: &Value) -> Result<Value, PluginError> {
        if !self.enabled {
            return Err(PluginError::unknown_capability("neural.embed"));
        }
        let vectors = input["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|text| {
                let text = text.as_str().unwrap();
                if text.contains("pipeline") || text.contains("connections") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect::<Vec<_>>();
        Ok(json!({"vectors": vectors}))
    }
}

impl HostCallTransport for FakeDocumentGraph {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        if matches!(
            capability,
            "storage.semantic" | "storage.plugin" | "runtime.project_execution"
        ) {
            self.seed_fixture()?;
        }
        match (capability, input["operation"].as_str()) {
            ("runtime.knowledge_scope", Some("current")) => self
                .memory_scope
                .as_ref()
                .map(|scope| json!(scope))
                .ok_or_else(|| {
                    PluginError::new("knowledge_scope_denied", "no fixture memory scope", false)
                }),
            ("runtime.project_execution", Some("scope")) => {
                Ok(json!({"client_instance_id":"document-close-test"}))
            }
            ("neural.embed", _) => self.embeddings.embed(&input),
            ("storage.semantic", Some("element")) if self.reject_unscoped_reads => {
                Err(PluginError::new(
                    "unscoped_document_read",
                    "Expected an exact project-scoped document query",
                    false,
                ))
            }
            ("storage.semantic", Some("element")) => Ok(
                json!({"element": self.elements.get(input["semantic_element_id"].as_str().unwrap())}),
            ),
            ("storage.semantic", Some("artifact")) => {
                self.artifact_reads += 1;
                Ok(json!({"artifact": self.artifacts.get(input["artifact_id"].as_str().unwrap())}))
            }
            ("storage.semantic", Some("artifacts_by_ids")) => {
                self.artifact_reads += 1;
                self.targeted_reads.push(input.clone());
                let artifacts = input["artifact_ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|id| self.artifacts.get(id.as_str().unwrap()).cloned())
                    .collect::<Vec<_>>();
                Ok(json!({"artifacts":artifacts}))
            }
            ("storage.semantic", Some("upsert_artifact")) => {
                self.upsert_artifact(&input["artifact"])
            }
            ("storage.semantic", Some("project_artifacts")) => Ok(json!({"artifacts":
            self.artifacts_in(
                input["project_root"].as_str().unwrap_or_default(),
                input["artifact_namespace"].as_str(),
            )})),
            ("storage.semantic", Some("artifacts_for_elements")) => {
                Ok(json!({"artifacts": self.artifacts_for_elements(&input)}))
            }
            ("storage.semantic", Some("project_roots")) => {
                Ok(json!({"project_roots": self.roots()}))
            }
            ("storage.semantic", Some("scoped_read")) => {
                // Mirrors the owner's Location validation: lines start at 1, so line 0 is refused, not normalized.
                if input["request"]["Location"]["line"]
                    .as_i64()
                    .is_some_and(|line| line < 1)
                {
                    return Err(PluginError::new(
                        "host_capability_execution_failed",
                        "storage.semantic operation failed: invalid value 0; expected line >= 1",
                        false,
                    ));
                }
                Ok(self.scoped(&input))
            }
            ("storage.semantic", Some("project_snapshot")) => Ok(self.snapshot(&input)),
            ("storage.semantic", Some("selective_subgraph")) => Ok(self.selective_subgraph(&input)),
            ("storage.semantic", Some("elements_by_ids_including_inactive")) => {
                self.targeted_reads.push(input.clone());
                Ok(self.targeted(&input))
            }
            ("storage.semantic", Some("resolve_revision")) => self.exact_revision(&input),
            ("plugin.invoke", _) if input["export_id"] == "ingest_index_batch" => {
                self.ingest(&input["input"])
            }
            _ => {
                let output =
                    self.persistence
                        .invoke("builtin.knowledge", capability, input.clone())?;
                if input["operation"] == "publish_revisions" {
                    self.reload_fixture()?;
                }
                Ok(output)
            }
        }
    }
}

impl FakeDocumentGraph {
    fn exact_revision(&mut self, input: &Value) -> Result<Value, PluginError> {
        self.targeted_reads.push(input.clone());
        let mut response =
            self.persistence
                .invoke("builtin.knowledge", "storage.semantic", input.clone())?;
        if input["record"]["kind"] == "artifact" {
            if let Some(body) = input["record"]["record_id"]
                .as_str()
                .and_then(|id| self.revision_artifact_overrides.get(id))
            {
                match body {
                    Some(body) => {
                        response["revision"]["body"] = json!({"state":"artifact", "record":body})
                    }
                    None => response["revision"] = Value::Null,
                }
            }
        }
        Ok(response)
    }

    pub(crate) fn targeted_documents(&mut self, input: Value) -> Result<Value, PluginError> {
        let mut response = self.host_call("storage.semantic", input)?;
        response["elements"]
            .as_array_mut()
            .unwrap()
            .retain(|element| super::record_format::is_owned(&element["metadata"]));
        Ok(response)
    }

    pub(crate) fn seed_sources(
        &mut self,
        elements: &[crate::semantic_context::SemanticElement],
    ) -> Result<(), PluginError> {
        if self.fixture_seeded {
            return Ok(());
        }
        for element in elements {
            self.elements
                .insert(element.semantic_element_id.clone(), json!(element));
        }
        self.seed_fixture()
    }

    fn seed_fixture(&mut self) -> Result<(), PluginError> {
        if self.fixture_seeded {
            return Ok(());
        }
        for root in self.roots() {
            self.persistence.seed_project(
                &root,
                fixture_rows(&self.elements, &root),
                fixture_rows(&self.relationships, &root),
            );
        }
        for artifact in self.artifacts.values() {
            self.persistence.seed_artifact(artifact);
        }
        self.fixture_seeded = true;
        Ok(())
    }

    fn reload_fixture(&mut self) -> Result<(), PluginError> {
        let roots = self.persistence.invoke(
            "builtin.knowledge",
            "storage.semantic",
            json!({"operation":"project_roots"}),
        )?;
        self.elements.clear();
        self.relationships.clear();
        for root in roots["project_roots"].as_array().unwrap() {
            let snapshot = self.persistence.invoke(
                "builtin.knowledge",
                "storage.semantic",
                json!({"operation":"project_snapshot", "scope":{"project_root":root}}),
            )?;
            self.commit_version = snapshot["commit_version"].as_i64().unwrap();
            for element in snapshot["elements"].as_array().unwrap() {
                self.elements.insert(
                    element["semantic_element_id"].as_str().unwrap().into(),
                    element.clone(),
                );
            }
            for artifact in snapshot["artifacts"].as_array().unwrap() {
                self.artifacts.insert(
                    artifact["artifact_id"].as_str().unwrap().into(),
                    artifact.clone(),
                );
            }
            for relationship in snapshot["relationships"].as_array().unwrap() {
                self.relationships
                    .insert(relationship.to_string(), relationship.clone());
            }
        }
        Ok(())
    }
    pub(crate) fn targeted(&self, input: &Value) -> Value {
        let ids = input["semantic_element_ids"].as_array().unwrap();
        let elements: Vec<&Value> = self
            .elements
            .values()
            .filter(|row| {
                let document_id =
                    row["metadata"]["indexer_metadata"]["knowledge_document"]["document_id"]
                        .as_str();
                row["project_root"] == input["project_root"]
                    && (ids.contains(&row["semantic_element_id"])
                        || document_id
                            .is_some_and(|id| ids.contains(&json!(super::records::element_id(id)))))
            })
            .collect();
        json!({"elements": elements})
    }

    pub(crate) fn call(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        let mut context = PluginContext::for_test(self);
        KnowledgePlugin::default().dispatch(export, input, &mut context)
    }

    /// Projection/document fixtures seed artifacts; write races use the real persistence tests.
    pub(crate) fn seed_knowledge(&mut self, input: Value) -> Result<(), PluginError> {
        self.call("create_knowledge", input)?;
        Ok(())
    }

    pub(crate) fn roots(&self) -> Vec<String> {
        self.elements
            .values()
            .filter_map(|row| row["project_root"].as_str().map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The portable scoped selectors: `Structure` lists one project; `Location` resolves one exact path,
    /// where a record without a span matches every line, as the real path index does.
    pub(crate) fn scoped(&mut self, input: &Value) -> Value {
        let kind = input["request"]
            .as_object()
            .and_then(|kinds| kinds.keys().next());
        self.scoped_read_kinds
            .push(kind.cloned().unwrap_or_default());
        if let Some(location) = input["request"].get("Location") {
            return self.location(location);
        }
        if let Some(prefix) = input["request"].get("PathPrefix") {
            let root = &prefix["project_root"];
            let elements: Vec<&Value> = self
                .elements
                .values()
                .filter(|row| {
                    &row["project_root"] == root
                        && row["path"].as_str().is_some_and(|path| {
                            path.starts_with(prefix["prefix"].as_str().unwrap_or("\0"))
                        })
                        && (prefix["include_inactive"] == true || row["lifecycle"] != "inactive")
                })
                .collect();
            return json!({"project_root": root, "elements": elements, "relationships": []});
        }
        let root = &input["request"]["Structure"]["scope"]["project_root"];
        let elements: Vec<&Value> = self
            .elements
            .values()
            .filter(|row| {
                &row["project_root"] == root
                    && (input["request"]["Structure"]["include_inactive"] == true
                        || row["lifecycle"] != "inactive")
            })
            .collect();
        json!({"project_root": root, "elements": elements, "relationships": []})
    }

    fn location(&self, location: &Value) -> Value {
        let root = &location["project_root"];
        let line = location["line"].as_i64().unwrap_or_default();
        let elements: Vec<&Value> = match location["path"].as_str() {
            None => Vec::new(),
            Some(path) => self
                .elements
                .values()
                .filter(|row| {
                    &row["project_root"] == root
                        && row["path"] == path
                        && (location["include_inactive"] == true || row["lifecycle"] != "inactive")
                        && row["start_line"].as_i64().is_none_or(|start| start <= line)
                        && row["end_line"].as_i64().is_none_or(|end| line <= end)
                })
                .collect(),
        };
        json!({"project_root": root, "elements": elements, "relationships": []})
    }

    /// Fails the next write of `kind` (`"publication"` or `"artifact"`) once,
    /// after `successes` writes of that kind have landed. Example: `fail_after("artifact", 0)`.
    pub(crate) fn fail_after(&mut self, kind: &'static str, successes: usize) {
        self.fault = Some((kind, successes));
    }

    fn take_fault(&mut self, kind: &'static str) -> Result<(), PluginError> {
        match self.fault {
            Some((pending, 0)) if pending == kind => {
                self.fault = None;
                Err(PluginError::new(
                    INJECTED,
                    "test boundary write interrupted",
                    true,
                ))
            }
            Some((pending, remaining)) if pending == kind => {
                self.fault = Some((pending, remaining - 1));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Stores one artifact after its active anchor check; body writes stage before pointers.
    fn upsert_artifact(&mut self, artifact: &Value) -> Result<Value, PluginError> {
        self.seed_fixture()?;
        self.take_fault("artifact")?;
        let id = artifact["artifact_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let output = self.persistence.invoke("builtin.knowledge", "storage.semantic",
            json!({"operation":"upsert_artifact", "artifact":artifact, "media_type":"text/markdown"}))?;
        self.artifacts.insert(id.clone(), artifact.clone());
        self.artifact_writes += 1;
        self.operations.push(format!("artifact:{id}"));
        Ok(output)
    }

    /// The project owning an artifact: its own scope, its knowledge scope, or its anchor's root.
    fn artifact_root(&self, artifact: &Value) -> Option<String> {
        artifact["project_root"]
            .as_str()
            .or_else(|| artifact["metadata"]["knowledge"]["project_root"].as_str())
            .map(str::to_owned)
            .or_else(|| {
                self.elements
                    .get(artifact["semantic_element_id"].as_str().unwrap_or_default())
                    .and_then(|element| element["project_root"].as_str().map(str::to_owned))
            })
    }

    /// Artifacts of one project, optionally narrowed to a namespace object such as `knowledge`.
    pub(crate) fn artifacts_in(&self, root: &str, namespace: Option<&str>) -> Vec<Value> {
        self.artifacts
            .values()
            .filter(|artifact| self.artifact_root(artifact).as_deref() == Some(root))
            .filter(|artifact| {
                namespace.is_none_or(|namespace| artifact["metadata"][namespace].is_object())
            })
            .cloned()
            .collect()
    }

    /// Direct anchors, plus artifacts inherited along `contains` edges from an ancestor.
    fn artifacts_for_elements(&self, input: &Value) -> Vec<Value> {
        let wanted: BTreeSet<&str> = input["semantic_element_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut found = Vec::new();
        for artifact in self.artifacts.values() {
            let anchor = artifact["semantic_element_id"].as_str().unwrap_or_default();
            if wanted.contains(anchor) {
                found.push(artifact.clone());
                continue;
            }
            for target in &wanted {
                if self.contains_path(anchor, target) {
                    let mut inherited = artifact.clone();
                    inherited["metadata"]["association_kind"] = json!("inherited");
                    found.push(inherited);
                    break;
                }
            }
        }
        found
    }

    fn contains_path(&self, ancestor: &str, descendant: &str) -> bool {
        let mut frontier = vec![ancestor.to_owned()];
        let mut seen = BTreeSet::new();
        while let Some(current) = frontier.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            for relationship in self.relationships.values() {
                if relationship["relationship_kind"] == "contains"
                    && relationship["source_element_id"] == current.as_str()
                {
                    let target = relationship["target_element_id"]
                        .as_str()
                        .unwrap_or_default();
                    if target == descendant {
                        return true;
                    }
                    frontier.push(target.to_owned());
                }
            }
        }
        false
    }

    /// One complete published snapshot. Tombstones are included as stored, so partitioning must filter them.
    fn snapshot(&self, input: &Value) -> Value {
        let root = input["scope"]["project_root"].as_str().unwrap_or_default();
        let elements: Vec<&Value> = self
            .elements
            .values()
            .filter(|element| element["project_root"] == root)
            .collect();
        let relationships: Vec<&Value> = self
            .relationships
            .values()
            .filter(|relationship| relationship["project_root"] == root)
            .collect();
        json!({"commit_version": self.commit_version, "published_at": "2026-10-20T00:00:00Z",
            "project_root": root, "elements": elements, "relationships": relationships,
            "artifacts": self.scoped_artifacts(root, input["artifact_namespace"].as_str())})
    }

    /// Artifacts as the owner returns them: the snapshot scope is injected into every record.
    fn scoped_artifacts(&self, root: &str, namespace: Option<&str>) -> Vec<Value> {
        self.artifacts_in(root, namespace)
            .into_iter()
            .map(|mut artifact| {
                artifact["project_root"] = json!(root);
                artifact
            })
            .collect()
    }

    /// One selective subgraph: the root, its direct neighbours by any edge, and every edge and
    /// One selective subgraph as the owner returns it: the anchor's complete subtree through
    /// `parent_element_id`, every edge touching that subtree, and the outside endpoints of those
    /// edges as `external_elements`. Knowledge document nodes arrive as externals and must be dropped.
    fn selective_subgraph(&self, input: &Value) -> Value {
        let root = input["project_root"].as_str().unwrap_or_default();
        let anchor = input["root_element_id"].as_str().unwrap_or_default();
        let mut subtree = BTreeSet::from([anchor.to_owned()]);
        loop {
            let before = subtree.len();
            for element in self.elements.values() {
                if element["parent_element_id"]
                    .as_str()
                    .is_some_and(|parent| subtree.contains(parent))
                {
                    subtree.insert(
                        element["semantic_element_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    );
                }
            }
            if subtree.len() == before {
                break;
            }
        }
        let inside = |id: &Value| subtree.contains(id.as_str().unwrap_or_default());
        let elements: Vec<&Value> = self
            .elements
            .values()
            .filter(|element| {
                element["project_root"] == root
                    && element["lifecycle"] != "inactive"
                    && inside(&element["semantic_element_id"])
            })
            .collect();
        let relationships: Vec<&Value> = self
            .relationships
            .values()
            .filter(|relationship| {
                relationship["project_root"] == root
                    && (inside(&relationship["source_element_id"])
                        || inside(&relationship["target_element_id"]))
            })
            .collect();
        let external_elements: Vec<&Value> = self
            .elements
            .values()
            .filter(|element| {
                element["project_root"] == root
                    && !inside(&element["semantic_element_id"])
                    && relationships.iter().any(|edge| {
                        edge["source_element_id"] == element["semantic_element_id"]
                            || edge["target_element_id"] == element["semantic_element_id"]
                    })
            })
            .collect();
        let artifacts: Vec<Value> = self
            .scoped_artifacts(root, input["artifact_namespace"].as_str())
            .into_iter()
            .filter(|artifact| inside(&artifact["semantic_element_id"]))
            .collect();
        json!({"subgraph": {"commit_version": self.commit_version, "published_at": "2026-10-20T00:00:00Z",
            "project_root": root, "root_element_id": anchor, "elements": elements,
            "relationships": relationships, "artifacts": artifacts,
            "external_elements": external_elements}})
    }

    /// The owner's complete snapshot plus named source records, so callers keep their own source fixtures.
    pub(crate) fn snapshot_with_sources(
        &self,
        input: &Value,
        sources: &[Value],
        relationships: &[Value],
    ) -> Value {
        let mut snapshot = self.snapshot(input);
        snapshot["elements"]
            .as_array_mut()
            .unwrap()
            .extend(sources.iter().cloned());
        snapshot["relationships"]
            .as_array_mut()
            .unwrap()
            .extend(relationships.iter().cloned());
        snapshot
    }

    /// Delegates one publication to the real Semantic plugin and captures its accepted view.
    pub(crate) fn ingest(&mut self, batch: &Value) -> Result<Value, PluginError> {
        self.seed_fixture()?;
        self.take_fault("publication")?;
        if batch["semantic_artifacts"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
        {
            self.take_fault("artifact")?;
        }
        let output = self.persistence.ingest(batch.clone())?;
        if output["accepted"] == true {
            self.writes += 1;
            self.artifact_writes += batch["semantic_artifacts"].as_array().map_or(0, Vec::len);
            self.published_document_ids.extend(
                batch["semantic_elements"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|element| {
                        element["metadata"]["knowledge_document"]["document_id"].as_str()
                    })
                    .map(str::to_owned),
            );
            self.operations.push(format!(
                "publish:{}",
                batch["replace_paths"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            self.reload_fixture()?;
        }
        Ok(json!({"output":output}))
    }
}

fn fixture_rows(rows: &BTreeMap<String, Value>, root: &str) -> Vec<Value> {
    rows.values()
        .filter(|row| row["project_root"] == root)
        .cloned()
        .collect()
}
