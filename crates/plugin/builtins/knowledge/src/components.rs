use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::semantic_context::{SemanticElement, SemanticRelationship};

/// Depth cap for the ancestor walk: guards against a malformed parent cycle
/// in the stored graph instead of looping forever.
const MAX_ANCESTOR_WALK: usize = 64;

#[derive(Clone)]
pub(crate) struct ComponentGroup {
    pub label: String,
    pub elements: Vec<SemanticElement>,
}

#[derive(Clone, Debug)]
pub(crate) struct ComponentRelationship {
    pub source: usize,
    pub target: usize,
    pub kind: String,
    pub count: usize,
}

pub(crate) fn scoped_component_groups(
    elements: &[SemanticElement],
    target: &SemanticElement,
    relationships: &[SemanticRelationship],
) -> Vec<ComponentGroup> {
    let immediate = elements
        .iter()
        .filter(|item| {
            item.parent_element_id.as_deref() == Some(target.semantic_element_id.as_str())
        })
        .filter(report_element)
        .cloned()
        .collect::<Vec<_>>();
    let child_ids = immediate
        .iter()
        .map(|item| item.semantic_element_id.clone())
        .collect::<HashSet<_>>();
    let descendants = descendant_ids(elements, &target.semantic_element_id);
    let external_ids = one_hop_external_ids(
        elements,
        relationships,
        &child_ids,
        &descendants,
        &target.semantic_element_id,
    );
    let external = elements
        .iter()
        .filter(|item| external_ids.contains(&item.semantic_element_id))
        .cloned();
    let mut groups = immediate
        .into_iter()
        .chain(external)
        .map(single_element_group)
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .elements
            .len()
            .cmp(&left.elements.len())
            .then_with(|| left.label.cmp(&right.label))
    });
    groups
}

/// Nearest ancestor (self included) at component scale. Real call graphs bind
/// `function`/`method` elements, while a C4 container view is drawn in
/// files/modules/folders, so an endpoint has to be lifted to the thing the
/// diagram actually draws before it can match a component.
fn component_scale_id<'a>(
    parents: &BTreeMap<&'a str, &'a str>,
    kinds: &BTreeMap<&'a str, &'a str>,
    id: &'a str,
) -> &'a str {
    let mut current = id;
    for _ in 0..MAX_ANCESTOR_WALK {
        if matches!(
            kinds.get(current).copied().unwrap_or_default(),
            "file" | "module" | "folder"
        ) {
            return current;
        }
        match parents.get(current) {
            Some(parent) => current = parent,
            None => return current,
        }
    }
    current
}

fn parent_index(elements: &[SemanticElement]) -> BTreeMap<&str, &str> {
    elements
        .iter()
        .filter_map(|element| {
            element
                .parent_element_id
                .as_deref()
                .map(|parent| (element.semantic_element_id.as_str(), parent))
        })
        .collect()
}

fn kind_index(elements: &[SemanticElement]) -> BTreeMap<&str, &str> {
    elements
        .iter()
        .map(|element| {
            (
                element.semantic_element_id.as_str(),
                element.element_kind.as_str(),
            )
        })
        .collect()
}

/// Component lookup by source path. A file component owns every element whose
/// path equals it; a folder/module component owns everything beneath it.
fn component_paths(groups: &[ComponentGroup]) -> BTreeMap<&str, usize> {
    let mut paths = BTreeMap::new();
    for (position, group) in groups.iter().enumerate() {
        for element in &group.elements {
            paths.entry(element.path.as_str()).or_insert(position);
        }
    }
    paths
}

fn component_position_for_path(path: &str, by_path: &BTreeMap<&str, usize>) -> Option<usize> {
    by_path.get(path).copied().or_else(|| {
        by_path
            .iter()
            .filter(|(candidate, _)| {
                path.starts_with(*candidate) && path.as_bytes().get(candidate.len()) == Some(&b'/')
            })
            .max_by_key(|(candidate, _)| candidate.len())
            .map(|(_, position)| *position)
    })
}

