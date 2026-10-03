use super::*;
pub(crate) fn project_general(
    rows: BatchedGraphRows,
    request: &SemanticGraphProjectionRequest,
    commit_version: i64,
    published_at: String,
) -> Result<SemanticGraphProjection> {
    let all = rows.all;
    let all_by_id = rows.all_by_id;
    let relationships = rows.relationships;
    let mut elements = all
        .iter()
        .filter(|element| element.lifecycle == "active")
        .filter(|element| in_scope(element, request))
        .filter(|element| {
            matches_granularity(element, request.granularity, request.include_external)
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut visible = elements
        .iter()
        .map(|element| element.semantic_element_id.clone())
        .collect::<HashSet<_>>();
    let scoped_visible = visible.clone();
    if request.include_first_neighbors {
        for neighbor_id in first_neighbor_ids(&visible, &relationships) {
            if let Some(neighbor) = all_by_id.get(&neighbor_id)
                && visible.insert(neighbor_id)
            {
                elements.push(neighbor.clone());
            }
        }
    }

    let parent_by_child = relationships
        .iter()
        .filter(|relationship| is_contains(relationship))
        .map(|relationship| {
            (
                relationship.target_element_id.as_str(),
                relationship.source_element_id.as_str(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut edges = BTreeMap::<String, SemanticGraphEdge>::new();
    for relationship in &relationships {
        let Some(source) =
            visible_ancestor(&relationship.source_element_id, &visible, &parent_by_child)
        else {
            continue;
        };
        let target = visible_ancestor(&relationship.target_element_id, &visible, &parent_by_child);
        let Some(target) = target else {
            if request.include_first_neighbors || !request.include_external {
                continue;
            }
            let Some(external) = all_by_id
                .get(&relationship.target_element_id)
                .filter(|element| is_external(element))
            else {
                continue;
            };
            if visible.insert(external.semantic_element_id.clone()) {
                elements.push(external.clone());
            }
            add_edge(
                &mut edges,
                &source,
                &external.semantic_element_id,
                relationship,
            );
            continue;
        };
        if request.include_first_neighbors
            && !scoped_visible.contains(&source)
            && !scoped_visible.contains(&target)
        {
            continue;
        }
        if source != target {
            add_edge(&mut edges, &source, &target, relationship);
        }
    }
    elements.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.start_line.cmp(&right.start_line))
            .then(left.semantic_element_id.cmp(&right.semantic_element_id))
    });
    let nodes = graph_nodes(&elements, &edges, &rows.artifacts_by_element);
    Ok(SemanticGraphProjection {
        commit_version,
        published_at,
        project_root: request.project_root.clone(),
        nodes,
        edges: edges.into_values().collect(),
    })
}

pub(crate) fn graph_nodes(
    elements: &[ProjectionElement],
    edges: &BTreeMap<String, SemanticGraphEdge>,
    artifacts: &HashMap<String, Vec<ProjectionArtifact>>,
) -> Vec<SemanticGraphNode> {
    let labels = elements
        .iter()
        .map(|element| (element.semantic_element_id.as_str(), element.name.as_str()))
        .collect::<HashMap<_, _>>();
    let parents = edges
        .values()
        .filter(|edge| edge.relationship_kind == "contains")
        .map(|edge| (edge.target.as_str(), edge.source.as_str()))
        .collect::<HashMap<_, _>>();
    let containers = parents.values().copied().collect::<HashSet<_>>();
    let mut strengths = HashMap::<&str, u64>::new();
    for edge in edges.values() {
        *strengths.entry(edge.source.as_str()).or_default() += edge.weight;
        *strengths.entry(edge.target.as_str()).or_default() += edge.weight;
    }
    elements
        .iter()
        .map(|element| {
            let previews = artifacts
                .get(&element.semantic_element_id)
                .map(|items| {
                    let mut items = items.clone();
                    items.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
                    items
                        .into_iter()
                        .map(|artifact| SemanticGraphArtifactPreview {
                            artifact_id: artifact.artifact_id,
                            artifact_kind: artifact.artifact_kind,
                            title: artifact.title,
                            text: artifact.content.or(artifact.searchable_text),
                            content_ref: artifact.content_ref,
                            content_size_bytes: artifact.content_size_bytes,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let parent = parents.get(element.semantic_element_id.as_str()).copied();
            SemanticGraphNode {
                id: element.semantic_element_id.clone(),
                label: element.name.clone(),
                kind: element.element_kind.clone(),
                parent_id: parent.map(str::to_owned),
                parent_label: parent.and_then(|id| labels.get(id).map(|label| (*label).to_owned())),
                depth: depth(&element.semantic_element_id, &parents),
                is_container: containers.contains(element.semantic_element_id.as_str()),
                path: element.path.clone(),
                line_start: element.start_line,
                line_end: element.end_line,
                code_size: code_size(element.start_line, element.end_line),
                connection_strength: strengths
                    .get(element.semantic_element_id.as_str())
                    .copied()
                    .unwrap_or(0),
                semantic_artifact_count: previews.len(),
                comment_count: 0,
                data_semantic_artifact_count: previews.len(),
                summary: format!("{} {}", element.element_kind, element.path),
                artifacts: previews,
                stable_ref: element.semantic_element_id.clone(),
            }
        })
        .collect()
}

pub(crate) fn add_edge(
    edges: &mut BTreeMap<String, SemanticGraphEdge>,
    source: &str,
    target: &str,
    relationship: &SemanticRelationship,
) {
    let id = format!(
        "{source}|{}|{}|{target}",
        relationship.relationship_kind, relationship.label
    );
    if let Some(edge) = edges.get_mut(&id) {
        edge.weight += 1;
        return;
    }
    edges.insert(
        id.clone(),
        SemanticGraphEdge {
            id,
            source: source.to_owned(),
            target: target.to_owned(),
            relationship_label: relationship.label.clone(),
            relationship_kind: relationship.relationship_kind.clone(),
            link_type: relationship.label.clone(),
            weight: 1,
            samples: Vec::new(),
        },
    );
}

pub(crate) fn first_neighbor_ids(
    visible: &HashSet<String>,
    relationships: &[SemanticRelationship],
) -> BTreeSet<String> {
    let mut neighbors = BTreeSet::new();
    for relationship in relationships.iter().filter(|edge| !is_contains(edge)) {
        match (
            visible.contains(&relationship.source_element_id),
            visible.contains(&relationship.target_element_id),
        ) {
            (true, false) => {
                neighbors.insert(relationship.target_element_id.clone());
            }
            (false, true) => {
                neighbors.insert(relationship.source_element_id.clone());
            }
            _ => {}
        }
    }
    neighbors
}

pub(crate) fn visible_ancestor(
    element_id: &str,
    visible: &HashSet<String>,
    parents: &HashMap<&str, &str>,
) -> Option<String> {
    let mut current = element_id;
    let mut seen = HashSet::new();
    loop {
        if visible.contains(current) {
            return Some(current.to_owned());
        }
        if !seen.insert(current) {
            return None;
        }
        current = parents.get(current).copied()?;
    }
}

pub(crate) fn in_scope(
    element: &ProjectionElement,
    request: &SemanticGraphProjectionRequest,
) -> bool {
    let Some(path) = request
        .target_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return true;
    };
    element.path == path
        || request.recursive
            && element
                .path
                .starts_with(&format!("{}/", path.trim_end_matches('/')))
}

pub(crate) fn matches_granularity(
    element: &ProjectionElement,
    granularity: SemanticGraphGranularity,
    include_external: bool,
) -> bool {
    if is_external(element) {
        return include_external;
    }
    let kinds: &[&str] = match granularity {
        SemanticGraphGranularity::File => &["file"],
        SemanticGraphGranularity::Class => &["file", "folder", "class"],
        SemanticGraphGranularity::Function => &["file", "folder", "class", "function"],
        SemanticGraphGranularity::Method => &[
            "file", "folder", "class", "function", "method", "property", "field",
        ],
        SemanticGraphGranularity::Property => return true,
    };
    kinds.contains(&element.element_kind.as_str())
}

pub(crate) fn is_contains(relationship: &SemanticRelationship) -> bool {
    relationship.relationship_kind == "contains" || relationship.label == "contains"
}

pub(crate) fn is_external(element: &ProjectionElement) -> bool {
    element.element_kind == "external" || element.metadata["external"].as_bool() == Some(true)
}

pub(crate) fn depth(id: &str, parents: &HashMap<&str, &str>) -> usize {
    let mut current = parents.get(id).copied();
    let mut seen = HashSet::new();
    let mut depth = 0;
    while let Some(parent) = current {
        if !seen.insert(parent) {
            break;
        }
        depth += 1;
        current = parents.get(parent).copied();
    }
    depth
}

pub(crate) fn code_size(start: Option<i64>, end: Option<i64>) -> usize {
    match (start, end) {
        (Some(start), Some(end)) if end >= start => (end - start + 1) as usize,
        _ => 1,
    }
}
