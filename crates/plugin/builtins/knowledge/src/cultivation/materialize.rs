//! Bottom-up source context reuses Knowledge’s canonical document owner.

use super::*;

type BuiltContexts = std::collections::BTreeMap<String, (KnowledgeDocument, bool)>;

struct FunctionalContextCultivation<'a> {
    request: &'a CultivationRequest,
    semantic: &'a SemanticContext,
    elements: Vec<&'a SemanticElement>,
    targets: std::collections::HashMap<String, String>,
    existing: std::collections::HashMap<String, KnowledgeDocument>,
    touching: std::collections::HashMap<&'a str, usize>,
    built: BuiltContexts,
}

/// Build source context from leaves to parents through the canonical document owner.
/// Example: materialize_functional(request, scoped_sources, context) returns saved or reused document IDs.
pub(super) fn materialize_functional(
    request: &CultivationRequest,
    semantic: &SemanticContext,
    context: &mut PluginContext<'_>,
) -> Result<Vec<String>, PluginError> {
    FunctionalContextCultivation::open(request, semantic, context)?.run(context)
}

impl<'a> FunctionalContextCultivation<'a> {
    fn open(
        request: &'a CultivationRequest,
        semantic: &'a SemanticContext,
        context: &mut PluginContext<'_>,
    ) -> Result<Self, PluginError> {
        let elements = bottom_up_elements(semantic);
        let targets = target_document_ids(&request.project_root, &elements, context)?;
        let ids = targets.values().cloned().collect();
        let existing = documents::find_documents(context, &request.project_root, &ids)?
            .into_iter()
            .map(|document| (document.document_id.clone(), document))
            .collect();
        Ok(Self {
            request,
            semantic,
            elements,
            targets,
            existing,
            touching: touching_index(&semantic.relationships),
            built: BuiltContexts::new(),
        })
    }

    fn run(&mut self, context: &mut PluginContext<'_>) -> Result<Vec<String>, PluginError> {
        let mut ids = Vec::new();
        for element in self.elements.clone() {
            let Some((document, reused)) = self.resolve(element, context)? else {
                continue;
            };
            ids.push(document.document_id.clone());
            self.built
                .insert(element.semantic_element_id.clone(), (document, reused));
        }
        ids.sort();
        Ok(ids)
    }

    fn resolve(
        &self,
        element: &SemanticElement,
        context: &mut PluginContext<'_>,
    ) -> Result<Option<(KnowledgeDocument, bool)>, PluginError> {
        let children = child_documents(element, &self.semantic.elements, &self.built);
        if let Some(document) = self.reusable_context(element, &children) {
            return Ok(Some((document, true)));
        }
        let children = children
            .into_iter()
            .map(|(document, _)| document)
            .collect::<Vec<_>>();
        let proposal =
            materialized_document(self.request, self.semantic, element, &children, context);
        Ok(self
            .publish(element, proposal, context)?
            .map(|document| (document, false)))
    }

    fn reusable_context(
        &self,
        element: &SemanticElement,
        children: &[(KnowledgeDocument, bool)],
    ) -> Option<KnowledgeDocument> {
        // A composite depends on its children and can only be reused when every child was reused.
        if children.iter().any(|(_, reused)| !reused) {
            return None;
        }
        functional::reusable_existing(
            &self.request.mode,
            element,
            self.touching
                .get(element.semantic_element_id.as_str())
                .copied()
                .unwrap_or(0),
            self.existing
                .get(&self.targets[&element.semantic_element_id]),
        )
    }

    fn publish(
        &self,
        element: &SemanticElement,
        mut proposal: KnowledgeDocument,
        context: &mut PluginContext<'_>,
    ) -> Result<Option<KnowledgeDocument>, PluginError> {
        if !functional::is_usable(&proposal) {
            return Ok(None);
        }
        proposal.document_id = self.targets[&element.semantic_element_id].clone();
        let expected = self
            .existing
            .get(&proposal.document_id)
            .map(documents::ObservedDocumentRevision::from);
        documents::save_generated(
            context,
            &documents::DocumentGenerator::FunctionalContext.owner(&element.semantic_element_id),
            proposal,
            expected,
        )
        .map(Some)
    }
}

fn bottom_up_elements(semantic: &SemanticContext) -> Vec<&SemanticElement> {
    let mut elements = semantic
        .elements
        .iter()
        .filter(|element| element.lifecycle == "active" && !ignored_path(&element.path))
        .collect::<Vec<_>>();
    let parents = parent_index(&semantic.elements);
    elements.sort_by_key(|element| std::cmp::Reverse(element_depth(element, &parents)));
    elements
}

fn target_document_ids(
    root: &str,
    elements: &[&SemanticElement],
    context: &mut PluginContext<'_>,
) -> Result<std::collections::HashMap<String, String>, PluginError> {
    let history = documents::known_source_documents(context, root)?;
    let sources = crate::source_revisions::SelectedSources::for_documents(context, root, &history)?;
    Ok(elements
        .iter()
        .map(|element| {
            let id = functional::existing_for_source(&history, element, &sources)
                .map(|document| document.document_id.clone())
                .unwrap_or_else(|| functional::document_id_for(element));
            (element.semantic_element_id.clone(), id)
        })
        .collect())
}

fn materialized_document(
    request: &CultivationRequest,
    semantic: &SemanticContext,
    element: &SemanticElement,
    children: &[KnowledgeDocument],
    context: &mut PluginContext<'_>,
) -> KnowledgeDocument {
    if children.is_empty() {
        return functional::leaf_document(
            &request.mode,
            element,
            &semantic.relationships,
            &semantic.artifacts,
            context,
        );
    }
    functional::composite_document(&request.mode, element, &semantic.relationships, children)
}
fn child_documents(
    parent: &SemanticElement,
    elements: &[SemanticElement],
    built: &std::collections::BTreeMap<String, (KnowledgeDocument, bool)>,
) -> Vec<(KnowledgeDocument, bool)> {
    let mut values = elements
        .iter()
        .filter(|item| item.parent_element_id.as_deref() == Some(&parent.semantic_element_id))
        .filter_map(|item| built.get(&item.semantic_element_id).cloned())
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.semantic_element_id.cmp(&right.0.semantic_element_id));
    values
}

type ParentIndex<'a> = std::collections::HashMap<&'a str, Option<&'a str>>;

fn parent_index(elements: &[SemanticElement]) -> ParentIndex<'_> {
    elements
        .iter()
        .map(|item| {
            (
                item.semantic_element_id.as_str(),
                item.parent_element_id.as_deref(),
            )
        })
        .collect()
}

fn element_depth(element: &SemanticElement, parents: &ParentIndex<'_>) -> usize {
    let mut current = element.parent_element_id.as_deref();
    let mut depth = 0;
    while let Some(parent) = current {
        depth += 1;
        current = parents.get(parent).copied().flatten();
    }
    depth
}

/// Touching-relationship counts for every endpoint, computed in one pass
/// instead of rescanning the relationship list per element.
fn touching_index(
    relationships: &[semantic_context::SemanticRelationship],
) -> std::collections::HashMap<&str, usize> {
    let mut counts = std::collections::HashMap::new();
    for relationship in relationships {
        *counts
            .entry(relationship.source_element_id.as_str())
            .or_insert(0) += 1;
        *counts
            .entry(relationship.target_element_id.as_str())
            .or_insert(0) += 1;
    }
    counts
}
fn ignored_path(path: &str) -> bool {
    let path = path.trim_start_matches("./");
    [
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
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}
