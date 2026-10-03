//! Owns read-only Knowledge transfer candidates between semantically similar projects.

use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
};

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{KnowledgeArtifact, semantic_context::SemanticElement, storage};

const AUTOMATIC_SIMHASH_DISTANCE: u32 = 8;
const EXTENDED_SIMHASH_DISTANCE: u32 = 12;
const MIN_EXTENDED_RANK_MARGIN: u32 = 16;

#[derive(Debug)]
pub(crate) struct KnowledgeTransferMatch {
    pub(crate) source: KnowledgeArtifact,
    pub(crate) target: SemanticElement,
    pub(crate) copy: KnowledgeArtifact,
    pub(crate) exact_match: bool,
    pub(crate) simhash_distance: u32,
}

/// Prepares copies for explicit review without reading or writing destination artifacts.
/// Example: `project_transfer_candidates(root, &indexed_elements, context)`.
pub(crate) fn project_transfer_candidates(
    target_root: &str,
    targets: &[SemanticElement],
    context: &mut PluginContext<'_>,
) -> Result<Vec<KnowledgeTransferMatch>, PluginError> {
    let targets = targets
        .iter()
        .filter(|target| target.project_root == target_root && target.lifecycle == "active")
        .cloned()
        .collect::<Vec<_>>();
    let elements = transfer_source_elements(target_root, &targets, context)?;
    let candidate_ids = elements
        .iter()
        .map(|element| element.semantic_element_id.clone())
        .collect::<HashSet<_>>();
    let artifacts = storage::artifacts_for_elements(context, &candidate_ids)?;
    let sources = owned_transfer_sources(elements, &artifacts);
    let source_index = SourceElementIndex::new(&sources);
    Ok(collect_transfer_matches(
        &artifacts,
        &targets,
        &source_index,
    ))
}

fn transfer_source_elements(
    target_root: &str,
    targets: &[SemanticElement],
    context: &mut PluginContext<'_>,
) -> Result<Vec<SemanticElement>, PluginError> {
    let keys = CandidateKeys::from_targets(targets);
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    Ok(storage::candidate_source_elements(
        context,
        &keys.content_fingerprints,
        &keys.kind_name_keys,
        &keys.kind_file_name_keys,
    )?
    .into_iter()
    .filter(|element| {
        element.project_root != target_root
            && !nested_project_roots(&element.project_root, target_root)
    })
    .collect())
}

