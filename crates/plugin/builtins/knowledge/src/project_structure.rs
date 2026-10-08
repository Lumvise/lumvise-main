use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use lumvise_plugin_sdk::PluginError;
use serde_json::{Value, json};

use crate::projection_identity::canonical_projection_json;
use crate::semantic_context::{SemanticContext, SemanticElement, SemanticRelationship};
use crate::{KnowledgeArtifact, artifact::knowledge_type_label, functional};

type ChildElements<'a> = BTreeMap<String, Vec<&'a SemanticElement>>;
type ArtifactsByElement<'a> = BTreeMap<String, Vec<&'a KnowledgeArtifact>>;
type RelationshipsBySource<'a> = BTreeMap<String, Vec<&'a SemanticRelationship>>;
type ElementPathHints = BTreeMap<String, String>;
type SemanticLinkTargets = BTreeMap<String, Value>;

pub(crate) fn project(
    project_root: &str,
    semantic: SemanticContext,
    artifacts: Vec<KnowledgeArtifact>,
) -> Result<Value, PluginError> {
    project_selected(project_root, semantic, artifacts, None)
}

pub(crate) fn project_selected(
    project_root: &str,
    semantic: SemanticContext,
    artifacts: Vec<KnowledgeArtifact>,
    selected_element_ids: Option<&BTreeSet<String>>,
) -> Result<Value, PluginError> {
    let elements = semantic
        .elements
        .into_iter()
        .filter(|item| item.lifecycle == "active" && item.project_root == project_root)
        .collect::<Vec<_>>();
    let children = child_element_map(&elements);
    let notes = project_artifacts(&artifacts, &elements);
    let relationships = relationships_by_source(&semantic.relationships, &elements);
    let paths = element_path_hints(&elements);
    let link_targets = semantic_link_targets(&elements, &paths);
    let projected = elements
        .iter()
        .filter(|item| is_filesystem_element(item))
        .filter(|item| {
            selected_element_ids.is_none_or(|ids| ids.contains(&item.semantic_element_id))
        })
        .map(|item| {
            structure_element(
                item,
                &children,
                &notes,
                &relationships,
                &paths,
                &link_targets,
            )
        })
        .collect::<Vec<_>>();
    Ok(json!({"spaces": project_spaces(), "elements": projected}))
}

fn project_spaces() -> Value {
    json!([
        {"spaceId": "project", "title": "Project", "status": "ready", "writePolicy": "annotation"},
        {"spaceId": "implementation", "title": "Implementation", "status": "ready", "writePolicy": "generated"}
    ])
}

fn structure_element(
    element: &SemanticElement,
    children: &ChildElements<'_>,
    artifacts: &ArtifactsByElement<'_>,
    relationships: &RelationshipsBySource<'_>,
    paths: &ElementPathHints,
    available_link_targets: &SemanticLinkTargets,
) -> Value {
    let nested = nested_children(element, children);
    let markdown_children = markdown_children(element, children);
    let markdown = structure_markdown(element, &markdown_children, artifacts, relationships, paths);
    let link_targets = projected_link_targets(
        element,
        &markdown_children,
        relationships,
        available_link_targets,
    );
    let content_hash = projection_content_md5(&markdown, &link_targets);
    let marker = format!("projection:{content_hash}");
    json!({"elementId": element.semantic_element_id, "space": "project",
        "kind": element.element_kind, "title": element.name,
        "contentMd5": content_hash, "markdown": markdown,
        "pathHint": structure_path(element, paths), "sourceId": element.project_root,
        "sourceRefs": [source_ref(element)], "syncToken": marker, "changeMarker": marker,
        "ownership": ownership(), "writePolicy": "annotation",
        "children": projected_children(element, &nested, &markdown_children, paths),
        "artifacts": projected_artifacts(element, &markdown_children, artifacts),
        "linkTargets": link_targets,
        "properties": {}})
}

fn semantic_link_targets(
    elements: &[SemanticElement],
    paths: &ElementPathHints,
) -> SemanticLinkTargets {
    let elements_by_id = elements
        .iter()
        .map(|element| (element.semantic_element_id.as_str(), element))
        .collect::<BTreeMap<_, _>>();
    elements
        .iter()
        .filter_map(|element| {
            let owner = project_note_owner(element, &elements_by_id)?;
            let heading = (!is_filesystem_element(element)).then(|| element.name.clone());
            Some((
                element.semantic_element_id.clone(),
                json!({"elementId": element.semantic_element_id, "title": element.name,
                    "notePathHint": structure_path(owner, paths), "heading": heading}),
            ))
        })
        .collect()
}

