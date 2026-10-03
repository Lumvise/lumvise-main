use crate::domain::fingerprint::fingerprint_algorithm;
use crate::domain::fingerprint::{
    fingerprint_hamming_distance, fingerprints_match_exactly, parse_content_fingerprint,
};
use crate::{SemanticElement, SemanticMatchEvidence};
use chrono::Utc;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const MAX_SIMHASH_DISTANCE: u32 = 12;

impl SemanticStructureReconciliation {
    /// Preserves existing identities while reconciling authoritative structure.
    /// Example: `SemanticStructureReconciliation::between(&existing, &incoming)`.
    pub fn between(existing: &[SemanticElement], incoming: &[SemanticElement]) -> Self {
        reconcile_semantic_elements(existing, incoming)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One preserved identity from an authoritative structure reconciliation.
pub struct SemanticIdentityRemap {
    pub incoming_id: String,
    pub resolved_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
/// Engine-neutral structure identity decision; publication belongs to the adapter.
pub struct SemanticStructureReconciliation {
    pub active_elements: Vec<SemanticElement>,
    pub inactive_element_ids: Vec<String>,
    pub remaps: Vec<SemanticIdentityRemap>,
}

pub(crate) fn reconcile_semantic_elements(
    existing_elements: &[SemanticElement],
    incoming_elements: &[SemanticElement],
) -> SemanticStructureReconciliation {
    let index = ExistingElementIndex::new(existing_elements);
    let mut remap_by_incoming = BTreeMap::new();
    let mut remaps = Vec::new();
    let mut active_elements = Vec::new();
    // Exact incoming identities already have owners, including later rows.
    // Fingerprint matching must not steal one and create duplicate snapshot IDs.
    let mut used_reused_ids = element_ids(incoming_elements);
    for incoming in incoming_elements {
        let resolution =
            reconcile_one_element(incoming, &index, &remap_by_incoming, &used_reused_ids);
        active_elements.push(record_resolution_remap(
            resolution,
            &mut remap_by_incoming,
            &mut remaps,
            &mut used_reused_ids,
        ));
    }
    let active_ids = element_ids(&active_elements);
    SemanticStructureReconciliation {
        active_elements,
        inactive_element_ids: inactive_ids(existing_elements, &active_ids),
        remaps,
    }
}

pub(crate) struct IdentityRemapIndex<'a> {
    resolved_by_incoming: HashMap<&'a str, &'a str>,
}

impl<'a> IdentityRemapIndex<'a> {
    pub(crate) fn new(remaps: &'a [SemanticIdentityRemap]) -> Self {
        Self {
            resolved_by_incoming: remaps
                .iter()
                .map(|remap| (remap.incoming_id.as_str(), remap.resolved_id.as_str()))
                .collect(),
        }
    }

    pub(crate) fn resolve(&self, semantic_element_id: &str) -> String {
        self.resolved_by_incoming
            .get(semantic_element_id)
            .copied()
            .unwrap_or(semantic_element_id)
            .to_string()
    }
}

fn reconcile_one_element(
    incoming: &SemanticElement,
    index: &ExistingElementIndex<'_>,
    remap_by_incoming: &BTreeMap<String, String>,
    used_reused_ids: &BTreeSet<String>,
) -> ElementResolution {
    let incoming = incoming.clone();
    if index.has_id(&incoming.semantic_element_id) {
        return ElementResolution::unchanged(incoming);
    }
    find_best_candidate(&incoming, index, remap_by_incoming, used_reused_ids)
        .map(|candidate| ElementResolution::reused(&incoming, candidate))
        .unwrap_or_else(|| ElementResolution::unchanged(incoming))
}

fn record_resolution_remap(
    resolution: ElementResolution,
    remap_by_incoming: &mut BTreeMap<String, String>,
    remaps: &mut Vec<SemanticIdentityRemap>,
    used_reused_ids: &mut BTreeSet<String>,
) -> SemanticElement {
    if let Some(remap) = resolution.remap {
        remap_by_incoming.insert(remap.incoming_id.clone(), remap.resolved_id.clone());
        used_reused_ids.insert(remap.resolved_id.clone());
        remaps.push(remap);
    }
    resolution.element
}

fn find_best_candidate<'a>(
    incoming: &SemanticElement,
    index: &'a ExistingElementIndex<'a>,
    remap_by_incoming: &BTreeMap<String, String>,
    used_reused_ids: &BTreeSet<String>,
) -> Option<MatchCandidate<'a>> {
    index
        .candidates_for(incoming)
        .into_iter()
        .filter(|existing| !used_reused_ids.contains(&existing.semantic_element_id))
        .filter_map(|existing| match_candidate(incoming, existing, remap_by_incoming))
        .min_by_key(|candidate| candidate.sort_key())
}

