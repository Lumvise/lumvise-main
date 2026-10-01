use super::*;

pub(crate) fn slice_file_projection(
    canonical: &SemanticGraphProjection,
    request: &SemanticGraphProjectionRequest,
) -> SemanticGraphProjection {
    let scoped = scoped_file_nodes(canonical, request);
    let visible = visible_file_nodes(canonical, request, &scoped);
    let edges = sliced_file_edges(canonical, &visible);
    let nodes = sliced_file_nodes(canonical, &visible, &edges);
    SemanticGraphProjection {
        commit_version: canonical.commit_version,
        published_at: canonical.published_at.clone(),
        project_root: canonical.project_root.clone(),
        nodes,
        edges,
    }
}

pub(super) fn scoped_file_nodes<'a>(
    projection: &'a SemanticGraphProjection,
    request: &SemanticGraphProjectionRequest,
) -> HashSet<&'a str> {
    projection
        .nodes
        .iter()
        .filter(|node| raw_path_in_scope(&node.path, request))
        .filter(|node| node.kind == "file" || request.include_external)
        .map(|node| node.id.as_str())
        .collect()
}

pub(super) fn visible_file_nodes<'a>(
    projection: &'a SemanticGraphProjection,
    request: &SemanticGraphProjectionRequest,
    scoped: &HashSet<&'a str>,
) -> HashSet<&'a str> {
    let external = projection
        .nodes
        .iter()
        .filter(|node| node.kind == "external")
        .map(|node| node.id.as_str())
        .collect::<HashSet<_>>();
    let mut visible = scoped.clone();
    for edge in &projection.edges {
        let source_scoped = scoped.contains(edge.source.as_str());
        let target_scoped = scoped.contains(edge.target.as_str());
        if request.include_first_neighbors && (source_scoped || target_scoped) {
            visible.insert(edge.source.as_str());
            visible.insert(edge.target.as_str());
        } else if request.include_external
            && source_scoped
            && external.contains(edge.target.as_str())
        {
            visible.insert(edge.target.as_str());
        }
    }
    visible
}

pub(super) fn sliced_file_edges(
    projection: &SemanticGraphProjection,
    visible: &HashSet<&str>,
) -> Vec<SemanticGraphEdge> {
    projection
        .edges
        .iter()
        .filter(|edge| {
            visible.contains(edge.source.as_str()) && visible.contains(edge.target.as_str())
        })
        .cloned()
        .collect()
}

pub(super) fn sliced_file_nodes(
    projection: &SemanticGraphProjection,
    visible: &HashSet<&str>,
    edges: &[SemanticGraphEdge],
) -> Vec<SemanticGraphNode> {
    let strengths = file_projection_strengths(edges);
    projection
        .nodes
        .iter()
        .filter(|node| visible.contains(node.id.as_str()))
        .cloned()
        .map(|mut node| {
            node.connection_strength = strengths.get(node.id.as_str()).copied().unwrap_or(0);
            node
        })
        .collect()
}

pub(super) fn file_projection_strengths(edges: &[SemanticGraphEdge]) -> HashMap<&str, u64> {
    let mut strengths = HashMap::new();
    for edge in edges {
        *strengths.entry(edge.source.as_str()).or_default() += edge.weight;
        *strengths.entry(edge.target.as_str()).or_default() += edge.weight;
    }
    strengths
}

pub(super) fn project_file(
    graph: &GrafeoDB,
    request: &SemanticGraphProjectionRequest,
    commit_version: i64,
    published_at: String,
) -> Result<SemanticGraphProjection> {
    let started = Instant::now();
    let rows = read_batched_rows(graph, &request.project_root);
    let file_by_path = rows
        .all
        .iter()
        .filter(|element| element.element_kind == "file")
        .map(|element| (element.path.as_str(), element.semantic_element_id.as_str()))
        .collect::<HashMap<_, _>>();
    let initial_visible = rows
        .all
        .iter()
        .filter(|element| raw_path_in_scope(&element.path, request))
        .filter(|element| element.element_kind == "file" || request.include_external)
        .map(|element| element.semantic_element_id.clone())
        .collect::<HashSet<_>>();
    let (visible, edges) = file_projection_edges(request, &rows, &file_by_path, initial_visible);
    let mut elements = visible
        .iter()
        .filter_map(|element_id| rows.all_by_id.get(element_id).cloned())
        .collect::<Vec<_>>();
    elements.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.start_line.cmp(&right.start_line))
            .then(left.semantic_element_id.cmp(&right.semantic_element_id))
    });
    metrics::histogram!(
        "lumvise_db_renderer_graph_projection_stage_seconds",
        "stage" => "file_scope"
    )
    .record(started.elapsed().as_secs_f64());
    let nodes = graph_nodes(&elements, &edges, &rows.artifacts_by_element);
    Ok(SemanticGraphProjection {
        commit_version,
        published_at,
        project_root: request.project_root.clone(),
        nodes,
        edges: edges.into_values().collect(),
    })
}

