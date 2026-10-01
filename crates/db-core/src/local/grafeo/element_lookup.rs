//! Compact semantic element lookup by name and project/path. GraphStore owns
//! cache lifetime; callers hydrate only selected semantic records.

use super::graph_rows::{value_non_negative_i64, value_string};
use grafeo::{GrafeoDB, NodeId, Value};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct SemanticElementLookup {
    revision: Option<u64>,
    index: SemanticElementIndex,
    scope_counts: HashMap<(String, bool), (u64, usize)>,
    project_paths: HashMap<String, ProjectPathIndex>,
}

impl SemanticElementLookup {
    pub(super) fn select(
        &mut self,
        graph: &GrafeoDB,
        revision: u64,
        project_root: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<NodeId> {
        if limit == 0 {
            return Vec::new();
        }
        self.ensure_current(graph, revision);
        self.index
            .search(project_root, &ElementNameQuery::new(query), limit)
    }
}

impl SemanticElementLookup {
    pub(super) fn scope_count(
        &mut self,
        graph: &GrafeoDB,
        revision: u64,
        project_root: &str,
        include_inactive: bool,
    ) -> usize {
        let key = (project_root.to_owned(), include_inactive);
        if let Some((cached_revision, count)) = self
            .scope_counts
            .get(&key)
            .filter(|(cached_revision, _)| *cached_revision == revision)
        {
            debug_assert_eq!(*cached_revision, revision);
            return *count;
        }
        let count =
            super::graph_rows::project_member_ids(graph, project_root, include_inactive).len();
        self.scope_counts.insert(key, (revision, count));
        count
    }

    fn ensure_current(&mut self, graph: &GrafeoDB, revision: u64) {
        if self.revision == Some(revision) {
            return;
        }
        self.index = SemanticElementIndex::from_graph(graph);
        self.revision = Some(revision);
    }

    pub(super) fn select_path(
        &mut self,
        graph: &GrafeoDB,
        revision: u64,
        project_root: &str,
        path: &str,
        line: i64,
        include_inactive: bool,
    ) -> Vec<NodeId> {
        let index = self
            .project_paths
            .entry(project_root.to_owned())
            .or_default();
        if index.revision != Some(revision) {
            *index = ProjectPathIndex::from_graph(graph, project_root, revision);
        }
        index
            .paths
            .get(path)
            .into_iter()
            .flatten()
            .filter(|entry| include_inactive || entry.lifecycle == "active")
            .filter(|entry| {
                entry.start_line.is_none_or(|start| start <= line)
                    && entry.end_line.is_none_or(|end| line <= end)
            })
            .map(|entry| entry.node_id)
            .collect()
    }
}

/// Path reads must not build substring postings for every project's names.
/// The stable graph lease supplies the revision used to invalidate this cache.
#[derive(Default)]
struct ProjectPathIndex {
    revision: Option<u64>,
    paths: HashMap<String, Vec<IndexedSemanticElement>>,
}

impl ProjectPathIndex {
    fn from_graph(graph: &GrafeoDB, project_root: &str, revision: u64) -> Self {
        let members = graph
            .find_nodes_by_property("project_root", &Value::from(project_root))
            .into_iter()
            .collect::<HashSet<_>>();
        let ids = graph
            .graph_store()
            .nodes_by_label("SemanticElement")
            .into_iter()
            .filter(|id| members.contains(id))
            .collect();
        let mut paths: HashMap<String, Vec<IndexedSemanticElement>> = HashMap::new();
        for entry in SemanticElementIndex::read_entries(graph, ids) {
            paths
                .entry(entry.normalized_path.clone())
                .or_default()
                .push(entry);
        }
        Self {
            revision: Some(revision),
            paths,
        }
    }
}

#[derive(Default)]
struct SemanticElementIndex {
    entries: Vec<IndexedSemanticElement>,
    all_indices: Vec<usize>,
    names: SubstringPostings,
    identifiers: SubstringPostings,
}

struct IndexedSemanticElement {
    node_id: NodeId,
    project_root: String,
    semantic_id: String,
    normalized_id: String,
    normalized_name: String,
    normalized_path: String,
    lifecycle: String,
    start_line: Option<i64>,
    end_line: Option<i64>,
}

struct ElementNameQuery {
    normalized: String,
    tokens: Vec<String>,
}

impl ElementNameQuery {
    fn new(query: &str) -> Self {
        let normalized = query.trim().to_ascii_lowercase();
        let tokens = normalized
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect();
        Self { normalized, tokens }
    }