fn parent_context_matches(
    incoming: &SemanticElement,
    existing: &SemanticElement,
    remap_by_incoming: &BTreeMap<String, String>,
) -> bool {
    let incoming_parent = incoming
        .parent_element_id
        .as_ref()
        .map(|parent_id| remap_by_incoming.get(parent_id).unwrap_or(parent_id));
    incoming_parent == existing.parent_element_id.as_ref()
}

fn match_candidate<'a>(
    incoming: &SemanticElement,
    existing: &'a SemanticElement,
    remap_by_incoming: &BTreeMap<String, String>,
) -> Option<MatchCandidate<'a>> {
    let left = existing.content_fingerprint.as_deref()?;
    let right = incoming.content_fingerprint.as_deref()?;
    if !fingerprint_algorithms_match(incoming, existing) {
        return None;
    }
    let context_score = contextual_score(incoming, existing);
    if fingerprints_match_exactly(left, right) {
        return Some(MatchCandidate::exact(existing, context_score));
    }
    if !parent_context_matches(incoming, existing, remap_by_incoming)
        || !fuzzy_context_supported(incoming, existing)
    {
        return None;
    }
    let distance = fingerprint_hamming_distance(left, right)?;
    (distance <= MAX_SIMHASH_DISTANCE)
        .then(|| MatchCandidate::similar(existing, distance, context_score))
}

fn fingerprint_algorithms_match(left: &SemanticElement, right: &SemanticElement) -> bool {
    matching_fingerprint_algorithm(left) == matching_fingerprint_algorithm(right)
}

fn fuzzy_context_supported(incoming: &SemanticElement, existing: &SemanticElement) -> bool {
    incoming.element_kind == existing.element_kind
        && (incoming.name.eq_ignore_ascii_case(&existing.name)
            || incoming.path == existing.path
            || file_name(&incoming.path) == file_name(&existing.path))
}

fn contextual_score(incoming: &SemanticElement, existing: &SemanticElement) -> u8 {
    u8::from(incoming.element_kind == existing.element_kind) * 16
        + u8::from(incoming.name.eq_ignore_ascii_case(&existing.name)) * 16
        + u8::from(incoming.path == existing.path) * 12
        + u8::from(file_name(&incoming.path) == file_name(&existing.path)) * 8
        + u8::from(incoming.parent_element_id == existing.parent_element_id) * 4
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn element_ids(elements: &[SemanticElement]) -> BTreeSet<String> {
    elements
        .iter()
        .map(|element| element.semantic_element_id.clone())
        .collect()
}

fn inactive_ids(existing: &[SemanticElement], active_ids: &BTreeSet<String>) -> Vec<String> {
    existing
        .iter()
        .filter(|element| !active_ids.contains(&element.semantic_element_id))
        .map(|element| element.semantic_element_id.clone())
        .collect()
}

struct ExistingElementIndex<'a> {
    ids: BTreeSet<String>,
    exact_hash: BTreeMap<(String, String), Vec<&'a SemanticElement>>,
    kind_name: BTreeMap<(String, String, String, String), Vec<&'a SemanticElement>>,
    kind_path: BTreeMap<(String, String, String, String), Vec<&'a SemanticElement>>,
    kind_file: BTreeMap<(String, String, String, String), Vec<&'a SemanticElement>>,
}

impl<'a> ExistingElementIndex<'a> {
    fn new(elements: &'a [SemanticElement]) -> Self {
        let mut index = Self {
            ids: BTreeSet::new(),
            exact_hash: BTreeMap::new(),
            kind_name: BTreeMap::new(),
            kind_path: BTreeMap::new(),
            kind_file: BTreeMap::new(),
        };
        for element in elements {
            index.insert(element);
        }
        index
    }

    fn has_id(&self, semantic_element_id: &str) -> bool {
        self.ids.contains(semantic_element_id)
    }