fn project_note_owner<'a>(
    element: &'a SemanticElement,
    elements_by_id: &BTreeMap<&str, &'a SemanticElement>,
) -> Option<&'a SemanticElement> {
    let mut current = element;
    let mut visited = BTreeSet::new();
    loop {
        if current.element_kind == "file" {
            return Some(current);
        }
        if !visited.insert(current.semantic_element_id.as_str()) {
            return None;
        }
        current = *elements_by_id.get(current.parent_element_id.as_deref()?)?;
    }
}

fn projected_link_targets(
    element: &SemanticElement,
    descendants: &[&SemanticElement],
    relationships: &RelationshipsBySource<'_>,
    available: &SemanticLinkTargets,
) -> Vec<Value> {
    let page_ids = std::iter::once(element.semantic_element_id.as_str())
        .chain(
            descendants
                .iter()
                .map(|child| child.semantic_element_id.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let referenced_ids = page_ids
        .iter()
        .copied()
        .map(str::to_owned)
        .chain(
            page_ids
                .iter()
                .flat_map(|id| relationships.get(*id).into_iter().flatten())
                .filter(|relationship| import_relation(relationship) || call_relation(relationship))
                .flat_map(|relationship| {
                    [
                        relationship.source_element_id.clone(),
                        relationship.target_element_id.clone(),
                    ]
                }),
        )
        .collect::<BTreeSet<_>>();
    referenced_ids
        .iter()
        .filter_map(|element_id| available.get(element_id).cloned())
        .collect()
}

fn structure_markdown(
    element: &SemanticElement,
    children: &[&SemanticElement],
    artifacts: &ArtifactsByElement<'_>,
    relationships: &RelationshipsBySource<'_>,
    paths: &ElementPathHints,
) -> String {
    let mut markdown = format!("# {}\n\nSource: `{}`", element.name, element.path);
    markdown.push_str("\n\n");
    markdown.push_str(&c4_trigger(element));
    append_relationship_sections(&mut markdown, element, children, relationships);
    if element.element_kind == "file" {
        append_artifact_sections(&mut markdown, "## Artifacts", element, artifacts);
    }
    append_child_sections(
        &mut markdown,
        children,
        artifacts,
        paths,
        element.element_kind == "file",
    );
    markdown
}

fn append_relationship_sections(
    markdown: &mut String,
    element: &SemanticElement,
    children: &[&SemanticElement],
    relationships: &RelationshipsBySource<'_>,
) {
    if element.element_kind != "file" {
        return;
    }
    let ids = BTreeSet::from_iter(
        std::iter::once(element.semantic_element_id.clone())
            .chain(children.iter().map(|item| item.semantic_element_id.clone())),
    );
    append_relationship_section(markdown, "Imports", &ids, relationships, import_relation);
    append_relationship_section(markdown, "Calls", &ids, relationships, call_relation);
}

fn append_relationship_section(
    markdown: &mut String,
    title: &str,
    ids: &BTreeSet<String>,
    relationships: &RelationshipsBySource<'_>,
    predicate: fn(&SemanticRelationship) -> bool,
) {
    let lines = ids
        .iter()
        .flat_map(|id| relationships.get(id).into_iter().flatten())
        .filter(|item| predicate(item))
        .map(|item| relationship_line(item))
        .collect::<BTreeSet<_>>();
    if lines.is_empty() {
        return;
    }
    markdown.push_str(&format!(
        "\n\n## {title}\n\n{}",
        lines.into_iter().collect::<Vec<_>>().join("\n")
    ));
}

fn relationship_line(relationship: &SemanticRelationship) -> String {
    if relationship.label.trim().is_empty() {
        return format!(
            "- `{}` -> `{}`",
            relationship.source_element_id, relationship.target_element_id
        );
    }
    format!(
        "- `{}` -> `{}` ({})",
        relationship.source_element_id,
        relationship.target_element_id,
        markdown_text(&relationship.label)
    )
}

fn import_relation(item: &SemanticRelationship) -> bool {
    relation_kind(item, &["import", "imports", "source_reference"])
        || matches!(
            item.label.to_ascii_lowercase().as_str(),
            "imports" | "references"
        )
}

fn call_relation(item: &SemanticRelationship) -> bool {
    relation_kind(item, &["call", "calls"]) || item.label.eq_ignore_ascii_case("calls")
}

fn relation_kind(item: &SemanticRelationship, kinds: &[&str]) -> bool {
    kinds
        .iter()
        .any(|kind| item.relationship_kind.eq_ignore_ascii_case(kind))
}

fn append_child_sections(
    markdown: &mut String,
    children: &[&SemanticElement],
    artifacts: &ArtifactsByElement<'_>,
    paths: &ElementPathHints,
    inline_artifacts: bool,
) {
    if children.is_empty() {
        return;
    }
    let sections = children
        .iter()
        .map(|child| child_section(child, artifacts, paths, inline_artifacts))
        .collect::<Vec<_>>()
        .join("\n\n");
    markdown.push_str(&format!("\n\n## Semantic Elements\n\n{sections}"));
}

fn child_section(
    child: &SemanticElement,
    artifacts: &ArtifactsByElement<'_>,
    paths: &ElementPathHints,
    inline_artifacts: bool,
) -> String {
    let mut lines = vec![
        format!("### {}", child.name),
        format!("- Kind: `{}`", child.element_kind),
        format!("- Element ID: `{}`", child.semantic_element_id),
        format!(
            "- Project context: {}",
            wikilink(&structure_path(child, paths), &child.name)
        ),
    ];
    if is_filesystem_element(child) {
        lines.push(format!("- C4 action: {}", c4_trigger(child)));
    }
    if !inline_artifacts
        && matches!(child.element_kind.as_str(), "folder" | "directory")
        && artifacts
            .get(&child.semantic_element_id)
            .is_some_and(|items| !items.is_empty())
    {
        lines.push("- Artifacts: `@artifacts`".into());
    }
    let mut markdown = lines.join("\n");
    if inline_artifacts {
        append_artifact_sections(&mut markdown, "#### Artifacts", child, artifacts);
    }
    markdown
}

fn append_artifact_sections(
    markdown: &mut String,
    heading: &str,
    owner: &SemanticElement,
    artifacts: &ArtifactsByElement<'_>,
) {
    let sections = artifacts
        .get(&owner.semantic_element_id)
        .into_iter()
        .flatten()
        .map(|artifact| artifact_summary(artifact))
        .collect::<Vec<_>>();
    if !sections.is_empty() {
        markdown.push_str(&format!("\n\n{heading}\n\n{}", sections.join("\n\n")));
    }
}

fn projected_children(
    element: &SemanticElement,
    nested: &[&SemanticElement],
    descendants: &[&SemanticElement],
    paths: &ElementPathHints,
) -> Vec<Value> {
    let selected = if element.element_kind == "file" {
        descendants
    } else {
        nested
    };
    selected
        .iter()
        .filter(|child| element.element_kind == "file" || is_filesystem_element(child))
        .map(|child| {
            json!({"elementId": child.semantic_element_id, "kind": child.element_kind,
            "title": child.name, "pathHint": structure_path(child, paths),
            "sourceRefs": [source_ref(child)]})
        })
        .collect()
}

fn projected_artifacts(
    file: &SemanticElement,
    descendants: &[&SemanticElement],
    artifacts: &ArtifactsByElement<'_>,
) -> Vec<Value> {
    std::iter::once(file)
        .chain(
            (file.element_kind == "file")
                .then_some(descendants)
                .into_iter()
                .flatten()
                .copied(),
        )
        .flat_map(|owner| {
            artifacts
                .get(&owner.semantic_element_id)
                .into_iter()
                .flatten()
                .map(move |artifact| projected_artifact(owner, artifact))
        })
        .collect()
}

fn projected_artifact(owner: &SemanticElement, artifact: &KnowledgeArtifact) -> Value {
    let markdown = artifact_summary(artifact);
    json!({"artifactId": artifact.artifact_id, "title": artifact.title,
        "markdown": markdown, "ownerElementId": owner.semantic_element_id,
        "ownerTitle": owner.name, "ownerKind": owner.element_kind,
        "contentMd5": content_md5(&markdown)})
}

fn artifact_summary(artifact: &KnowledgeArtifact) -> String {
    let fields = [
        ("Job", "job", "responsibility"),
        ("Receives", "receives", "input"),
        ("Outcome", "outcome", "output"),
        ("Effects", "effects", "side-effect"),
        ("Interface", "source_interface", "interface"),
        ("C4 Level", "c4_level", "architecture"),
        ("Responsibility", "responsibility", "responsibility"),
        ("Interface", "interface", "interface"),
        ("Contains", "contains", "dependency"),
        ("Dependencies", "dependencies", "dependency"),
    ];
    let mut lines = fields
        .iter()
        .filter_map(|(label, key, kind)| detail_section(artifact, label, key, kind))
        .collect::<Vec<_>>();
    if artifact.metadata["job"].as_str().is_none() {
        lines.extend(
            [
                ("Function", "function", "definition"),
                ("Transformation", "transformation", "transformation"),
            ]
            .iter()
            .filter_map(|(label, key, kind)| detail_section(artifact, label, key, kind)),
        );
    }
    if lines.is_empty() {
        return generic_artifact_summary(artifact);
    }
    lines.join("\n\n")
}

fn detail_section(
    artifact: &KnowledgeArtifact,
    label: &str,
    key: &str,
    kind: &str,
) -> Option<String> {
    let content = detail_content(artifact, key)?;
    Some(format!("##### {label} (`{kind}`)\n\n{content}"))
}

fn detail_content(artifact: &KnowledgeArtifact, key: &str) -> Option<String> {
    let value = &artifact.metadata[key];
    if let Some(text) = value.as_str() {
        return Some(if matches!(key, "source_interface" | "interface") {
            format!("```text\n{text}\n```")
        } else {
            markdown_text(text)
        });
    }
    let items = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .filter(|item| useful_array_item(key, item))
        .take(4)
        .map(|item| format!("- {}", markdown_text(item)))
        .collect::<Vec<_>>();
    (!items.is_empty()).then(|| items.join("\n"))
}

fn generic_artifact_summary(artifact: &KnowledgeArtifact) -> String {
    format!(
        "##### {} (`{}`)\n\n{}",
        artifact.title,
        knowledge_type_label(&artifact.knowledge_type),
        artifact.content
    )
}

fn useful_array_item(key: &str, item: &str) -> bool {
    match key {
        "receives" => !item.starts_with("No explicit parameters are indexed;"),
        "effects" => {
            item != "No obvious external side effect is visible from the indexed source span."
        }
        _ => true,
    }
}

fn project_artifacts<'a>(
    artifacts: &'a [KnowledgeArtifact],
    elements: &[SemanticElement],
) -> ArtifactsByElement<'a> {
    let element_ids = elements
        .iter()
        .map(|item| item.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    artifacts
        .iter()
        .filter(|artifact| {
            element_ids.contains(artifact.semantic_element_id.as_str())
                && !crate::obsidian_reports::is_report(artifact)
                && (!artifact
                    .tags
                    .iter()
                    .any(|tag| tag == "cultivation-functional")
                    || functional::is_usable(artifact))
        })
        .fold(BTreeMap::new(), |mut grouped, artifact| {
            let values = grouped
                .entry(artifact.semantic_element_id.clone())
                .or_default();
            values.push(artifact);
            values.sort_by_key(|item| item.artifact_id.as_str());
            grouped
        })
}

fn relationships_by_source<'a>(
    relationships: &'a [SemanticRelationship],
    elements: &[SemanticElement],
) -> RelationshipsBySource<'a> {
    let active = elements
        .iter()
        .map(|item| item.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    relationships
        .iter()
        .filter(|item| active.contains(item.source_element_id.as_str()))
        .fold(BTreeMap::new(), |mut grouped, item| {
            grouped
                .entry(item.source_element_id.clone())
                .or_default()
                .push(item);
            grouped
        })
}

