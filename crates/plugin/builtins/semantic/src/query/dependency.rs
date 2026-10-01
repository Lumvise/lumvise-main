//! Dependency response presentation. Storage owns coherent record selection;
//! this module owns branch ordering, cycle markers and missing-node views.
use super::{
    active_relationships, children_by_parent, collect_descendants, invalid_input, is_active,
    structural_depth,
};
use crate::{
    models::{DEFAULT_DEPENDENCY_DEPTH, SemanticElement, SemanticRelationship},
    parse, require_non_empty, serialize_response, storage,
};
use lumvise_contracts::{
    RelationshipTreeBranchV2, RelationshipTreeNodeV2, RelationshipTreeRequestV2,
    RelationshipTreeResponseV2, SemanticElementV2, SemanticRelationshipV2,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

pub(crate) fn dependency_tree(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: RelationshipTreeRequestV2 = parse(input, "semantic dependency tree input")?;
    require_non_empty(&request.semantic_element_id, "semantic_element_id")?;
    let direction = dependency_direction(request.direction.as_deref())?;
    let max_depth = structural_depth(request.max_depth, DEFAULT_DEPENDENCY_DEPTH)?;
    let include_descendants = request.include_descendants.unwrap_or(true);
    let include_inactive = request.include_inactive.unwrap_or(false);
    let snapshot = storage::dependency_snapshot::<SemanticElement, SemanticRelationship>(
        context,
        &request.semantic_element_id,
        direction.as_str(),
        max_depth,
        include_descendants,
        include_inactive,
    )?;
    let total_nodes = snapshot.scope_element_count.ok_or_else(|| {
        invalid_input(
            "missing scope_element_count",
            "dependency scoped result count",
        )
    })?;
    let mut elements = snapshot.elements;
    elements.retain(|element| include_inactive || is_active(element));
    let root = elements
        .iter()
        .find(|element| element.semantic_element_id == request.semantic_element_id)
        .cloned()
        .ok_or_else(|| invalid_input(&request.semantic_element_id, "existing semantic element"))?;
    let relationships = active_relationships(snapshot.relationships);
    let descendants = children_by_parent(&relationships);
    let by_id = elements
        .into_iter()
        .map(|element| (element.semantic_element_id.clone(), element))
        .collect::<HashMap<_, _>>();
    let scope = DependencyScope {
        by_id,
        outgoing: relationship_adjacency(&relationships, true),
        incoming: relationship_adjacency(&relationships, false),
        relationships,
        descendants,
        include_descendants,
        max_depth,
    };
    let root_node = dependency_node(
        &request.semantic_element_id,
        0,
        direction,
        &mut HashSet::new(),
        &scope,
    );
    serialize_response(RelationshipTreeResponseV2 {
        project_root: root.project_root,
        semantic_element_id: request.semantic_element_id,
        direction: direction.as_str().into(),
        total_nodes,
        max_depth: scope.max_depth,
        root: root_node,
        commit_version: snapshot.commit_version,
        published_at: snapshot.published_at,
    })
}

#[derive(Clone, Copy)]
enum Direction {
    Dependencies,
    Dependents,
    Both,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dependencies => "dependencies",
            Self::Dependents => "dependents",
            Self::Both => "both",
        }
    }
    fn dependencies(self) -> bool {
        matches!(self, Self::Dependencies | Self::Both)
    }
    fn dependents(self) -> bool {
        matches!(self, Self::Dependents | Self::Both)
    }
}

struct DependencyScope {
    by_id: HashMap<String, SemanticElement>,
    relationships: Vec<SemanticRelationship>,
    outgoing: HashMap<String, Vec<usize>>,
    incoming: HashMap<String, Vec<usize>>,
    descendants: HashMap<String, Vec<String>>,
    include_descendants: bool,
    max_depth: usize,
}

fn relationship_identity(relationship: &SemanticRelationship) -> (String, String, String, String) {
    (
        relationship.source_element_id.clone(),
        relationship.target_element_id.clone(),
        relationship.relationship_kind.clone(),
        relationship.label.clone(),
    )
}

fn dependency_direction(value: Option<&str>) -> Result<Direction, PluginError> {
    match value.unwrap_or("both") {
        "dependencies" => Ok(Direction::Dependencies),
        "dependents" => Ok(Direction::Dependents),
        "both" => Ok(Direction::Both),
        other => Err(invalid_input(other, "dependencies, dependents, or both")),
    }
}