fn owned_transfer_sources(
    elements: Vec<SemanticElement>,
    artifacts: &[KnowledgeArtifact],
) -> Vec<SourceElement> {
    let owner_ids = artifacts
        .iter()
        .map(|artifact| artifact.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    elements
        .into_iter()
        .filter(|element| owner_ids.contains(element.semantic_element_id.as_str()))
        .map(|element| {
            let project_root = element.project_root.clone();
            SourceElement::new(&project_root, element)
        })
        .collect()
}

/// Exact identity keys derived from one transfer review's target
/// elements, mirroring `SourceElementIndex::candidates_for`'s own admission
/// rule byte-for-byte: a target missing a parseable content fingerprint
/// contributes no keys at all - not even its kind/name or kind/file-name keys
/// - exactly like that rule's existing per-target short-circuit.
#[derive(Default)]
struct CandidateKeys {
    content_fingerprints: HashSet<String>,
    kind_name_keys: HashSet<String>,
    kind_file_name_keys: HashSet<String>,
}

impl CandidateKeys {
    fn from_targets(targets: &[SemanticElement]) -> Self {
        let mut keys = Self::default();
        for target in targets {
            let has_fingerprint = target
                .content_fingerprint
                .as_deref()
                .and_then(Fingerprint::parse)
                .is_some();
            if !has_fingerprint {
                continue;
            }
            keys.content_fingerprints
                .insert(target.content_fingerprint.clone().unwrap_or_default());
            let (kind, name) = kind_name_key(target);
            keys.kind_name_keys.insert(format!("{kind}\u{1}{name}"));
            let (kind, file_name) = kind_file_name_key(target);
            keys.kind_file_name_keys
                .insert(format!("{kind}\u{1}{file_name}"));
        }
        keys
    }

    fn is_empty(&self) -> bool {
        self.content_fingerprints.is_empty()
            && self.kind_name_keys.is_empty()
            && self.kind_file_name_keys.is_empty()
    }
}

fn collect_transfer_matches(
    artifacts: &[KnowledgeArtifact],
    targets: &[SemanticElement],
    sources: &SourceElementIndex<'_>,
) -> Vec<KnowledgeTransferMatch> {
    let mut matches = std::collections::BTreeMap::new();
    for target in targets {
        let candidates = sources.candidates_for(target);
        let Some(selected) = best_source_match(target, &candidates) else {
            continue;
        };
        for candidate in matched_transfer_artifacts(artifacts, target, selected) {
            matches
                .entry(candidate.copy.artifact_id.clone())
                .or_insert(candidate);
        }
    }
    matches.into_values().collect()
}

fn matched_transfer_artifacts(
    artifacts: &[KnowledgeArtifact],
    target: &SemanticElement,
    selected: RankedSource<'_>,
) -> Vec<KnowledgeTransferMatch> {
    artifacts
        .iter()
        .filter(|artifact| selected.owns(artifact))
        .map(|artifact| KnowledgeTransferMatch {
            source: artifact.clone(),
            target: target.clone(),
            copy: inherited_artifact(artifact, target, &selected),
            exact_match: selected.evidence.exact_hash,
            simhash_distance: selected.evidence.simhash_distance,
        })
        .collect()
}

fn best_source_match<'a>(
    target: &SemanticElement,
    sources: &[&'a SourceElement],
) -> Option<RankedSource<'a>> {
    let mut candidates = sources
        .iter()
        .filter_map(|source| RankedSource::new(target, source))
        .collect::<Vec<_>>();
    candidates.sort_by(RankedSource::compare);
    let selected = candidates.first().copied()?;
    selected
        .has_required_margin(candidates.get(1).copied())
        .then_some(selected)
}

#[derive(Debug)]
struct SourceElement {
    project_root: String,
    element: SemanticElement,
}

struct SourceElementIndex<'a> {
    elements: &'a [SourceElement],
    exact_hash: HashMap<String, Vec<usize>>,
    kind_name: HashMap<(String, String), Vec<usize>>,
    kind_file_name: HashMap<(String, String), Vec<usize>>,
}

impl<'a> SourceElementIndex<'a> {
    fn new(elements: &'a [SourceElement]) -> Self {
        let mut index = Self {
            elements,
            exact_hash: HashMap::new(),
            kind_name: HashMap::new(),
            kind_file_name: HashMap::new(),
        };
        for element_index in 0..elements.len() {
            index.insert(element_index);
        }
        index
    }

    fn candidates_for(&self, target: &SemanticElement) -> Vec<&'a SourceElement> {
        let Some(fingerprint) = target
            .content_fingerprint
            .as_deref()
            .and_then(Fingerprint::parse)
        else {
            return Vec::new();
        };
        let mut indices = BTreeSet::new();
        extend_indices(&mut indices, self.exact_hash.get(fingerprint.exact_hash));
        extend_indices(&mut indices, self.kind_name.get(&kind_name_key(target)));
        extend_indices(
            &mut indices,
            self.kind_file_name.get(&kind_file_name_key(target)),
        );
        indices
            .into_iter()
            .map(|index| &self.elements[index])
            .collect()
    }

    fn insert(&mut self, element_index: usize) {
        let element = &self.elements[element_index].element;
        if let Some(fingerprint) = element
            .content_fingerprint
            .as_deref()
            .and_then(Fingerprint::parse)
        {
            self.exact_hash
                .entry(fingerprint.exact_hash.to_owned())
                .or_default()
                .push(element_index);
        }
        self.kind_name
            .entry(kind_name_key(element))
            .or_default()
            .push(element_index);
        self.kind_file_name
            .entry(kind_file_name_key(element))
            .or_default()
            .push(element_index);
    }
}

fn extend_indices(indices: &mut BTreeSet<usize>, candidates: Option<&Vec<usize>>) {
    if let Some(candidates) = candidates {
        indices.extend(candidates);
    }
}