    fn candidates_for(&self, incoming: &SemanticElement) -> Vec<&'a SemanticElement> {
        let mut candidates = BTreeMap::new();
        if let Some(key) = exact_key(incoming) {
            self.extend(&mut candidates, self.exact_hash.get(&key));
        }
        if let Some(key) = kind_name_key(incoming) {
            self.extend(&mut candidates, self.kind_name.get(&key));
        }
        if let Some(key) = kind_path_key(incoming) {
            self.extend(&mut candidates, self.kind_path.get(&key));
        }
        if let Some(key) = kind_file_key(incoming) {
            self.extend(&mut candidates, self.kind_file.get(&key));
        }
        candidates.into_values().collect()
    }

    fn insert(&mut self, element: &'a SemanticElement) {
        self.ids.insert(element.semantic_element_id.clone());
        insert_candidate(&mut self.exact_hash, exact_key(element), element);
        insert_candidate(&mut self.kind_name, kind_name_key(element), element);
        insert_candidate(&mut self.kind_path, kind_path_key(element), element);
        insert_candidate(&mut self.kind_file, kind_file_key(element), element);
    }

    fn extend(
        &self,
        selected: &mut BTreeMap<String, &'a SemanticElement>,
        candidates: Option<&Vec<&'a SemanticElement>>,
    ) {
        for candidate in candidates.into_iter().flatten() {
            selected.insert(candidate.semantic_element_id.clone(), candidate);
        }
    }
}

fn insert_candidate<'a, K: Ord>(
    index: &mut BTreeMap<K, Vec<&'a SemanticElement>>,
    key: Option<K>,
    element: &'a SemanticElement,
) {
    if let Some(key) = key {
        index.entry(key).or_default().push(element);
    }
}

fn exact_key(element: &SemanticElement) -> Option<(String, String)> {
    let fingerprint = parse_content_fingerprint(element.content_fingerprint.as_deref()?)?;
    Some((element.project_root.clone(), fingerprint.exact_hash))
}

fn kind_name_key(element: &SemanticElement) -> Option<(String, String, String, String)> {
    contextual_key(element, element.name.to_ascii_lowercase())
}

fn kind_path_key(element: &SemanticElement) -> Option<(String, String, String, String)> {
    contextual_key(element, element.path.clone())
}

fn kind_file_key(element: &SemanticElement) -> Option<(String, String, String, String)> {
    contextual_key(element, file_name(&element.path).to_string())
}

fn contextual_key(
    element: &SemanticElement,
    context: String,
) -> Option<(String, String, String, String)> {
    Some((
        element.project_root.clone(),
        matching_fingerprint_algorithm(element)?.to_string(),
        element.element_kind.clone(),
        context,
    ))
}

fn matching_fingerprint_algorithm(element: &SemanticElement) -> Option<&str> {
    fingerprint_algorithm(element).or_else(|| {
        element
            .content_fingerprint
            .as_deref()?
            .split_once(':')
            .map(|(version, _)| version)
    })
}

#[derive(Debug)]
struct ElementResolution {
    element: SemanticElement,
    remap: Option<SemanticIdentityRemap>,
}

impl ElementResolution {
    fn unchanged(element: SemanticElement) -> Self {
        Self {
            element,
            remap: None,
        }
    }

    fn reused(incoming: &SemanticElement, candidate: MatchCandidate<'_>) -> Self {
        let mut element = incoming.clone();
        element.semantic_element_id = candidate.element.semantic_element_id.clone();
        element.match_evidence = candidate.evidence();
        Self {
            element,
            remap: Some(candidate.remap(incoming)),
        }
    }
}

#[derive(Debug)]
struct MatchCandidate<'a> {
    element: &'a SemanticElement,
    distance: Option<u32>,
    confidence: u8,
    context_score: u8,
    reason: &'static str,
}

impl<'a> MatchCandidate<'a> {
    fn exact(element: &'a SemanticElement, context_score: u8) -> Self {
        Self {
            element,
            distance: None,
            confidence: 100,
            context_score,
            reason: "stable_fingerprint_match",
        }
    }

    fn similar(element: &'a SemanticElement, distance: u32, context_score: u8) -> Self {
        Self {
            element,
            distance: Some(distance),
            confidence: 90,
            context_score,
            reason: "similar_rebind",
        }
    }

    fn evidence(&self) -> Option<SemanticMatchEvidence> {
        let distance = self.distance?;
        Some(SemanticMatchEvidence {
            match_confidence: self.confidence,
            simhash_distance: Some(distance),
            matched_at: Utc::now().to_rfc3339(),
            precaution: Some(self.reason.to_string()),
        })
    }

    fn remap(&self, incoming: &SemanticElement) -> SemanticIdentityRemap {
        SemanticIdentityRemap {
            incoming_id: incoming.semantic_element_id.clone(),
            resolved_id: self.element.semantic_element_id.clone(),
            reason: self.reason.to_string(),
        }
    }

    fn sort_key(&self) -> (u32, u8, u8, String) {
        (
            self.distance.unwrap_or(0),
            u8::MAX.saturating_sub(self.context_score),
            100u8.saturating_sub(self.confidence),
            self.element.semantic_element_id.clone(),
        )
    }
}