/// Source path embedded in a semantic element id, shaped
/// `<source-kind>:<source-id>:<path>:<element-kind>:<name>:`. Relationship
/// endpoints are frequently outside the report's element budget, so this is
/// the only place their owning file is still recoverable. Returns None for
/// any id that does not carry the expected shape.
pub(crate) fn element_path_from_id(id: &str) -> Option<&str> {
    let mut parts = id.split(':');
    let _source_kind = parts.next()?;
    let _source_id = parts.next()?;
    let path = parts.next()?;
    // The id must still carry an element kind after the path, otherwise this
    // is not the convention and guessing would invent relationships.
    parts.next()?;
    (!path.is_empty()).then_some(path)
}

/// Structural containment, in either the kind or the label the indexer used.
fn is_containment(relationship: &SemanticRelationship) -> bool {
    let kind = relationship.relationship_kind.trim();
    let label = relationship.label.trim();
    kind.eq_ignore_ascii_case("contains") || label.eq_ignore_ascii_case("contains")
}

pub(crate) fn component_relationships(
    groups: &[ComponentGroup],
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
) -> Vec<ComponentRelationship> {
    let index = component_index(groups);
    let parents = parent_index(elements);
    let by_path = component_paths(groups);
    // Resolve an endpoint to the component that owns it. Call graphs bind
    // functions while components are files/modules/folders, so the endpoint
    // itself is rarely a component; and the scope carries a bounded element
    // budget that drops most functions, so a parent chain is often absent
    // too. The element id embeds the owning path, which survives both.
    let owner = |id: &str| -> Option<usize> {
        if let Some(position) = index.get(id) {
            return Some(*position);
        }
        let mut current = id;
        for _ in 0..MAX_ANCESTOR_WALK {
            let Some(parent) = parents.get(current) else {
                break;
            };
            if let Some(position) = index.get(*parent) {
                return Some(*position);
            }
            current = parent;
        }
        component_position_for_path(element_path_from_id(id)?, &by_path)
    };
    let mut grouped = BTreeMap::<(usize, usize, String), usize>::new();
    for relationship in relationships {
        if is_containment(relationship) {
            // Containment is structure, not a C4 relationship — the component
            // list already states it. Drawing it also lets a container and the
            // declarations sharing its path resolve to each other and produce
            // edges that mean nothing.
            continue;
        }
        let (Some(source), Some(target)) = (
            owner(&relationship.source_element_id),
            owner(&relationship.target_element_id),
        ) else {
            continue;
        };
        if source == target {
            // A component's internal calls are not a C4 relationship; drawn as
            // self-loops they would also crowd out every real edge.
            continue;
        }
        let key = (source, target, relationship_label(relationship));
        *grouped.entry(key).or_default() += 1;
    }
    let mut values = grouped
        .into_iter()
        .map(|((source, target, kind), count)| ComponentRelationship {
            source,
            target,
            kind,
            count,
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        (left.source == left.target)
            .cmp(&(right.source == right.target))
            .then(left.source.cmp(&right.source))
            .then(left.target.cmp(&right.target))
            .then(left.kind.cmp(&right.kind))
    });
    values
}

fn component_index(groups: &[ComponentGroup]) -> BTreeMap<String, usize> {
    let mut index = BTreeMap::new();
    for (position, group) in groups.iter().enumerate() {
        for element in &group.elements {
            index.insert(element.semantic_element_id.clone(), position);
        }
    }
    index
}

fn single_element_group(element: SemanticElement) -> ComponentGroup {
    ComponentGroup {
        label: element.name.clone(),
        elements: vec![element],
    }
}

