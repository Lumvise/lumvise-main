use lumvise_contracts::{
    CreateKnowledgeDocumentRequest, KnowledgeDocument, KnowledgeDocumentKind,
    KnowledgeDocumentReference,
};
use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    c4_text::{c4_report_path, c4_text, display_label, lazy_c4_link, project_note_link},
    components::{ComponentGroup, component_relationships, scoped_component_groups},
    documents, functional,
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
    contexts: &[KnowledgeDocument],
    fingerprint: &str,
) -> KnowledgeDocument {
    let groups = scoped_component_groups(elements, target, relationships);
    let references = elements
        .iter()
        .map(|element| KnowledgeDocumentReference::SemanticElement {
            semantic_element_id: element.semantic_element_id.clone(),
        })
        .chain(
            contexts
                .iter()
                .filter(|item| functional::is_usable(item))
                .map(|item| KnowledgeDocumentReference::Document {
                    document_id: item.document_id.clone(),
                }),
        )
        .collect();
    let jobs = FunctionalJobIndex::from_documents(contexts);
    documents::draft_document(CreateKnowledgeDocumentRequest {
        operation_id: None,
        document_id: report_id(&target.semantic_element_id),
        project_root: project_root.into(),
        document_kind: KnowledgeDocumentKind::Report,
        title: report_title(target),
        content: report_content(&groups, elements, relationships, &jobs),
        tags: vec![
            "nucleus".into(),
            "cultivation-nucleus".into(),
            "cultivation-report".into(),
            "scoped-c4".into(),
        ],
        references,
        metadata: report_metadata(project_root, target, &groups, &jobs, fingerprint),
        path: None,
    })
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
/// Whole-project inputs arrive before the report's display cap. Hash the complete
/// sorted key so element and context IDs cannot exceed the graph value limit
/// when this opaque freshness marker is persisted in report metadata.
pub(crate) fn fingerprint(
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    contexts: &[KnowledgeDocument],
) -> String {
    let mut parts = vec!["schema:c4-report-v10".to_owned()];
    parts.extend(elements.iter().map(|item| {
        format!(
            "element:{}:{}",
            item.semantic_element_id,
            item.content_fingerprint.as_deref().unwrap_or("")
        )
    }));
    parts.extend(
        contexts
            .iter()
            .filter(|item| functional::is_usable(item))
            .map(|item| {
                format!(
                    "document:{}:{}",
                    item.document_id,
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
    format!(
        "c4-inputs:{}",
        functional::stable_content_hash(&parts.join("|"))
    )
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
    jobs: &FunctionalJobIndex,
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
fn component_lines(groups: &[ComponentGroup], jobs: &FunctionalJobIndex) -> Vec<String> {
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
    jobs: &FunctionalJobIndex,
    fingerprint: &str,
) -> Value {
    json!({
        "cultivation_report": true, "cultivation_nucleus": true, "scoped_c4_nucleus": true,
        "nucleus": {"id": "c4-architecture-scoped", "title": report_title(target),
            "document_role": "nucleus",
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
    jobs: &FunctionalJobIndex,
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

struct FunctionalJobIndex {
    by_element_id: HashMap<String, String>,
}

impl FunctionalJobIndex {
    fn from_documents(contexts: &[KnowledgeDocument]) -> Self {
        let mut ordered = contexts
            .iter()
            .filter(|item| functional::is_usable(item))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|item| {
            (
                item.document_id
                    .starts_with("knowledge-cultivation-functional-"),
                item.document_id.as_str(),
            )
        });
        let by_element_id = ordered
            .into_iter()
            .filter_map(|item| {
                let job = item.metadata["job"].as_str()?.trim();
                let source = functional::source_element_id(item)?;
                (!functional::is_unresolved_functional_job(job))
                    .then(|| (source.to_owned(), job.into()))
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

    fn element(id: &str, kind: &str, name: &str, parent: Option<&str>) -> SemanticElement {
        SemanticElement {
            revision_dependencies: Vec::new(),
            project_root: "/repo".into(),
            semantic_element_id: id.into(),
            semantic_source_id: "src".into(),
            path: "illustrated.docx".into(),
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

    #[test]
    fn fingerprint_stays_small_for_a_project_sized_element_set() {
        let elements = (0..100_000)
            .map(|index| {
                element(
                    &format!("filesystem:project:crates/application/renderer/workspace/modules/component_{index}.rs:function:component_{index}:"),
                    "function",
                    "component",
                    None,
                )
            })
            .collect::<Vec<_>>();
        let key = fingerprint(&elements, &[], &[]);
        assert!(
            key.len() < 512,
            "fingerprint length {}; expected less than 512",
            key.len()
        );
    }

    #[test]
    fn fingerprint_reacts_to_element_changes_but_not_to_store_order() {
        let one = element("one", "function", "One", None);
        let mut two = element("two", "function", "Two", None);
        let original = fingerprint(&[one.clone(), two.clone()], &[], &[]);
        assert_eq!(original, fingerprint(&[two.clone(), one.clone()], &[], &[]));
        assert_ne!(original, fingerprint(&[one.clone()], &[], &[]));
        assert_ne!(
            original,
            fingerprint(&[one.clone(), two.clone(), two.clone()], &[], &[])
        );
        two.content_fingerprint = Some("changed-content".into());
        assert_ne!(original, fingerprint(&[one, two], &[], &[]));
    }

    /// Converted documents index their sections as `markdown_section` and
    /// figures as `document_image`. Generating knowledge for a docx drew an
    /// empty Functional Component Map with no Component Jobs.
    #[test]
    fn converted_document_report_draws_its_sections() {
        let elements = vec![
            element("doc", "file", "illustrated.docx", None),
            element("s1", "markdown_section", "Field report", Some("doc")),
            element("s2", "markdown_section", "Coastal sensors", Some("s1")),
            element("fig", "document_image", "Figure 1", Some("doc")),
        ];
        let report = report("/repo", &elements[0], &elements, &[], &[], "fp");
        assert!(
            report.content.contains("c0[") && report.content.contains("c1["),
            "diagram draws the document's top-level parts: {}",
            report.content
        );
        assert!(report.content.contains("## Component Jobs"));
        assert!(report.content.contains("Field report"));
        assert!(report.content.contains("Figure 1"));
    }

    #[test]
    fn report_id_matches_canonical_contract() {
        assert_eq!(
            report_id("file:src/a.rs"),
            "knowledge-cultivation-report-c4-architecture-scoped-file-src-a-rs"
        );
    }
}
