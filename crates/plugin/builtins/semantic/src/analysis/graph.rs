use super::{Request, Snapshot, invalid, page};
use lumvise_contracts::{SemanticElementV2, SemanticRelationshipV2};
use lumvise_plugin_sdk::PluginError;
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) struct Filter {
    name: Option<Regex>,
    path: Option<Regex>,
    kind: Option<String>,
}

impl Filter {
    pub(super) fn new(request: &Request) -> Result<Self, PluginError> {
        Ok(Self {
            name: compile(request.name_pattern.as_deref(), request.case_sensitive)?,
            path: compile(request.path_pattern.as_deref(), request.case_sensitive)?,
            kind: request.element_kind.clone(),
        })
    }
    pub(super) fn accepts(&self, element: &SemanticElementV2) -> bool {
        self.name
            .as_ref()
            .is_none_or(|pattern| pattern.is_match(&element.name))
            && self
                .path
                .as_ref()
                .is_none_or(|pattern| pattern.is_match(&element.path))
            && self
                .kind
                .as_ref()
                .is_none_or(|kind| kind == &element.element_kind)
    }
}

fn compile(pattern: Option<&str>, case_sensitive: bool) -> Result<Option<Regex>, PluginError> {
    pattern
        .map(|pattern| {
            RegexBuilder::new(pattern)
                .case_insensitive(!case_sensitive)
                .build()
                .map_err(|error| invalid(pattern, &format!("valid regular expression: {error}")))
        })
        .transpose()
}

pub(super) fn search(request: &Request, snapshot: &Snapshot) -> Result<Value, PluginError> {
    let filter = Filter::new(request)?;
    let query = request.query.to_lowercase();
    let mut rows: Vec<_> = snapshot
        .elements
        .iter()
        .filter(|element| filter.accepts(element))
        .filter_map(|element| {
            let score = rank(element, &query);
            (score > 0).then_some((score, element))
        })
        .collect();
    rows.sort_by(|(a, left), (b, right)| {
        b.cmp(a)
            .then(left.path.cmp(&right.path))
            .then(left.start_line.cmp(&right.start_line))
            .then(left.semantic_element_id.cmp(&right.semantic_element_id))
    });
    Ok(page(
        rows.into_iter()
            .map(|(score, element)| json!({"element":element,"score":score}))
            .collect(),
        request,
    ))
}

pub(super) fn rank(element: &SemanticElementV2, query: &str) -> usize {
    let name = element.name.to_lowercase();
    if query.is_empty() || name == query {
        return 100;
    }
    if name.starts_with(query) {
        return 80;
    }
    if name.contains(query) {
        return 60;
    }
    if element.path.to_lowercase().contains(query) {
        return 20;
    }
    0
}

pub(super) fn selected_edge(edge: &SemanticRelationshipV2, labels: &[String]) -> bool {
    edge.relationship_kind != "contains"
        && edge.label != "contains"
        && (labels.is_empty()
            || labels.contains(&edge.label)
            || labels.contains(&edge.relationship_kind))
}

pub(super) fn adjacency<'a>(
    snapshot: &'a Snapshot,
    labels: &[String],
    direction: &str,
) -> BTreeMap<&'a str, Vec<(&'a str, usize)>> {
    let mut adjacent = BTreeMap::<&str, Vec<(&str, usize)>>::new();
    for (index, edge) in snapshot
        .relationships
        .iter()
        .enumerate()
        .filter(|(_, edge)| selected_edge(edge, labels))
    {
        if direction != "inbound" {
            adjacent
                .entry(&edge.source_element_id)
                .or_default()
                .push((&edge.target_element_id, index));
        }
        if direction != "outbound" {
            adjacent
                .entry(&edge.target_element_id)
                .or_default()
                .push((&edge.source_element_id, index));
        }
    }
    adjacent
}

pub(super) fn trace(
    request: &Request,
    snapshot: &Snapshot,
    roots: &[String],
) -> Result<Value, PluginError> {
    if !matches!(request.direction.as_str(), "inbound" | "outbound" | "both") {
        return Err(invalid(&request.direction, "inbound, outbound or both"));
    }
    let mut traversal = CallTraversal::new(request, snapshot, roots)?;
    let filter = Filter::new(request)?;
    while let Some(current) = traversal.pending.pop_front() {
        traversal.expand(current, request.max_depth, &filter);
    }
    Ok(traversal.response(request, snapshot))
}

struct CallTraversal<'a> {
    elements: BTreeMap<&'a str, &'a SemanticElementV2>,
    adjacent: BTreeMap<&'a str, Vec<(&'a str, usize)>>,
    depths: BTreeMap<&'a str, usize>,
    pending: VecDeque<&'a str>,
    edges: BTreeSet<usize>,
    truncated: bool,
}

impl<'a> CallTraversal<'a> {
    fn new(
        request: &Request,
        snapshot: &'a Snapshot,
        roots: &'a [String],
    ) -> Result<Self, PluginError> {
        let elements: BTreeMap<_, _> = snapshot
            .elements
            .iter()
            .map(|element| (element.semantic_element_id.as_str(), element))
            .collect();
        for root in roots {
            if !elements.contains_key(root.as_str()) {
                return Err(invalid(root, "active element in project"));
            }
        }
        Ok(Self {
            elements,
            adjacent: adjacency(snapshot, &request.relationship_labels, &request.direction),
            depths: roots.iter().map(|root| (root.as_str(), 0)).collect(),
            pending: roots.iter().map(String::as_str).collect(),
            edges: BTreeSet::new(),
            truncated: false,
        })
    }

    fn expand(&mut self, current: &str, max_depth: usize, filter: &Filter) {
        for &(next, index) in self.adjacent.get(current).into_iter().flatten() {
            if !self
                .elements
                .get(next)
                .is_some_and(|element| filter.accepts(element))
            {
                continue;
            }
            if self.depths[current] == max_depth {
                self.truncated |= !self.depths.contains_key(next);
                continue;
            }
            self.edges.insert(index);
            if !self.depths.contains_key(next) {
                self.depths.insert(next, self.depths[current] + 1);
                self.pending.push_back(next);
            }
        }
    }

    fn response(&self, request: &Request, snapshot: &Snapshot) -> Value {
        json!({"elements":self.depths.iter().map(|(id,depth)| json!({"element":self.elements[id],"depth":depth})).collect::<Vec<_>>(),
            "relationships":self.edges.iter().map(|index| &snapshot.relationships[*index]).collect::<Vec<_>>(),
            "truncated":self.truncated,"max_depth":request.max_depth,"direction":request.direction})
    }
}