pub(super) fn raw_path_in_scope(path: &str, request: &SemanticGraphProjectionRequest) -> bool {
    let Some(target) = request
        .target_path
        .as_deref()
        .map(str::trim)
        .filter(|target| !target.is_empty())
    else {
        return true;
    };
    path == target
        || request.recursive && path.starts_with(&format!("{}/", target.trim_end_matches('/')))
}

pub(super) fn file_projection_edges(
    request: &SemanticGraphProjectionRequest,
    rows: &BatchedGraphRows,
    file_by_path: &HashMap<&str, &str>,
    initial_visible: HashSet<String>,
) -> (HashSet<String>, BTreeMap<String, SemanticGraphEdge>) {
    let mut visible = initial_visible;
    let mut edges = BTreeMap::new();
    for relationship in &rows.relationships {
        add_file_relationship(
            relationship,
            request,
            &rows.all_by_id,
            file_by_path,
            &mut visible,
            &mut edges,
        );
    }
    (visible, edges)
}

pub(super) fn add_file_relationship(
    relationship: &SemanticRelationship,
    request: &SemanticGraphProjectionRequest,
    all_by_id: &HashMap<String, ProjectionElement>,
    file_by_path: &HashMap<&str, &str>,
    visible: &mut HashSet<String>,
    edges: &mut BTreeMap<String, SemanticGraphEdge>,
) {
    let Some(source_element) = all_by_id.get(&relationship.source_element_id) else {
        return;
    };
    let Some(target_element) = all_by_id.get(&relationship.target_element_id) else {
        return;
    };
    let Some(source) = file_endpoint(
        all_by_id,
        file_by_path,
        &source_element.semantic_element_id,
        &source_element.path,
        &source_element.element_kind,
    ) else {
        return;
    };
    let Some(target) = file_endpoint(
        all_by_id,
        file_by_path,
        &target_element.semantic_element_id,
        &target_element.path,
        &target_element.element_kind,
    ) else {
        return;
    };
    let source_scoped = raw_path_in_scope(&source_element.path, request);
    let target_scoped = raw_path_in_scope(&target_element.path, request);
    update_file_visibility(
        request,
        all_by_id,
        source,
        target,
        source_scoped,
        target_scoped,
        visible,
    );
    if source == target || !visible.contains(source) || !visible.contains(target) {
        return;
    }
    add_edge(edges, source, target, relationship);
}

pub(super) fn file_endpoint<'a>(
    all_by_id: &'a HashMap<String, ProjectionElement>,
    file_by_path: &HashMap<&str, &'a str>,
    semantic_id: &str,
    path: &str,
    kind: &str,
) -> Option<&'a str> {
    if kind == "external" {
        return all_by_id
            .get(semantic_id)
            .map(|element| element.semantic_element_id.as_str());
    }
    file_by_path.get(path).copied()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn update_file_visibility(
    request: &SemanticGraphProjectionRequest,
    all_by_id: &HashMap<String, ProjectionElement>,
    source: &str,
    target: &str,
    source_scoped: bool,
    target_scoped: bool,
    visible: &mut HashSet<String>,
) {
    if request.include_first_neighbors && (source_scoped || target_scoped) {
        visible.insert(source.to_owned());
        visible.insert(target.to_owned());
    }
    if request.include_external && source_scoped && all_by_id.get(target).is_some_and(is_external) {
        visible.insert(target.to_owned());
    }
}