fn child_element_map(elements: &[SemanticElement]) -> ChildElements<'_> {
    elements
        .iter()
        .filter_map(|item| item.parent_element_id.as_ref().map(|parent| (parent, item)))
        .fold(BTreeMap::new(), |mut children, (parent, item)| {
            children.entry(parent.clone()).or_default().push(item);
            children
        })
}

fn nested_children<'a>(
    element: &SemanticElement,
    children: &'a ChildElements<'a>,
) -> Vec<&'a SemanticElement> {
    children
        .get(&element.semantic_element_id)
        .into_iter()
        .flatten()
        .copied()
        .collect()
}

fn markdown_children<'a>(
    element: &SemanticElement,
    children: &'a ChildElements<'a>,
) -> Vec<&'a SemanticElement> {
    if element.element_kind != "file" {
        return nested_children(element, children);
    }
    let mut descendants = Vec::new();
    append_descendants(&mut descendants, element, children);
    descendants
}

fn append_descendants<'a>(
    descendants: &mut Vec<&'a SemanticElement>,
    element: &SemanticElement,
    children: &'a ChildElements<'a>,
) {
    for child in nested_children(element, children) {
        descendants.push(child);
        append_descendants(descendants, child, children);
    }
}

fn element_path_hints(elements: &[SemanticElement]) -> ElementPathHints {
    let counts = elements.iter().fold(BTreeMap::new(), |mut counts, item| {
        *counts.entry(base_path(item)).or_insert(0_usize) += 1;
        counts
    });
    let mut sorted = elements.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| {
        base_path(left)
            .cmp(&base_path(right))
            .then_with(|| is_filesystem_element(right).cmp(&is_filesystem_element(left)))
            .then_with(|| left.semantic_element_id.cmp(&right.semantic_element_id))
    });
    unique_paths(sorted, &counts)
}