/// Neighbours one hop outside the target subtree. Endpoints are compared and
/// returned at component scale so a function-level `calls` edge still
/// surfaces the file it reaches into (and the file that reaches in), instead
/// of matching nothing.
pub(crate) fn one_hop_external_ids(
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    child_ids: &HashSet<String>,
    descendants: &HashSet<String>,
    target_id: &str,
) -> BTreeSet<String> {
    let parents = parent_index(elements);
    let kinds = kind_index(elements);
    let mut candidates = elements
        .iter()
        .filter(|element| {
            child_ids.contains(&element.semantic_element_id)
                || matches!(element.element_kind.as_str(), "file" | "module" | "folder")
        })
        .cloned()
        .map(single_element_group)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.label.cmp(&right.label));
    let candidate_index = component_index(&candidates);
    let candidate_paths = component_paths(&candidates);
    let child_paths = elements
        .iter()
        .filter(|element| child_ids.contains(&element.semantic_element_id))
        .enumerate()
        .map(|(position, element)| (element.path.as_str(), position))
        .collect::<BTreeMap<_, _>>();
    let internal = |id: &str| -> bool {
        if child_ids.contains(id) {
            return true;
        }
        let mut current = id;
        for _ in 0..MAX_ANCESTOR_WALK {
            let Some(parent) = parents.get(current) else {
                break;
            };
            if child_ids.contains(*parent) {
                return true;
            }
            current = parent;
        }
        if !matches!(
            kinds.get(target_id).copied(),
            Some("folder" | "directory" | "file")
        ) {
            return false;
        }
        element_path_from_id(id)
            .and_then(|path| component_position_for_path(path, &child_paths))
            .is_some()
    };
    relationships
        .iter()
        .filter_map(|item| {
            let source = item.source_element_id.as_str();
            let target = item.target_element_id.as_str();
            match (internal(source), internal(target)) {
                (true, false) => Some(target),
                (false, true) => Some(source),
                _ => None,
            }
        })
        .map(|endpoint| {
            let scaled = component_scale_id(&parents, &kinds, endpoint);
            if candidate_index.contains_key(scaled) || kinds.contains_key(scaled) {
                return scaled.to_owned();
            }
            element_path_from_id(scaled)
                .or_else(|| element_path_from_id(endpoint))
                .and_then(|path| component_position_for_path(path, &candidate_paths))
                .and_then(|position| candidates[position].elements.first())
                .map(|element| element.semantic_element_id.clone())
                .unwrap_or_else(|| scaled.to_owned())
        })
        .filter(|endpoint| *endpoint != target_id)
        .filter(|endpoint| !descendants.contains(endpoint))
        .collect()
}

fn descendant_ids(elements: &[SemanticElement], target_id: &str) -> HashSet<String> {
    let mut descendants = HashSet::new();
    loop {
        let before = descendants.len();
        for element in elements {
            let Some(parent) = element.parent_element_id.as_deref() else {
                continue;
            };
            if parent == target_id || descendants.contains(parent) {
                descendants.insert(element.semantic_element_id.clone());
            }
        }
        if descendants.len() == before {
            return descendants;
        }
    }
}

fn relationship_label(relationship: &SemanticRelationship) -> String {
    let kind = relationship.relationship_kind.trim();
    let label = relationship.label.trim();
    if label.is_empty() || label == kind || label == format!("{kind}s") {
        readable(kind)
    } else {
        readable(label)
    }
}

fn readable(value: &str) -> String {
    value.replace('_', " ")
}