fn kind_name_key(element: &SemanticElement) -> (String, String) {
    (
        element.element_kind.clone(),
        element.name.to_ascii_lowercase(),
    )
}

fn kind_file_name_key(element: &SemanticElement) -> (String, String) {
    (
        element.element_kind.clone(),
        element
            .path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned(),
    )
}

impl SourceElement {
    fn new(project_root: &str, element: SemanticElement) -> Self {
        Self {
            project_root: project_root.to_owned(),
            element,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RankedSource<'a> {
    source: &'a SourceElement,
    evidence: MatchEvidence,
}

impl<'a> RankedSource<'a> {
    fn new(target: &SemanticElement, source: &'a SourceElement) -> Option<Self> {
        let evidence = MatchEvidence::between(target, &source.element)?;
        Some(Self { source, evidence })
    }

    fn compare(left: &Self, right: &Self) -> Ordering {
        right
            .evidence
            .exact_hash
            .cmp(&left.evidence.exact_hash)
            .then(
                left.evidence
                    .simhash_distance
                    .cmp(&right.evidence.simhash_distance),
            )
            .then(
                right
                    .evidence
                    .context_score
                    .cmp(&left.evidence.context_score),
            )
            .then(left.source.project_root.cmp(&right.source.project_root))
            .then(
                left.source
                    .element
                    .semantic_element_id
                    .cmp(&right.source.element.semantic_element_id),
            )
    }

    fn has_required_margin(&self, runner_up: Option<Self>) -> bool {
        if self.evidence.tier != MatchTier::Extended {
            return true;
        }
        runner_up.is_none_or(|candidate| {
            self.rank_points().saturating_sub(candidate.rank_points()) >= MIN_EXTENDED_RANK_MARGIN
        })
    }

    fn rank_points(&self) -> u32 {
        (64 - self.evidence.simhash_distance) * 128 + self.evidence.context_score
    }

    fn owns(&self, artifact: &KnowledgeArtifact) -> bool {
        artifact.project_root.as_deref() == Some(&self.source.project_root)
            && artifact.semantic_element_id == self.source.element.semantic_element_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MatchEvidence {
    exact_hash: bool,
    simhash_distance: u32,
    context_score: u32,
    tier: MatchTier,
}

impl MatchEvidence {
    fn between(target: &SemanticElement, source: &SemanticElement) -> Option<Self> {
        let target_fingerprint = Fingerprint::parse(target.content_fingerprint.as_deref()?)?;
        let source_fingerprint = Fingerprint::parse(source.content_fingerprint.as_deref()?)?;
        let simhash_distance =
            (target_fingerprint.simhash ^ source_fingerprint.simhash).count_ones();
        let exact_hash = target_fingerprint.exact_hash == source_fingerprint.exact_hash;
        let context = ContextEvidence::between(target, source);
        let tier = match_tier(exact_hash, simhash_distance, context)?;
        Some(Self {
            exact_hash,
            simhash_distance,
            context_score: context.score(),
            tier,
        })
    }
}

fn match_tier(
    exact_hash: bool,
    simhash_distance: u32,
    context: ContextEvidence,
) -> Option<MatchTier> {
    match simhash_distance {
        _ if exact_hash => Some(MatchTier::ExactContent),
        0..=AUTOMATIC_SIMHASH_DISTANCE if context.supports_automatic() => {
            Some(MatchTier::Automatic)
        }
        0..=EXTENDED_SIMHASH_DISTANCE if context.supports_extended() => Some(MatchTier::Extended),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchTier {
    ExactContent,
    Automatic,
    Extended,
}

impl MatchTier {
    fn as_str(self) -> &'static str {
        match self {
            Self::ExactContent => "exact_content",
            Self::Automatic => "automatic_0_to_8",
            Self::Extended => "extended_9_to_12",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ContextEvidence {
    kind_match: bool,
    name_match: bool,
    path_score: u32,
}

impl ContextEvidence {
    fn between(target: &SemanticElement, source: &SemanticElement) -> Self {
        Self {
            kind_match: target.element_kind == source.element_kind,
            name_match: target.name.eq_ignore_ascii_case(&source.name),
            path_score: path_score(&target.path, &source.path),
        }
    }

    fn supports_automatic(self) -> bool {
        self.kind_match && (self.name_match || self.path_score > 0)
    }

    fn supports_extended(self) -> bool {
        self.kind_match && self.name_match && self.path_score > 0
    }

    fn score(self) -> u32 {
        u32::from(self.kind_match) * 40 + u32::from(self.name_match) * 32 + self.path_score
    }
}

struct Fingerprint<'a> {
    simhash: u64,
    exact_hash: &'a str,
}

impl<'a> Fingerprint<'a> {
    fn parse(value: &'a str) -> Option<Self> {
        let (simhash, exact_hash) = value.strip_prefix("fp1:")?.split_once(':')?;
        (simhash.len() == 16 && !exact_hash.is_empty()).then_some(Self {
            simhash: u64::from_str_radix(simhash, 16).ok()?,
            exact_hash,
        })
    }
}

fn path_score(target: &str, source: &str) -> u32 {
    if target == source {
        return 28;
    }
    target
        .split('/')
        .rev()
        .zip(source.split('/').rev())
        .take_while(|(left, right)| left == right)
        .count()
        .min(4) as u32
        * 6
}

fn nested_project_roots(left: &str, right: &str) -> bool {
    let left = left.trim_end_matches('/');
    let right = right.trim_end_matches('/');
    left.starts_with(&format!("{right}/")) || right.starts_with(&format!("{left}/"))
}

fn inherited_artifact(
    source: &KnowledgeArtifact,
    target: &SemanticElement,
    selected: &RankedSource<'_>,
) -> KnowledgeArtifact {
    let origin_id = source.metadata["inheritance"]["origin_artifact_id"]
        .as_str()
        .unwrap_or(&source.artifact_id);
    let digest = Sha256::digest(format!("{origin_id}\u{1f}{}", target.semantic_element_id));
    let mut copy = source.clone();
    copy.artifact_id = format!("knowledge-inherited-{digest:x}");
    copy.semantic_element_id
        .clone_from(&target.semantic_element_id);
    copy.project_root = Some(target.project_root.clone());
    attach_inheritance_metadata(&mut copy, source, selected, origin_id);
    copy
}

fn attach_inheritance_metadata(
    copy: &mut KnowledgeArtifact,
    source: &KnowledgeArtifact,
    selected: &RankedSource<'_>,
    origin_id: &str,
) {
    let metadata = copy.metadata.as_object().cloned().unwrap_or_else(|| {
        serde_json::Map::from_iter([("source_metadata".into(), copy.metadata.clone())])
    });
    copy.metadata = Value::Object(metadata);
    copy.metadata["inheritance"] = json!({
        "origin_artifact_id": origin_id,
        "source_artifact_id": source.artifact_id,
        "source_project_root": source.project_root,
        "source_semantic_element_id": source.semantic_element_id,
        "match": "ranked_content_fingerprint",
        "exact_hash_match": selected.evidence.exact_hash,
        "simhash_distance": selected.evidence.simhash_distance,
        "context_score": selected.evidence.context_score,
        "match_tier": selected.evidence.tier.as_str(),
        "automatic_max_simhash_distance": AUTOMATIC_SIMHASH_DISTANCE,
        "extended_max_simhash_distance": EXTENDED_SIMHASH_DISTANCE,
        "minimum_extended_rank_margin": MIN_EXTENDED_RANK_MARGIN
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KnowledgeKind;
    use lumvise_plugin_sdk::HostCallTransport;

    #[test]
    fn extended_tier_accepts_twelve_bits_and_rejects_thirteen() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "target");
        let accepted = element("accepted", "run", "function", "src/lib.rs", 0xfff, "other");
        let rejected = element("rejected", "run", "function", "src/lib.rs", 0x1fff, "other");
        assert_eq!(
            MatchEvidence::between(&target, &accepted).map(|evidence| evidence.tier),
            Some(MatchTier::Extended)
        );
        assert!(MatchEvidence::between(&target, &rejected).is_none());
    }

    #[test]
    fn closest_fingerprint_wins_before_contextual_similarity() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "target");
        let close = source(
            "/old-a",
            element("close", "run", "function", "moved.rs", 1, "close"),
        );
        let contextual = source(
            "/old-b",
            element("context", "run", "function", "src/lib.rs", 3, "context"),
        );
        let sources = [contextual, close];
        let selected = select_source(&target, &sources).expect("best source");
        assert_eq!(selected.source.element.semantic_element_id, "close");
    }

    #[test]
    fn context_breaks_equal_fingerprint_distance() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "target");
        let renamed = source(
            "/old-a",
            element("renamed", "run", "function", "other/lib.rs", 1, "a"),
        );
        let aligned = source(
            "/old-b",
            element("aligned", "run", "function", "src/lib.rs", 2, "b"),
        );
        let sources = [renamed, aligned];
        let selected = select_source(&target, &sources).expect("best source");
        assert_eq!(selected.source.element.semantic_element_id, "aligned");
    }

    #[test]
    fn near_fingerprint_requires_kind_and_name_or_path_support() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "target");
        let kind_only = element("old", "other", "function", "other/file.rs", 1, "other");
        assert_eq!(MatchEvidence::between(&target, &kind_only), None);
    }

    #[test]
    fn exact_content_can_survive_rename_move_and_kind_change() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "same");
        let moved = element("old", "other", "class", "other/file.rs", u64::MAX, "same");
        assert_eq!(
            MatchEvidence::between(&target, &moved).map(|evidence| evidence.tier),
            Some(MatchTier::ExactContent)
        );
    }

    #[test]
    fn extended_match_is_rejected_when_best_candidates_are_ambiguous() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "target");
        let first = source(
            "/old-a",
            element("first", "run", "function", "src/lib.rs", 0x1ff, "a"),
        );
        let second = source(
            "/old-b",
            element("second", "run", "function", "src/lib.rs", 0x2ff, "b"),
        );
        assert!(select_source(&target, &[first, second]).is_none());
    }

    #[test]
    fn extended_match_is_accepted_with_clear_contextual_margin() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "target");
        let exact_path = source(
            "/old-a",
            element("exact-path", "run", "function", "src/lib.rs", 0x1ff, "a"),
        );
        let suffix_path = source(
            "/old-b",
            element("suffix-path", "run", "function", "other/lib.rs", 0x2ff, "b"),
        );
        let sources = [suffix_path, exact_path];
        let selected = select_source(&target, &sources).expect("clear extended match");
        assert_eq!(selected.source.element.semantic_element_id, "exact-path");
    }

    #[test]
    fn lone_extended_match_is_accepted_with_strong_context() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "target");
        let candidate = source(
            "/old",
            element("candidate", "run", "function", "src/lib.rs", 0xfff, "other"),
        );
        assert!(select_source(&target, &[candidate]).is_some());
    }

    #[test]
    fn source_index_excludes_unrelated_elements_before_ranking() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "target");
        let mut sources = (0..1_000)
            .map(|index| {
                source(
                    "/old",
                    element(
                        &format!("unrelated-{index}"),
                        &format!("name-{index}"),
                        "class",
                        &format!("src/file-{index}.rs"),
                        index as u64,
                        &format!("hash-{index}"),
                    ),
                )
            })
            .collect::<Vec<_>>();
        sources.push(source(
            "/old",
            element("candidate", "run", "function", "other/lib.rs", 1, "other"),
        ));
        let index = SourceElementIndex::new(&sources);

        assert_eq!(index.candidates_for(&target).len(), 1);
    }

    #[test]
    fn inheritance_preserves_every_category_and_records_evidence() {
        let target = element("new-element", "run", "function", "src/lib.rs", 0, "same");
        let source_element = source(
            "/old",
            element("old-element", "run", "function", "src/lib.rs", 0, "same"),
        );
        let selected = RankedSource::new(&target, &source_element).expect("ranked source");
        for (index, knowledge_type) in knowledge_kinds().into_iter().enumerate() {
            let source = artifact(index, knowledge_type);
            let first = inherited_artifact(&source, &target, &selected);
            let repeated = inherited_artifact(&source, &target, &selected);
            assert_eq!(first.artifact_id, repeated.artifact_id);
            assert_eq!(first.knowledge_type, source.knowledge_type);
            assert_eq!(first.metadata["inheritance"]["simhash_distance"], 0);
            assert_eq!(first.metadata["inheritance"]["context_score"], 100);
            assert_eq!(first.metadata["inheritance"]["match_tier"], "exact_content");
            assert_eq!(
                first.metadata["inheritance"]["automatic_max_simhash_distance"],
                8
            );
        }
    }

    #[test]
    fn malformed_or_missing_fingerprints_do_not_match() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "same");
        let mut source = element("source", "run", "function", "src/lib.rs", 0, "same");
        source.content_fingerprint = Some("fp1:1:same".into());
        assert_eq!(MatchEvidence::between(&target, &source), None);
        source.content_fingerprint = None;
        assert_eq!(MatchEvidence::between(&target, &source), None);
    }

    #[test]
    fn nested_projects_are_not_copy_peers() {
        assert!(nested_project_roots("/repo", "/repo/examples/demo"));
        assert!(nested_project_roots("/repo/examples/demo", "/repo"));
        assert!(!nested_project_roots("/repo-copy", "/repo"));
    }

    struct TransferCandidateFakeHost {
        elements: Vec<SemanticElement>,
        artifacts: Vec<KnowledgeArtifact>,
        requests: Vec<Value>,
    }

    impl HostCallTransport for TransferCandidateFakeHost {
        fn host_call(&mut self, capability_id: &str, input: Value) -> Result<Value, PluginError> {
            self.requests.push(input.clone());
            assert_eq!(capability_id, "storage.semantic");
            match input["operation"].as_str() {
                Some("candidate_source_elements") => Ok(json!({"elements": self.elements})),
                Some("artifacts_for_elements") => Ok(
                    json!({"artifacts": self.artifacts.iter().map(graph_artifact).collect::<Vec<_>>()}),
                ),
                _ => Err(PluginError::new(
                    "unexpected_candidate_operation",
                    format!(
                        "invalid candidate request `{input}`; expected read-only candidate or artifact lookup"
                    ),
                    false,
                )),
            }
        }
    }

    fn graph_artifact(artifact: &KnowledgeArtifact) -> Value {
        json!({
            "artifact_id": artifact.artifact_id, "semantic_element_id": artifact.semantic_element_id,
            "artifact_kind": artifact.knowledge_type, "title": artifact.title, "content": artifact.content,
            "dependencies": artifact.dependencies, "metadata": {"knowledge": {
                "tags": artifact.tags, "metadata": artifact.metadata, "path": artifact.path,
                "project_root": artifact.project_root
            }}
        })
    }

    #[test]
    fn transfer_candidates_read_only_filter_project_scope_and_deduplicate_copies() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "same");
        let mut inactive = target.clone();
        inactive.lifecycle = "inactive".into();
        let mut wrong_project = target.clone();
        wrong_project.project_root = "/elsewhere".into();
        let owned = artifact(0, KnowledgeKind::Annotation);
        let mut host = transfer_candidate_host(vec![owned.clone(), owned.clone()]);
        host.elements.extend([
            source("/new", target.clone()).element,
            source("/new/nested", target.clone()).element,
        ]);
        let matches = project_transfer_candidates(
            "/new",
            &[target.clone(), target, inactive, wrong_project],
            &mut PluginContext::for_test(&mut host),
        )
        .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].source, owned);
        assert_eq!(matches[0].target.semantic_element_id, "target");
        assert_eq!(matches[0].copy.project_root.as_deref(), Some("/new"));
        assert!(matches[0].exact_match);
        assert_eq!(matches[0].simhash_distance, 0);
        assert_eq!(
            host.requests
                .iter()
                .map(|request| request["operation"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["candidate_source_elements", "artifacts_for_elements"]
        );
        assert_eq!(
            host.requests[1]["semantic_element_ids"],
            json!(["old-element"])
        );
    }

    #[test]
    fn transfer_candidates_ignore_inactive_wrong_project_and_missing_fingerprints() {
        let mut inactive = element("inactive", "run", "function", "src/lib.rs", 0, "same");
        inactive.lifecycle = "inactive".into();
        let wrong_project = source(
            "/elsewhere",
            element("elsewhere", "run", "function", "src/lib.rs", 0, "same"),
        )
        .element;
        let mut malformed = element("malformed", "run", "function", "src/lib.rs", 0, "same");
        malformed.content_fingerprint = Some("not-a-fingerprint".into());
        let mut host = transfer_candidate_host(Vec::new());
        let matches = project_transfer_candidates(
            "/new",
            &[inactive, wrong_project, malformed],
            &mut PluginContext::for_test(&mut host),
        )
        .unwrap();
        assert!(matches.is_empty());
        assert!(host.requests.is_empty());
    }

    #[test]
    fn transfer_candidates_rank_only_sources_that_own_artifacts() {
        let target = element("target", "run", "function", "src/lib.rs", 0, "same");
        let mut host = transfer_candidate_host(vec![
            artifact(0, KnowledgeKind::Decision),
            artifact(1, KnowledgeKind::Annotation),
        ]);
        host.elements[0].content_fingerprint = Some("fp1:0000000000000001:changed".into());
        host.elements.push(
            source(
                "/closer",
                element("unowned", "run", "function", "src/lib.rs", 0, "same"),
            )
            .element,
        );
        let matches =
            project_transfer_candidates("/new", &[target], &mut PluginContext::for_test(&mut host))
                .unwrap();
        assert_eq!(matches.len(), 2);
        for candidate in matches {
            assert_eq!(candidate.source.semantic_element_id, "old-element");
            assert!(!candidate.exact_match);
            assert_eq!(candidate.simhash_distance, 1);
        }
    }

    fn transfer_candidate_host(artifacts: Vec<KnowledgeArtifact>) -> TransferCandidateFakeHost {
        TransferCandidateFakeHost {
            elements: vec![
                source(
                    "/old",
                    element("old-element", "run", "function", "src/lib.rs", 0, "same"),
                )
                .element,
            ],
            artifacts,
            requests: Vec::new(),
        }
    }

    fn element(
        id: &str,
        name: &str,
        kind: &str,
        path: &str,
        simhash: u64,
        hash: &str,
    ) -> SemanticElement {
        SemanticElement {
            project_root: "/new".into(),
            semantic_element_id: id.into(),
            semantic_source_id: "source".into(),
            path: path.into(),
            element_kind: kind.into(),
            name: name.into(),
            parent_element_id: None,
            content_fingerprint: Some(format!("fp1:{simhash:016x}:{hash}")),
            start_line: Some(1),
            end_line: Some(2),
            lifecycle: "active".into(),
            metadata: json!({}),
        }
    }

    fn source(project_root: &str, mut element: SemanticElement) -> SourceElement {
        element.project_root = project_root.into();
        SourceElement::new(project_root, element)
    }

    fn select_source<'a>(
        target: &SemanticElement,
        sources: &'a [SourceElement],
    ) -> Option<RankedSource<'a>> {
        let index = SourceElementIndex::new(sources);
        let candidates = index.candidates_for(target);
        best_source_match(target, &candidates)
    }

    fn artifact(index: usize, knowledge_type: KnowledgeKind) -> KnowledgeArtifact {
        KnowledgeArtifact {
            artifact_id: format!("artifact-{index}"),
            semantic_element_id: "old-element".into(),
            knowledge_type,
            title: "Attached context".into(),
            content: "Keep this".into(),
            tags: vec!["durable".into()],
            dependencies: Vec::new(),
            metadata: json!({"author": "test"}),
            path: None,
            project_root: Some("/old".into()),
        }
    }

    fn knowledge_kinds() -> [KnowledgeKind; 9] {
        [
            KnowledgeKind::Specification,
            KnowledgeKind::Issue,
            KnowledgeKind::TaskAssignment,
            KnowledgeKind::Definition,
            KnowledgeKind::Annotation,
            KnowledgeKind::Report,
            KnowledgeKind::Decision,
            KnowledgeKind::ManualNote,
            KnowledgeKind::DerivedSummary,
        ]
    }
}