fn unique_paths(
    elements: Vec<&SemanticElement>,
    counts: &BTreeMap<String, usize>,
) -> ElementPathHints {
    let mut used = BTreeSet::new();
    elements
        .into_iter()
        .map(|element| {
            let base = base_path(element);
            let path = if (is_filesystem_element(element) || counts[&base] <= 1)
                && used.insert(base.clone())
            {
                base
            } else {
                disambiguated_path(element, &base, &mut used)
            };
            (element.semantic_element_id.clone(), path)
        })
        .collect()
}

fn disambiguated_path(
    element: &SemanticElement,
    base: &str,
    used: &mut BTreeSet<String>,
) -> String {
    let candidate = if is_filesystem_element(element) {
        format!("{base}/{}", safe_segment(&element.semantic_element_id))
    } else {
        format!("{base}/{}", semantic_kind_segment(element))
    };
    if used.insert(candidate.clone()) {
        return candidate;
    }
    let fallback = format!(
        "{candidate}-{:016x}",
        stable_hash_seed(&element.semantic_element_id)
    );
    used.insert(fallback.clone());
    fallback
}

fn structure_path(element: &SemanticElement, paths: &ElementPathHints) -> String {
    paths
        .get(&element.semantic_element_id)
        .cloned()
        .unwrap_or_else(|| base_path(element))
}

