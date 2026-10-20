//! Named fake I/O for the real document owner used by Knowledge's close tests.
//!
//! Semantic operations apply atomically per call, like the real owner: a publication
//! (`ingest_index_batch`) replaces its declared partitions and resolves citations, and an
//! artifact upsert requires an active anchor. Writes are counted and ordered so tests can
//! prove staging order, and `fail_after` interrupts exactly one write of a chosen kind.

use crate::KnowledgePlugin;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const INJECTED: &str = "injected_write_failure";

#[derive(Default)]
pub(crate) struct FakeDocumentGraph {
    pub(crate) elements: BTreeMap<String, Value>,
    pub(crate) artifacts: BTreeMap<String, Value>,
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
        match (capability, input["operation"].as_str()) {
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
            // `artifacts_by_ids` is not offered: the host authorizes it for Semantic only.
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
            ("plugin.invoke", _) if input["export_id"] == "ingest_index_batch" => {
                self.ingest(&input["input"])
            }
            _ => Err(PluginError::unknown_capability(capability)),
        }
    }
}

impl FakeDocumentGraph {
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
    pub(crate) fn scoped(&self, input: &Value) -> Value {
        if let Some(location) = input["request"].get("Location") {
            return self.location(location);
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
        self.take_fault("artifact")?;
        let id = artifact["artifact_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let anchor = artifact["semantic_element_id"].as_str().unwrap_or_default();
        if self.active_root(anchor).is_none() {
            return Err(PluginError::new(
                "missing_semantic_element",
                format!("anchor `{anchor}` is not an active semantic element"),
                false,
            ));
        }
        self.artifacts.insert(id.clone(), artifact.clone());
        self.artifact_writes += 1;
        self.operations.push(format!("artifact:{id}"));
        Ok(json!({"artifact_id": id}))
    }

    fn active_root(&self, element_id: &str) -> Option<String> {
        self.elements
            .get(element_id)
            .filter(|element| element["lifecycle"] != "inactive")
            .and_then(|element| element["project_root"].as_str().map(str::to_owned))
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
        json!({"commit_version": self.writes, "published_at": "2026-10-20T00:00:00Z",
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
        json!({"subgraph": {"commit_version": self.writes, "published_at": "2026-10-20T00:00:00Z",
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

    /// Applies one publication: the declared partitions are replaced, then citations resolve
    /// only to active same-project elements, and artifacts only to active anchors.
    pub(crate) fn ingest(&mut self, batch: &Value) -> Result<Value, PluginError> {
        self.take_fault("publication")?;
        let root = batch["project_root"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let partition: BTreeSet<String> = ["replace_paths", "removed_paths"]
            .iter()
            .flat_map(|key| batch[*key].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let incoming: BTreeSet<String> = batch["semantic_elements"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|element| element["semantic_element_id"].as_str().map(str::to_owned))
            .collect();
        let owned: BTreeSet<String> = self
            .elements
            .values()
            .filter(|element| {
                element["project_root"] == root.as_str()
                    && partition.contains(element["path"].as_str().unwrap_or_default())
            })
            .filter_map(|element| element["semantic_element_id"].as_str().map(str::to_owned))
            .collect();
        for id in owned.difference(&incoming) {
            if let Some(element) = self.elements.get_mut(id) {
                element["lifecycle"] = json!("inactive");
            }
        }
        let replaced: BTreeSet<String> = owned.union(&incoming).cloned().collect();
        self.relationships.retain(|_, relationship| {
            !(relationship["project_root"] == root.as_str()
                && replaced.contains(
                    relationship["source_element_id"]
                        .as_str()
                        .unwrap_or_default(),
                ))
        });
        for element in batch["semantic_elements"].as_array().into_iter().flatten() {
            if let Some(id) = element["metadata"]["knowledge_document"]["document_id"].as_str() {
                self.published_document_ids.push(id.into());
            }
            let stored = self.stored_element(batch, element);
            self.elements.insert(
                element["semantic_element_id"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                stored,
            );
        }
        let known: BTreeSet<String> = self
            .elements
            .values()
            .filter(|element| {
                element["project_root"] == root.as_str() && element["lifecycle"] != "inactive"
            })
            .filter_map(|element| element["semantic_element_id"].as_str().map(str::to_owned))
            .collect();
        for relationship in batch["semantic_relationships"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let (Some(source), Some(target)) = (
                relationship["source_element_id"].as_str(),
                relationship["target_element_id"].as_str(),
            ) else {
                continue;
            };
            if !known.contains(source) || !known.contains(target) {
                continue;
            }
            let kind = relationship["relationship_kind"]
                .as_str()
                .unwrap_or_default();
            let label = relationship["relationship_label"]
                .as_str()
                .unwrap_or_default();
            let key = serde_json::to_string(&json!([root, source, target, kind, label])).unwrap();
            self.relationships.insert(
                key,
                json!({"project_root": root, "source_element_id": source,
                    "target_element_id": target, "relationship_kind": kind,
                    "label": label, "lifecycle": "active", "metadata": {}}),
            );
        }
        for artifact in batch["semantic_artifacts"].as_array().into_iter().flatten() {
            let anchor = artifact["semantic_element_id"].as_str().unwrap_or_default();
            if !known.contains(anchor) {
                continue;
            }
            let id = artifact["artifact_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let mut stored = artifact.clone();
            stored["project_root"] = json!(root);
            self.artifacts.insert(id.clone(), stored);
            self.artifact_writes += 1;
            self.operations.push(format!("artifact:{id}"));
        }
        self.writes += 1;
        self.operations.push(format!(
            "publish:{}",
            partition.iter().cloned().collect::<Vec<_>>().join(",")
        ));
        Ok(json!({"output": {"accepted": true}}))
    }

    pub(crate) fn stored_element(&self, batch: &Value, element: &Value) -> Value {
        let mut stored = element.clone();
        stored["project_root"] = batch["project_root"].clone();
        stored["element_kind"] = element["semantic_element_type"].clone();
        stored["name"] = element["semantic_element_name"].clone();
        stored["lifecycle"] = json!("active");
        stored["metadata"] = json!({"provider_instance_id": batch["provider_instance_id"],
            "indexer_metadata": element["metadata"]});
        stored
    }
}
