use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    KnowledgeArtifact, KnowledgeKind,
    c4_text::{c4_report_path, c4_text, display_label, lazy_c4_link, project_note_link},
    components::{ComponentGroup, component_relationships, scoped_component_groups},
    functional,
    semantic_context::{SemanticElement, SemanticRelationship},
};

const MAX_COMPONENT_JOB_SECTIONS: usize = 12;
const MAX_DIAGRAM_COMPONENTS: usize = 12;
const MAX_DIAGRAM_LINKS: usize = 32;

pub(crate) fn report(
    project_root: &str,
    target: &SemanticElement,
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    artifacts: &[KnowledgeArtifact],
    fingerprint: &str,
) -> KnowledgeArtifact {
    let groups = scoped_component_groups(elements, target, relationships);
    let dependencies = elements
        .iter()
        .map(|element| ArtifactDependency {
            target: ArtifactDependencyTarget::SemanticElement {
                semantic_element_id: element.semantic_element_id.clone(),
            },
        })
        .chain(
            artifacts
                .iter()
                .filter(|item| functional::is_usable(item))
                .map(|item| ArtifactDependency {
                    target: ArtifactDependencyTarget::Artifact {
                        artifact_id: item.artifact_id.clone(),
                    },
                }),
        )
        .collect();
    let jobs = ArtifactJobIndex::from_artifacts(artifacts);
    KnowledgeArtifact {
        artifact_id: report_id(&target.semantic_element_id),
        semantic_element_id: target.semantic_element_id.clone(),
        knowledge_type: KnowledgeKind::Report,
        title: report_title(target),
        content: report_content(&groups, elements, relationships, &jobs),
        tags: vec![
            "nucleus".into(),
            "cultivation-nucleus".into(),
            "cultivation-report".into(),
            "scoped-c4".into(),
        ],
        dependencies,
        metadata: report_metadata(project_root, target, &groups, &jobs, fingerprint),
        path: None,
        project_root: Some(project_root.into()),
    }
}

/// Semantic elements whose functional summaries this C4 report indexes.
pub(crate) fn functional_summary_elements(
    elements: &[SemanticElement],
    target: &SemanticElement,
    relationships: &[SemanticRelationship],
) -> Vec<SemanticElement> {
    scoped_component_groups(elements, target, relationships)
        .into_iter()
        .flat_map(|group| group.elements)
        .collect()
}

pub(crate) fn report_id(target_id: &str) -> String {
    format!(
        "knowledge-cultivation-report-c4-architecture-scoped-{}",
        safe_id(target_id)
    )
}

/// Change key for the report's inputs.
///
/// Elements and artifacts stay itemised — the scope caps them at
/// `MAX_C4_ELEMENTS` — but relationships are unbounded: a wide folder target
/// reaches tens of thousands, and listing them verbatim produced an 11 MB
/// string that was sorted, joined, persisted in metadata and re-serialised on
/// every freshness check. They are folded into one digest instead, which
/// detects exactly the same changes at a bounded size.
pub(crate) fn fingerprint(
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    artifacts: &[KnowledgeArtifact],
) -> String {
    let mut parts = vec!["schema:c4-report-v8".to_owned()];
    parts.extend(elements.iter().map(|item| {
        format!(
            "element:{}:{}",
            item.semantic_element_id,
            item.content_fingerprint.as_deref().unwrap_or("")
        )
    }));
    parts.extend(
        artifacts
            .iter()
            .filter(|item| functional::is_usable(item))
            .map(|item| {
                format!(
                    "artifact:{}:{}",
                    item.artifact_id,
                    functional::functional_content_hash(item)
                )
            }),
    );
    parts.sort();
    parts.push(format!(
        "relationships:{}:{}",
        relationships.len(),
        relationship_digest(relationships)
    ));
    parts.join("|")
}

/// Digest over the relationship set. Per-edge hashes are sorted numerically
/// and mixed in order, so the result is independent of the order the store
/// returned the edges while still reacting to a duplicate, a removal or a
/// changed kind. Sorting 64-bit hashes costs nothing next to sorting the
/// formatted strings themselves.
fn relationship_digest(relationships: &[SemanticRelationship]) -> String {
    let mut hashes = relationships
        .iter()
        .map(|item| {
            functional::stable_content_hash(&format!(
                "{}:{}:{}:{}",
                item.source_element_id, item.target_element_id, item.relationship_kind, item.label
            ))
        })
        .collect::<Vec<_>>();
    hashes.sort_unstable();
    functional::stable_content_hash(&hashes.concat())
}