fn base_path(element: &SemanticElement) -> String {
    let base = element.path.trim_matches('/');
    if base.is_empty() {
        return safe_segment(&element.semantic_element_id);
    }
    if !is_filesystem_element(element) && base.ends_with(".md") {
        let name = Path::new(base)
            .file_name()
            .and_then(|item| item.to_str())
            .unwrap_or_default();
        return if element.name.trim().is_empty() || element.name == name {
            format!("{base}/{}", semantic_kind_segment(element))
        } else {
            format!("{base}/{}", safe_segment(&element.name))
        };
    }
    let path_name = Path::new(base)
        .file_name()
        .and_then(|item| item.to_str())
        .unwrap_or_default();
    if path_name == element.name || element.name.trim().is_empty() {
        base.into()
    } else {
        format!("{base}/{}", safe_segment(&element.name))
    }
}

fn source_ref(element: &SemanticElement) -> Value {
    json!({"entityKind": "semantic_element", "entityId": element.semantic_element_id,
        "path": element.path, "lineStart": element.start_line, "lineEnd": element.end_line})
}

fn ownership() -> Value {
    json!({"content": "lumvise", "source": "lumvise"})
}
fn content_md5(content: &str) -> String {
    format!("{:x}", md5::compute(content.as_bytes()))
}
fn projection_content_md5(markdown: &str, link_targets: &[Value]) -> String {
    content_md5(&canonical_projection_json(&json!([markdown, link_targets])))
}
fn is_filesystem_element(item: &SemanticElement) -> bool {
    matches!(item.element_kind.as_str(), "file" | "folder" | "directory")
}
fn semantic_kind_segment(item: &SemanticElement) -> String {
    item.start_line.map_or_else(
        || safe_segment(&item.element_kind),
        |line| format!("{}-{line}", safe_segment(&item.element_kind)),
    )
}
fn wikilink(path: &str, label: &str) -> String {
    format!("[[{}|{label}]]", path.trim_end_matches(".md"))
}
fn c4_trigger(item: &SemanticElement) -> String {
    format!(
        "[Generate C4 report](obsidian://lumvise-c4?sourceId={}&elementId={})",
        url_encode(&item.project_root),
        url_encode(&item.semantic_element_id)
    )
}
fn safe_segment(value: &str) -> String {
    value
        .replace(
            [
                '\\', '/', ':', '*', '?', '"', '<', '>', '|', '#', '^', '[', ']',
            ],
            "-",
        )
        .trim_matches([' ', '.', '-'])
        .into()
}
fn url_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}
fn markdown_text(value: &str) -> String {
    value
        .split('`')
        .enumerate()
        .map(|(index, part)| {
            if index % 2 == 1 {
                part.into()
            } else {
                part.replace('<', "&lt;").replace('>', "&gt;")
            }
        })
        .collect::<Vec<_>>()
        .join("`")
}
fn stable_hash_seed(value: &str) -> u64 {
    stable_hash_parts(0xcbf29ce484222325, [value])
}
fn stable_hash_parts<'a>(hash: u64, parts: impl IntoIterator<Item = &'a str>) -> u64 {
    parts.into_iter().fold(hash, |state, value| {
        value.as_bytes().iter().fold(state ^ 0xff, |hash, byte| {
            hash.wrapping_mul(0x100000001b3) ^ u64::from(*byte)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_page_carries_cross_file_relationship_link_targets() {
        let source_file =
            semantic_element("file:src/file.rs", "file", "file.rs", "src/file.rs", None);
        let source_function = semantic_element(
            "fn:src/file.rs:run",
            "function",
            "run",
            "src/file.rs",
            Some("file:src/file.rs"),
        );
        let target_file = semantic_element(
            "file:src/store.rs",
            "file",
            "store.rs",
            "src/store.rs",
            None,
        );
        let target_function = semantic_element(
            "fn:src/store.rs:store",
            "function",
            "store",
            "src/store.rs",
            Some("file:src/store.rs"),
        );
        let semantic = SemanticContext {
            elements: vec![source_file, source_function, target_file, target_function],
            relationships: vec![SemanticRelationship {
                project_root: "/project".into(),
                source_element_id: "fn:src/file.rs:run".into(),
                target_element_id: "fn:src/store.rs:store".into(),
                relationship_kind: "calls".into(),
                label: "calls".into(),
                lifecycle: "active".into(),
                metadata: json!({}),
            }],
            artifacts: Vec::new(),
        };
        let selected = BTreeSet::from(["file:src/file.rs".to_owned()]);

        let projection = project_selected("/project", semantic, Vec::new(), Some(&selected))
            .expect("selected projection");
        let element = &projection["elements"][0];

        assert_eq!(projection["elements"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            element["linkTargets"],
            json!([
                {"elementId": "file:src/file.rs", "title": "file.rs",
                    "notePathHint": "src/file.rs", "heading": null},
                {"elementId": "fn:src/file.rs:run", "title": "run",
                    "notePathHint": "src/file.rs", "heading": "run"},
                {"elementId": "fn:src/store.rs:store", "title": "store",
                    "notePathHint": "src/store.rs", "heading": "store"}
            ])
        );
    }

    fn semantic_element(
        id: &str,
        kind: &str,
        name: &str,
        path: &str,
        parent_id: Option<&str>,
    ) -> SemanticElement {
        SemanticElement {
            project_root: "/project".into(),
            semantic_element_id: id.into(),
            semantic_source_id: "source".into(),
            path: path.into(),
            element_kind: kind.into(),
            name: name.into(),
            parent_element_id: parent_id.map(str::to_owned),
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".into(),
            metadata: json!({}),
        }
    }
}