    fn score(&self, entry: &IndexedSemanticElement) -> u8 {
        if entry.normalized_name == self.normalized {
            return 4;
        }
        if entry.normalized_name.contains(&self.normalized) {
            return 3;
        }
        if self
            .tokens
            .iter()
            .any(|token| entry.normalized_name.contains(token))
        {
            return 2;
        }
        u8::from(entry.normalized_id.contains(&self.normalized))
    }
}

impl SemanticElementIndex {
    fn from_graph(graph: &GrafeoDB) -> Self {
        let ids = graph.graph_store().nodes_by_label("SemanticElement");
        let mut index = Self::default();
        for entry in Self::read_entries(graph, ids) {
            index.insert(entry);
        }
        index
    }

    fn read_entries(graph: &GrafeoDB, ids: Vec<NodeId>) -> Vec<IndexedSemanticElement> {
        let store = graph.graph_store();
        let keys = [
            "project_root",
            "semantic_element_id",
            "name",
            "lifecycle",
            "semantic_source_id",
            "path",
            "element_kind",
            "start_line",
            "end_line",
        ]
        .map(Into::into);
        let rows = store.get_nodes_properties_selective_batch(&ids, &keys);
        ids.into_iter()
            .zip(rows)
            .filter_map(|(node_id, row)| {
                IndexedSemanticElement::from_properties(node_id, |key| row.get(key))
            })
            .collect()
    }

    fn insert(&mut self, entry: IndexedSemanticElement) {
        let ordinal = self.entries.len();
        if entry.lifecycle != "inactive" {
            self.names.insert(&entry.normalized_name, ordinal);
            self.identifiers.insert(&entry.normalized_id, ordinal);
            self.all_indices.push(ordinal);
        }
        self.entries.push(entry);
    }

    fn search(
        &self,
        project_root: Option<&str>,
        query: &ElementNameQuery,
        limit: usize,
    ) -> Vec<NodeId> {
        let candidates = self.candidate_indices(query);
        let mut ranked = candidates
            .into_iter()
            .filter_map(|ordinal| {
                let entry = &self.entries[ordinal];
                if project_root.is_some_and(|root| root != entry.project_root) {
                    return None;
                }
                let score = query.score(entry);
                (score > 0).then_some((Reverse(score), entry.semantic_id.as_str(), entry.node_id))
            })
            .collect::<Vec<_>>();
        if ranked.len() > limit {
            ranked.select_nth_unstable_by(limit, |a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
            ranked.truncate(limit);
        }
        ranked.sort_unstable_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        ranked.into_iter().map(|(_, _, node_id)| node_id).collect()
    }

    fn candidate_indices(&self, query: &ElementNameQuery) -> HashSet<usize> {
        let mut candidates = self
            .identifiers
            .candidates(&query.normalized, &self.all_indices)
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        for term in std::iter::once(&query.normalized).chain(query.tokens.iter()) {
            candidates.extend(self.names.candidates(term, &self.all_indices));
        }
        candidates
    }
}

impl IndexedSemanticElement {
    fn from_properties<'a>(
        node_id: NodeId,
        property: impl Fn(&str) -> Option<&'a Value>,
    ) -> Option<Self> {
        for key in ["semantic_source_id", "path", "element_kind"] {
            property(key).and_then(value_string)?;
        }
        let semantic_id = property("semantic_element_id").and_then(value_string)?;
        Some(Self {
            node_id,
            project_root: property("project_root").and_then(value_string)?,
            normalized_path: property("path")
                .and_then(value_string)?
                .trim()
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_owned(),
            lifecycle: property("lifecycle")
                .and_then(value_string)
                .unwrap_or_else(|| "active".into()),
            start_line: property("start_line").and_then(value_non_negative_i64),
            end_line: property("end_line").and_then(value_non_negative_i64),
            normalized_id: semantic_id.to_ascii_lowercase(),
            semantic_id,
            normalized_name: property("name")
                .and_then(value_string)?
                .to_ascii_lowercase(),
        })
    }
}

#[derive(Default)]
struct SubstringPostings {
    trigrams: HashMap<[u8; 3], Vec<usize>>,
}

impl SubstringPostings {
    fn insert(&mut self, text: &str, ordinal: usize) {
        let grams = text
            .as_bytes()
            .windows(3)
            .map(|bytes| [bytes[0], bytes[1], bytes[2]])
            .collect::<HashSet<_>>();
        for gram in grams {
            self.trigrams.entry(gram).or_default().push(ordinal);
        }
    }

    fn candidates<'a>(&'a self, term: &str, all: &'a [usize]) -> &'a [usize] {
        let mut shortest = all;
        for bytes in term.as_bytes().windows(3) {
            let Some(posting) = self.trigrams.get(&[bytes[0], bytes[1], bytes[2]]) else {
                return &[];
            };
            if posting.len() < shortest.len() {
                shortest = posting;
            }
        }
        shortest
    }
}

#[cfg(test)]
mod tests;