fn report_content(
    groups: &[ComponentGroup],
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    jobs: &ArtifactJobIndex,
) -> String {
    let mut sections = vec![format!(
        "## Functional Component Map\n\n{}",
        project_structure_diagram(groups, elements, relationships, jobs)
    )];
    let components = component_lines(groups, jobs);
    if !components.is_empty() {
        sections.push(format!("## Component Jobs\n\n{}", components.join("\n\n")));
    }
    sections.join("\n\n")
}

/// One section per component the diagram draws. A component whose functional
/// summary has not been generated yet still gets its section, marked as
/// pending: dropping it made the whole "Component Jobs" heading disappear
/// whenever a freshly scoped folder had no summaries, which reads as a
/// broken report rather than as work still queued.
fn component_lines(groups: &[ComponentGroup], jobs: &ArtifactJobIndex) -> Vec<String> {
    groups
        .iter()
        .take(MAX_COMPONENT_JOB_SECTIONS)
        .map(|group| {
            let job = jobs.component_job(group).map_or_else(
                || {
                    "_Not generated yet — this component's functional summary is still queued._"
                        .to_owned()
                },
                |value| format!("**Job:** {value}"),
            );
            format!("### {}\n\n{}", component_job_link(group), job)
        })
        .collect::<Vec<_>>()
}

fn component_job_link(group: &ComponentGroup) -> String {
    group.elements.first().map_or_else(
        || group.label.clone(),
        |element| project_note_link(element, &group.label),
    )
}

fn report_metadata(
    project_root: &str,
    target: &SemanticElement,
    groups: &[ComponentGroup],
    jobs: &ArtifactJobIndex,
    fingerprint: &str,
) -> Value {
    json!({
        "cultivation_report": true, "cultivation_nucleus": true, "scoped_c4_nucleus": true,
        "nucleus": {"id": "c4-architecture-scoped", "title": report_title(target),
            "artifact_role": "nucleus",
            "mission": "Document a scoped C4 architecture model for the selected semantic element.",
            "missing_functional_element_ids": jobs.missing_component_element_ids(groups),
            "status": "current", "last_observed_change_id": 0,
            "dependency_fingerprint": fingerprint, "target_element_id": target.semantic_element_id,
            "target_path": target.path, "project_root": project_root},
        "obsidian": {"path": c4_report_path(target)},
        "cultivation": {"mode": "synthesize_nucleus", "report_kind": "c4-architecture-scoped"}
    })
}
fn report_title(target: &SemanticElement) -> String {
    if target.name.trim().is_empty() {
        target
            .path
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or("Project")
            .into()
    } else {
        target.name.clone()
    }
}

fn project_structure_diagram(
    groups: &[ComponentGroup],
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    jobs: &ArtifactJobIndex,
) -> String {
    let project_root = groups
        .first()
        .and_then(|group| group.elements.first())
        .map(|item| item.project_root.as_str())
        .unwrap_or_default();
    let mut lines = vec![
        "```mermaid".into(),
        "flowchart LR".into(),
        "  %% C4Container".into(),
    ];
    lines.extend(
        groups
            .iter()
            .take(MAX_DIAGRAM_COMPONENTS)
            .enumerate()
            .map(|(index, group)| {
                let label = linked_group_label(project_root, group);
                jobs.component_job(group).map_or_else(
                    || format!("  c{index}[\"{label}\"]"),
                    |job| {
                        format!(
                            "  c{index}[\"{label}<br/>{}\"]",
                            c4_text(&format!("Job: {job}"))
                        )
                    },
                )
            }),
    );
    let rendered = groups.len().min(MAX_DIAGRAM_COMPONENTS);
    lines.extend(
        component_relationships(groups, elements, relationships)
            .into_iter()
            .filter(|item| item.source < rendered && item.target < rendered)
            .take(MAX_DIAGRAM_LINKS)
            .map(|item| {
                format!(
                    "  c{} -->|{}| c{}",
                    item.source,
                    c4_text(&format!("{} x{}", item.kind, item.count)),
                    item.target
                )
            }),
    );
    lines.push("```".into());
    lines.join("\n")
}