fn report_element(element: &&SemanticElement) -> bool {
    let ignored = [
        ".git/",
        ".lumvise/",
        ".fastembed_cache/",
        "target/",
        "node_modules/",
        "dist/",
        "build/",
        ".next/",
        "coverage/",
        "vendor/",
    ];
    // Documents (markdown notes) decompose into `heading` children; without
    // the kind here a document target drew an empty component map even though
    // every section had a resolved functional summary.
    let kind = matches!(
        element.element_kind.as_str(),
        "folder"
            | "module"
            | "file"
            | "component"
            | "service"
            | "contract"
            | "dataset"
            | "transformation"
            | "report"
            | "metric"
            | "class"
            | "struct"
            | "enum"
            | "interface"
            | "trait"
            | "function"
            | "method"
            | "constructor"
            | "heading"
    );
    kind && !matches!(
        element.path.as_str(),
        "crates" | "tools" | ".fastembed_cache"
    ) && !ignored.iter().any(|prefix| {
        element.path == prefix.trim_end_matches('/') || element.path.starts_with(prefix)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn element(
        id: &str,
        kind: &str,
        name: &str,
        path: &str,
        parent: Option<&str>,
    ) -> SemanticElement {
        SemanticElement {
            project_root: "/repo".into(),
            semantic_element_id: id.into(),
            semantic_source_id: "src".into(),
            path: path.into(),
            element_kind: kind.into(),
            name: name.into(),
            parent_element_id: parent.map(str::to_owned),
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".into(),
            metadata: json!({}),
        }
    }

    fn calls(source: &str, target: &str) -> SemanticRelationship {
        SemanticRelationship {
            project_root: "/repo".into(),
            source_element_id: source.into(),
            target_element_id: target.into(),
            // Real graphs store the verb in `label` under a `semantic` kind.
            relationship_kind: "semantic".into(),
            label: "calls".into(),
            lifecycle: "active".into(),
            metadata: json!({}),
        }
    }

    /// Indexed call graphs bind functions, while the diagram draws files. The
    /// report must still show that one file calls another, aggregated with a
    /// count, instead of dropping every edge.
    #[test]
    fn function_level_calls_roll_up_to_the_files_the_diagram_draws() {
        let elements = vec![
            element("dir", "folder", "src", "src", None),
            element("file:a", "file", "a.rs", "src/a.rs", Some("dir")),
            element("file:b", "file", "b.rs", "src/b.rs", Some("dir")),
            element("fn:a1", "function", "a1", "src/a.rs", Some("file:a")),
            element("fn:a2", "function", "a2", "src/a.rs", Some("file:a")),
            element("fn:b1", "function", "b1", "src/b.rs", Some("file:b")),
        ];
        let relationships = vec![
            calls("fn:a1", "fn:b1"),
            calls("fn:a2", "fn:b1"),
            // Internal call: same component, never a C4 relationship.
            calls("fn:a1", "fn:a2"),
        ];
        let target = elements[0].clone();
        let groups = scoped_component_groups(&elements, &target, &relationships);
        let labels = groups
            .iter()
            .map(|group| group.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["a.rs", "b.rs"], "components are the files");

        let edges = component_relationships(&groups, &elements, &relationships);
        assert_eq!(edges.len(), 1, "one aggregated cross-file edge: {edges:?}");
        assert_eq!(edges[0].kind, "calls");
        assert_eq!(edges[0].count, 2, "both call sites aggregate into one edge");
        assert_eq!(groups[edges[0].source].label, "a.rs");
        assert_eq!(groups[edges[0].target].label, "b.rs");
    }

    /// A markdown document decomposes into heading sections. They must be
    /// drawable components, or every document target renders an empty
    /// Functional Component Map despite resolved section summaries.
    #[test]
    fn document_sections_become_components() {
        let elements = vec![
            element("doc", "file", "CONTEXT.md", "CONTEXT.md", None),
            element("h1", "heading", "Language", "CONTEXT.md", Some("doc")),
            element(
                "h2",
                "heading",
                "Example Dialogue",
                "CONTEXT.md",
                Some("doc"),
            ),
        ];
        let target = elements[0].clone();
        let groups = scoped_component_groups(&elements, &target, &[]);
        let labels = groups
            .iter()
            .map(|group| group.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["Example Dialogue", "Language"]);
    }

    /// A callee outside the target subtree has to appear as a component, or
    /// the "called from / calls out to" half of the map is invisible.
    #[test]
    fn calls_out_of_scope_surface_the_external_file_as_a_component() {
        let elements = vec![
            element("dir", "folder", "src", "src", None),
            element("file:a", "file", "a.rs", "src/a.rs", Some("dir")),
            element("fn:a1", "function", "a1", "src/a.rs", Some("file:a")),
            element("other", "folder", "lib", "lib", None),
            element("file:z", "file", "z.rs", "lib/z.rs", Some("other")),
            element("fn:z1", "function", "z1", "lib/z.rs", Some("file:z")),
        ];
        let relationships = vec![calls("fn:a1", "fn:z1")];
        let target = elements[0].clone();
        let groups = scoped_component_groups(&elements, &target, &relationships);
        assert!(
            groups.iter().any(|group| group.label == "z.rs"),
            "external callee file is a component: {:?}",
            groups.iter().map(|g| g.label.clone()).collect::<Vec<_>>()
        );
        let edges = component_relationships(&groups, &elements, &relationships);
        assert_eq!(edges.len(), 1);
        assert_eq!(groups[edges[0].source].label, "a.rs");
        assert_eq!(groups[edges[0].target].label, "z.rs");
    }

    /// The report carries a bounded element budget (`MAX_C4_ELEMENTS`) and
    /// ranks functions last, so in a real project the call endpoints are
    /// truncated away before the diagram is built. The owning file must still
    /// be recoverable from the endpoint id, or every edge silently vanishes.
    #[test]
    fn calls_still_aggregate_when_endpoints_are_outside_the_element_budget() {
        let elements = vec![
            element("filesystem:h:src:folder:src:", "folder", "src", "src", None),
            element(
                "filesystem:h:src/a.rs:file:a.rs:",
                "file",
                "a.rs",
                "src/a.rs",
                Some("filesystem:h:src:folder:src:"),
            ),
            element(
                "filesystem:h:src/b.rs:file:b.rs:",
                "file",
                "b.rs",
                "src/b.rs",
                Some("filesystem:h:src:folder:src:"),
            ),
        ];
        // Function endpoints are absent from `elements` entirely — exactly
        // what truncation leaves behind.
        let relationships = vec![
            calls(
                "filesystem:h:src/a.rs:function:one:",
                "filesystem:h:src/b.rs:function:two:",
            ),
            calls(
                "filesystem:h:src/a.rs:function:three:",
                "filesystem:h:src/b.rs:function:two:",
            ),
            calls(
                "filesystem:h:src/a.rs:function:one:",
                "filesystem:h:src/a.rs:function:three:",
            ),
        ];
        let target = elements[0].clone();
        let groups = scoped_component_groups(&elements, &target, &relationships);
        let edges = component_relationships(&groups, &elements, &relationships);
        assert_eq!(edges.len(), 1, "one aggregated cross-file edge: {edges:?}");
        assert_eq!(edges[0].count, 2);
        assert_eq!(groups[edges[0].source].label, "a.rs");
        assert_eq!(groups[edges[0].target].label, "b.rs");
    }

    /// An id that does not carry the path convention must never be guessed
    /// into a component.
    #[test]
    fn unconventional_ids_resolve_to_nothing() {
        assert_eq!(
            element_path_from_id("filesystem:h:src/a.rs:file:a.rs:"),
            Some("src/a.rs")
        );
        assert_eq!(element_path_from_id("too:short"), None);
        assert_eq!(element_path_from_id("filesystem:h::file:x:"), None);
    }

    /// Structural containment must never become an edge. A file target whose
    /// children are `mod` declarations shares its path with them, so drawing
    /// containment produced edges between declarations that call nothing.
    #[test]
    fn containment_is_never_drawn_as_a_relationship() {
        let elements = vec![
            element(
                "filesystem:h:src/mod.rs:file:mod.rs:",
                "file",
                "mod.rs",
                "src/mod.rs",
                None,
            ),
            element(
                "filesystem:h:src/mod.rs:module:one:",
                "module",
                "one",
                "src/mod.rs",
                Some("filesystem:h:src/mod.rs:file:mod.rs:"),
            ),
            element(
                "filesystem:h:src/mod.rs:module:two:",
                "module",
                "two",
                "src/mod.rs",
                Some("filesystem:h:src/mod.rs:file:mod.rs:"),
            ),
        ];
        let contains = |source: &str, target: &str| SemanticRelationship {
            project_root: "/repo".into(),
            source_element_id: source.into(),
            target_element_id: target.into(),
            relationship_kind: "contains".into(),
            label: "contains".into(),
            lifecycle: "active".into(),
            metadata: json!({}),
        };
        let relationships = vec![
            contains(
                "filesystem:h:src/mod.rs:file:mod.rs:",
                "filesystem:h:src/mod.rs:module:one:",
            ),
            contains(
                "filesystem:h:src/mod.rs:file:mod.rs:",
                "filesystem:h:src/mod.rs:module:two:",
            ),
        ];
        let target = elements[0].clone();
        let groups = scoped_component_groups(&elements, &target, &relationships);
        let edges = component_relationships(&groups, &elements, &relationships);
        assert!(edges.is_empty(), "containment drawn as edges: {edges:?}");
    }
}