fn dependency_node(
    element_id: &str,
    depth: usize,
    direction: Direction,
    visited: &mut HashSet<String>,
    scope: &DependencyScope,
) -> RelationshipTreeNodeV2 {
    let Some(element) = scope.by_id.get(element_id) else {
        return RelationshipTreeNodeV2 {
            element: missing_element(element_id),
            dependencies: Vec::new(),
            dependents: Vec::new(),
        };
    };
    if depth >= scope.max_depth {
        return RelationshipTreeNodeV2 {
            element: element.view(element.parent_element_id.clone()),
            dependencies: Vec::new(),
            dependents: Vec::new(),
        };
    }
    visited.insert(element_id.to_owned());
    let dependencies = if direction.dependencies() {
        dependency_branches(element_id, true, depth, visited, scope)
    } else {
        Vec::new()
    };
    let dependents = if direction.dependents() {
        dependency_branches(element_id, false, depth, visited, scope)
    } else {
        Vec::new()
    };
    visited.remove(element_id);
    RelationshipTreeNodeV2 {
        element: element.view(element.parent_element_id.clone()),
        dependencies,
        dependents,
    }
}

fn dependency_branches(
    element_id: &str,
    outgoing: bool,
    depth: usize,
    visited: &mut HashSet<String>,
    scope: &DependencyScope,
) -> Vec<RelationshipTreeBranchV2> {
    let ids = dependency_scope_ids(element_id, scope);
    let mut seen = BTreeSet::new();
    let adjacency = if outgoing {
        &scope.outgoing
    } else {
        &scope.incoming
    };
    let candidates = ids
        .iter()
        .flat_map(|id| adjacency.get(id).into_iter().flatten().copied())
        .collect::<BTreeSet<_>>();
    let mut branches = candidates
        .into_iter()
        .map(|index| &scope.relationships[index])
        .filter_map(|relationship| {
            let next = if outgoing && ids.contains(&relationship.source_element_id) {
                Some(relationship.target_element_id.as_str())
            } else if !outgoing && ids.contains(&relationship.target_element_id) {
                Some(relationship.source_element_id.as_str())
            } else {
                None
            }?;
            if !seen.insert(relationship_identity(relationship)) {
                return None;
            }
            let cycle = visited.contains(next);
            let node = if cycle {
                let element = scope.by_id.get(next)?;
                RelationshipTreeNodeV2 {
                    element: element.view(element.parent_element_id.clone()),
                    dependencies: Vec::new(),
                    dependents: Vec::new(),
                }
            } else {
                dependency_node(next, depth + 1, Direction::Both, visited, scope)
            };
            Some(RelationshipTreeBranchV2 {
                relationship: relationship_view(relationship),
                node,
                cycle,
            })
        })
        .collect::<Vec<_>>();
    branches.sort_by(|left, right| {
        left.node
            .element
            .semantic_element_id
            .cmp(&right.node.element.semantic_element_id)
    });
    branches
}

fn dependency_scope_ids(element_id: &str, scope: &DependencyScope) -> HashSet<String> {
    let mut ids = HashSet::from([element_id.to_owned()]);
    if scope.include_descendants {
        collect_descendants(element_id, &scope.descendants, &mut ids);
    }
    ids
}

fn relationship_adjacency(
    relationships: &[SemanticRelationship],
    outgoing: bool,
) -> HashMap<String, Vec<usize>> {
    let mut adjacency = HashMap::<String, Vec<usize>>::new();
    for (index, relationship) in relationships.iter().enumerate() {
        let id = if outgoing {
            &relationship.source_element_id
        } else {
            &relationship.target_element_id
        };
        adjacency.entry(id.clone()).or_default().push(index);
    }
    adjacency
}

fn relationship_view(relationship: &SemanticRelationship) -> SemanticRelationshipV2 {
    SemanticRelationshipV2 {
        source_element_id: relationship.source_element_id.clone(),
        target_element_id: relationship.target_element_id.clone(),
        relationship_kind: relationship.relationship_kind.clone(),
        label: relationship.label.clone(),
        target_label: relationship
            .metadata
            .get("target_label")
            .and_then(Value::as_str)
            .map(str::to_owned),
        target_locator: relationship
            .metadata
            .get("target_locator")
            .and_then(Value::as_str)
            .map(str::to_owned),
        project_root: None,
        lifecycle: None,
        metadata: None,
    }
}

fn missing_element(id: &str) -> SemanticElementV2 {
    SemanticElementV2 {
        semantic_element_id: id.to_owned(),
        element_kind: "missing".into(),
        name: id.to_owned(),
        path: String::new(),
        parent_element_id: None,
        start_line: None,
        end_line: None,
        project_root: None,
        semantic_source_id: None,
        content_fingerprint: None,
        lifecycle: None,
        metadata: None,
    }
}