fn linked_group_label(_project_root: &str, group: &ComponentGroup) -> String {
    let target = group
        .elements
        .iter()
        .find(|item| item.element_kind == "folder")
        .or_else(|| {
            group
                .elements
                .iter()
                .find(|item| item.element_kind == "file")
        })
        .or_else(|| group.elements.first());
    target.map_or_else(
        || c4_text(&display_label(group)),
        |element| lazy_c4_link(element, &display_label(group)),
    )
}

struct ArtifactJobIndex {
    by_element_id: HashMap<String, String>,
}

impl ArtifactJobIndex {
    fn from_artifacts(artifacts: &[KnowledgeArtifact]) -> Self {
        let mut ordered = artifacts
            .iter()
            .filter(|item| item.tags.iter().any(|tag| tag == "cultivation-functional"))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|item| {
            (
                item.artifact_id
                    .starts_with("knowledge-cultivation-functional-"),
                item.artifact_id.as_str(),
            )
        });
        let by_element_id = ordered
            .into_iter()
            .filter_map(|item| {
                let job = item.metadata["job"].as_str()?.trim();
                (!functional::is_unresolved_functional_job(job))
                    .then(|| (item.semantic_element_id.clone(), job.into()))
            })
            .collect();
        Self { by_element_id }
    }

    fn component_job(&self, group: &ComponentGroup) -> Option<&str> {
        group
            .elements
            .iter()
            .find_map(|item| self.by_element_id.get(&item.semantic_element_id))
            .map(String::as_str)
    }

    fn missing_component_element_ids(&self, groups: &[ComponentGroup]) -> Vec<String> {
        groups
            .iter()
            .filter(|group| self.component_job(group).is_none())
            .filter_map(|group| group.elements.first())
            .map(|element| element.semantic_element_id.clone())
            .collect()
    }
}

fn safe_id(value: &str) -> String {
    value
        .chars()
        .map(|item| {
            if item.is_ascii_alphanumeric() {
                item
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn relationship(source: &str, target: &str, label: &str) -> SemanticRelationship {
        SemanticRelationship {
            project_root: "/repo".into(),
            source_element_id: source.into(),
            target_element_id: target.into(),
            relationship_kind: "semantic".into(),
            label: label.into(),
            lifecycle: "active".into(),
            metadata: json!({}),
        }
    }

    /// A wide folder target reaches tens of thousands of relationships. Listing
    /// them verbatim produced an 11 MB fingerprint that was sorted, joined,
    /// persisted and re-serialised on every freshness check — which is what
    /// pushed `generate_knowledge` past its invocation deadline.
    #[test]
    fn fingerprint_stays_small_for_a_project_sized_relationship_set() {
        let relationships = (0..44_000)
            .map(|index| {
                relationship(
                    &format!("filesystem:h:crates/a{index}.rs:function:f{index}:"),
                    &format!("filesystem:h:crates/b{index}.rs:function:g{index}:"),
                    "calls",
                )
            })
            .collect::<Vec<_>>();
        let key = fingerprint(&[], &relationships, &[]);
        assert!(key.len() < 512, "fingerprint length {}", key.len());
        assert!(key.contains("relationships:44000:"));
    }

    #[test]
    fn fingerprint_reacts_to_edge_changes_but_not_to_store_order() {
        let one = relationship("a", "b", "calls");
        let two = relationship("b", "c", "calls");
        let forward = fingerprint(&[], &[one.clone(), two.clone()], &[]);
        let reversed = fingerprint(&[], &[two.clone(), one.clone()], &[]);
        assert_eq!(
            forward, reversed,
            "order of the stored edges must not matter"
        );

        for changed in [
            vec![one.clone()],
            vec![one.clone(), two.clone(), two.clone()],
            vec![one.clone(), relationship("b", "c", "uses_type")],
            vec![one.clone(), relationship("b", "d", "calls")],
        ] {
            assert_ne!(
                forward,
                fingerprint(&[], &changed, &[]),
                "a removal, duplicate, changed kind or retarget must move the key"
            );
        }
    }

    #[test]
    fn report_id_matches_canonical_contract() {
        assert_eq!(
            report_id("file:src/a.rs"),
            "knowledge-cultivation-report-c4-architecture-scoped-file-src-a-rs"
        );
    }
}
