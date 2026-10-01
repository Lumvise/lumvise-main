use super::{
    Request, Snapshot,
    graph::{self, Filter},
    page,
};
use lumvise_plugin_sdk::PluginError;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn schema(snapshot: &Snapshot) -> Value {
    let mut kinds = BTreeMap::<&str, usize>::new();
    let mut labels = BTreeMap::<(&str, &str), usize>::new();
    for element in &snapshot.elements {
        *kinds.entry(&element.element_kind).or_default() += 1;
    }
    for edge in &snapshot.relationships {
        *labels
            .entry((&edge.relationship_kind, &edge.label))
            .or_default() += 1;
    }
    json!({"element_kinds":kinds,"relationship_types":labels.into_iter().map(|((kind,label),count)|
        json!({"kind":kind,"label":label,"count":count})).collect::<Vec<_>>(),
        "element_fields":["semantic_element_id","element_kind","name","path","parent_element_id","start_line","end_line","metadata"],
        "resolution":"syntax-derived candidates; runtime observations carry origin=runtime"})
}

pub(super) fn coverage(request: &Request, snapshot: &Snapshot) -> Result<Value, PluginError> {
    let filter = Filter::new(request)?;
    let mut files: Vec<_> = snapshot
        .elements
        .iter()
        .filter(|element| element.element_kind == "file" && filter.accepts(element))
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut statuses = BTreeMap::<String, usize>::new();
    let rows = files.into_iter().map(|file| {
        let extraction = file.metadata.as_ref().and_then(|metadata| metadata.get("indexer_metadata").and_then(|indexer| indexer.get("extraction")))
            .cloned().unwrap_or_else(|| json!({"status":"unknown","reason":"reindex required for coverage metadata"}));
        *statuses.entry(extraction["status"].as_str().unwrap_or("unknown").into()).or_default() += 1;
        json!({"path":file.path,"semantic_element_id":file.semantic_element_id,"extraction":extraction})
    }).collect();
    let mut result = page(rows, request);
    result["status_counts"] = json!(statuses);
    result["note"] = json!(
        "Unresolved references include external dependencies and unknown receiver types; they are not evidence of dead code."
    );
    Ok(result)
}

pub(super) fn metrics(request: &Request, snapshot: &Snapshot) -> Result<Value, PluginError> {
    let connectivity = CallConnectivity::new(request, snapshot)?;
    let cycle_rows = cycles(
        &connectivity.nodes,
        &connectivity.outgoing,
        &connectivity.incoming,
    )
    .into_iter()
    .map(|component| json!(component))
    .collect();
    let dead_code_rows = connectivity
        .dead_candidates(snapshot)
        .into_iter()
        .map(|element| {
            json!({
                "semantic_element_id":element.semantic_element_id,
                "name":element.name,
                "path":element.path,
                "element_kind":element.element_kind,
                "start_line":element.start_line,
            })
        })
        .collect();
    Ok(
        json!({"element_count":connectivity.nodes.len(),"edge_count":connectivity.outgoing.values().map(BTreeSet::len).sum::<usize>(),
        "hotspots":page(connectivity.hotspots(),request),
        "cycles":page(cycle_rows,request),
        "dead_code_candidates":page(dead_code_rows,request),
        "caveat":"Zero incoming indexed calls is only a candidate signal: entrypoints, exports, callbacks, macros and unresolved calls may be live."}),
    )
}

struct CallConnectivity<'a> {
    nodes: BTreeSet<&'a str>,
    outgoing: Adjacency<'a>,
    incoming: Adjacency<'a>,
}

impl<'a> CallConnectivity<'a> {
    fn new(request: &Request, snapshot: &'a Snapshot) -> Result<Self, PluginError> {
        let filter = Filter::new(request)?;
        let nodes = snapshot
            .elements
            .iter()
            .filter(|element| filter.accepts(element))
            .map(|element| element.semantic_element_id.as_str())
            .collect();
        let mut graph = Self {
            nodes,
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
        };
        for edge in snapshot
            .relationships
            .iter()
            .filter(|edge| graph::selected_edge(edge, &request.relationship_labels))
        {
            graph.insert(&edge.source_element_id, &edge.target_element_id);
        }
        Ok(graph)
    }

    fn insert(&mut self, source: &'a str, target: &'a str) {
        if !self.nodes.contains(source) || !self.nodes.contains(target) {
            return;
        }
        self.outgoing.entry(source).or_default().insert(target);
        self.incoming.entry(target).or_default().insert(source);
    }

    fn hotspots(&self) -> Vec<Value> {
        let mut rows: Vec<_> = self
            .nodes
            .iter()
            .map(|id| {
                (
                    *id,
                    self.incoming.get(id).map_or(0, BTreeSet::len),
                    self.outgoing.get(id).map_or(0, BTreeSet::len),
                )
            })
            .collect();
        rows.sort_by_key(|(id, inbound, outbound)| (std::cmp::Reverse(inbound + outbound), *id));
        rows.into_iter().map(|(id,fanin,fanout)| json!({"semantic_element_id":id,"fan_in":fanin,"fan_out":fanout})).collect()
    }

    fn dead_candidates<'s>(
        &self,
        snapshot: &'s Snapshot,
    ) -> Vec<&'s lumvise_contracts::SemanticElementV2> {
        snapshot
            .elements
            .iter()
            .filter(|element| {
                self.nodes.contains(element.semantic_element_id.as_str())
                    && matches!(element.element_kind.as_str(), "function" | "method")
                    && !self
                        .incoming
                        .contains_key(element.semantic_element_id.as_str())
            })
            .collect()
    }
}

type Adjacency<'a> = BTreeMap<&'a str, BTreeSet<&'a str>>;

fn cycles<'a>(
    nodes: &BTreeSet<&'a str>,
    outgoing: &Adjacency<'a>,
    incoming: &Adjacency<'a>,
) -> Vec<Vec<&'a str>> {
    let order = finish_order(nodes, outgoing);
    let mut seen = BTreeSet::new();
    let mut cycles = Vec::new();
    for root in order.into_iter().rev() {
        if seen.contains(root) {
            continue;
        }
        let mut stack = vec![root];
        let mut component = Vec::new();
        while let Some(node) = stack.pop() {
            if !seen.insert(node) {
                continue;
            }
            component.push(node);
            stack.extend(incoming.get(node).into_iter().flatten().copied());
        }
        component.sort_unstable();
        if component.len() > 1
            || outgoing
                .get(root)
                .is_some_and(|neighbors| neighbors.contains(root))
        {
            cycles.push(component);
        }
    }
    cycles.sort();
    cycles
}

fn finish_order<'a>(nodes: &BTreeSet<&'a str>, outgoing: &Adjacency<'a>) -> Vec<&'a str> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for root in nodes {
        let mut stack = vec![(*root, false)];
        while let Some((node, expanded)) = stack.pop() {
            if expanded {
                order.push(node);
                continue;
            }
            if !seen.insert(node) {
                continue;
            }
            stack.push((node, true));
            stack.extend(
                outgoing
                    .get(node)
                    .into_iter()
                    .flatten()
                    .map(|neighbor| (*neighbor, false)),
            );
        }
    }
    order
}
