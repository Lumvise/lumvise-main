//! Read-time staleness for Knowledge artifacts: compares each note's recorded anchor fingerprint with its
//! current anchor. Read handlers call `mark`; writes call `baseline`. The signal is never stored.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use lumvise_contracts::{KnowledgeOutdatedReasonV2, KnowledgeOutdatedV2, SemanticRecordRef};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use lumvise_semantic_revisions::{ContentFingerprintParts, SemanticRevisionBody};
use serde_json::{Value, json};

use crate::{KnowledgeArtifact, semantic_context::SemanticElement, storage};

/// Calibrated on core history: above 8 bits flags 85% of full rewrites and 1% of small edits (12 flagged 74% and 0%).
const OUTDATED_SIMHASH_DISTANCE: u32 = 8;

/// Sets `outdated` on each artifact whose current anchor was rewritten or removed since its baseline.
/// Artifacts without a project root are left unmarked, and so are those whose anchor lookup fails:
/// the marker is a hint that must never fail the read returning the note.
pub(crate) fn mark(context: &mut PluginContext<'_>, artifacts: &mut [KnowledgeArtifact]) {
    let mut by_project: BTreeMap<String, Vec<&mut KnowledgeArtifact>> = BTreeMap::new();
    for artifact in artifacts.iter_mut() {
        if let Some(root) = artifact.project_root.clone() {
            by_project.entry(root).or_default().push(artifact);
        }
    }
    for (root, project) in by_project {
        let owners = project
            .iter()
            .map(|artifact| artifact.semantic_element_id.clone())
            .collect();
        let Ok(mut anchors) = Anchors::open(context, &root, owners) else {
            continue;
        };
        for artifact in project {
            if let Ok(anchor) = anchors.current(context, &artifact.semantic_element_id) {
                artifact.outdated =
                    classify(artifact.anchor_fingerprint.as_deref(), anchor.as_ref());
            }
        }
    }
}

/// The fingerprint a write records as the anchor's baseline: the current anchor's when it resolves,
/// otherwise the owner element's own fingerprint.
pub(crate) fn baseline(
    context: &mut PluginContext<'_>,
    root: &str,
    owner: &str,
    element: &Value,
) -> Result<Option<String>, PluginError> {
    let mut anchors = Anchors::open(context, root, HashSet::from([owner.to_owned()]))?;
    let current = anchors
        .current(context, owner)?
        .filter(|anchor| anchor.lifecycle != "inactive")
        .and_then(|anchor| anchor.content_fingerprint);
    Ok(current.or_else(|| element["content_fingerprint"].as_str().map(str::to_owned)))
}

/// A missing or inactive anchor is `removed`. Otherwise a fingerprint further than the
/// threshold from the baseline is `rewritten`. Absent or unparsable evidence stays unmarked.
fn classify(
    baseline: Option<&str>,
    anchor: Option<&SemanticElement>,
) -> Option<KnowledgeOutdatedV2> {
    let Some(anchor) = anchor.filter(|anchor| anchor.lifecycle != "inactive") else {
        return Some(KnowledgeOutdatedV2 {
            reason: KnowledgeOutdatedReasonV2::Removed,
            distance: None,
        });
    };
    let recorded = ContentFingerprintParts::parse(baseline?)?;
    let current = ContentFingerprintParts::parse(anchor.content_fingerprint.as_deref()?)?;
    if recorded.matches_exact(&current) {
        return None;
    }
    let distance = recorded.hamming_distance(&current)?;
    (distance > OUTDATED_SIMHASH_DISTANCE).then_some(KnowledgeOutdatedV2 {
        reason: KnowledgeOutdatedReasonV2::Rewritten,
        distance: Some(distance),
    })
}

/// Resolves owner IDs to their current anchor for one project, caching each lookup for one call.
/// Unmanaged projects use exact `elements_by_ids`; revision-managed projects follow successors.
struct Anchors {
    root: String,
    owners: HashSet<String>,
    /// Working selection's source context; `None` when the project is not revision-managed.
    source_context: Option<String>,
    /// Exact live elements for `owners`, fetched once when a lookup first needs them.
    exact: Option<HashMap<String, SemanticElement>>,
    /// Whether each path's element records are selected in the working selection.
    selected_by_path: HashMap<String, BTreeSet<SemanticRecordRef>>,
    current: HashMap<String, Option<SemanticElement>>,
}

impl Anchors {
    fn open(
        context: &mut PluginContext<'_>,
        root: &str,
        owners: HashSet<String>,
    ) -> Result<Self, PluginError> {
        let membership =
            storage::selection_membership(context, root, json!({"kind": "none"}), vec![])?;
        Ok(Self {
            root: root.to_owned(),
            owners,
            source_context: membership.map(|observation| observation.selection.source_context),
            exact: None,
            selected_by_path: HashMap::new(),
            current: HashMap::new(),
        })
    }

    /// `None` means the anchor is missing, pruned, or no longer selected.
    fn current(
        &mut self,
        context: &mut PluginContext<'_>,
        owner: &str,
    ) -> Result<Option<SemanticElement>, PluginError> {
        if let Some(found) = self.current.get(owner) {
            return Ok(found.clone());
        }
        let found = self.resolve(context, owner)?;
        self.current.insert(owner.to_owned(), found.clone());
        Ok(found)
    }

    fn resolve(
        &mut self,
        context: &mut PluginContext<'_>,
        owner: &str,
    ) -> Result<Option<SemanticElement>, PluginError> {
        let Some(source_context) = self.source_context.clone() else {
            return self.exact(context, owner);
        };
        let Some(mut record) =
            storage::read_revision(context, &self.root, SemanticRecordRef::element(owner))?
        else {
            return self.exact(context, owner);
        };
        let mut visited = BTreeSet::new();
        loop {
            visited.insert(record.record.clone());
            let newer = record
                .successors
                .iter()
                .filter(|link| link.source_context == source_context)
                .max_by_key(|link| link.commit_version)
                .map(|link| link.newer.clone())
                .filter(|newer| !visited.contains(newer));
            let Some(newer) = newer else { break };
            let Some(next) = storage::read_revision(context, &self.root, newer)? else {
                return Ok(None);
            };
            record = next;
        }
        let SemanticRevisionBody::Element(element) = record.body else {
            return Ok(None);
        };
        if !self.is_selected(context, &element.path, &record.record)? {
            return Ok(None);
        }
        crate::parse(json!(element), "a current semantic element").map(Some)
    }

    fn is_selected(
        &mut self,
        context: &mut PluginContext<'_>,
        path: &str,
        record: &SemanticRecordRef,
    ) -> Result<bool, PluginError> {
        if !self.selected_by_path.contains_key(path) {
            let coverage = json!({"kind": "partition",
                "partition": {"project_root": self.root, "replace_paths": [path]}});
            let records = storage::selection_membership(context, &self.root, coverage, vec![])?
                .map(|observation| observation.expected.records.into_iter().collect())
                .unwrap_or_default();
            self.selected_by_path.insert(path.to_owned(), records);
        }
        Ok(self.selected_by_path[path].contains(record))
    }

    fn exact(
        &mut self,
        context: &mut PluginContext<'_>,
        owner: &str,
    ) -> Result<Option<SemanticElement>, PluginError> {
        if self.exact.is_none() {
            let elements = storage::elements_by_ids(context, &self.root, &self.owners)?;
            self.exact = Some(
                elements
                    .into_iter()
                    .map(|element| (element.semantic_element_id.clone(), element))
                    .collect(),
            );
        }
        Ok(self
            .exact
            .as_ref()
            .and_then(|exact| exact.get(owner))
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn element(fingerprint: Option<&str>, lifecycle: &str) -> SemanticElement {
        serde_json::from_value(json!({
            "project_root": "/repo", "semantic_element_id": "file:src/a.rs",
            "semantic_source_id": "src", "path": "src/a.rs", "element_kind": "function",
            "name": "a", "parent_element_id": null, "content_fingerprint": fingerprint,
            "start_line": 1, "end_line": 2, "lifecycle": lifecycle, "metadata": {}
        }))
        .unwrap()
    }

    #[test]
    fn missing_or_inactive_anchor_is_removed_without_distance() {
        let removed = Some(KnowledgeOutdatedV2 {
            reason: KnowledgeOutdatedReasonV2::Removed,
            distance: None,
        });
        assert_eq!(classify(Some("fp1:0000000000000000:a"), None), removed);
        assert_eq!(classify(None, None), removed);
        let inactive = element(Some("fp1:0000000000000000:a"), "inactive");
        assert_eq!(
            classify(Some("fp1:0000000000000000:a"), Some(&inactive)),
            removed
        );
    }

    #[test]
    fn missing_or_unparsable_evidence_is_not_marked() {
        let active = element(Some("fp1:ffffffffffffffff:c"), "active");
        let unfingerprinted = element(None, "active");
        let unversioned = element(Some("plain-hash"), "active");
        assert_eq!(classify(None, Some(&active)), None);
        assert_eq!(classify(Some("plain-hash"), Some(&active)), None);
        assert_eq!(
            classify(Some("fp1:0000000000000000:a"), Some(&unfingerprinted)),
            None
        );
        assert_eq!(
            classify(Some("fp1:0000000000000000:a"), Some(&unversioned)),
            None
        );
    }

    #[test]
    fn exact_hash_match_is_not_marked_even_when_simhash_differs() {
        let anchor = element(Some("fp1:ffffffffffffffff:a"), "active");
        assert_eq!(
            classify(Some("fp1:0000000000000000:a"), Some(&anchor)),
            None
        );
    }

    #[test]
    fn small_simhash_drift_is_not_marked() {
        let anchor = element(Some("fp1:00000000000000ff:b"), "active");
        assert_eq!(
            classify(Some("fp1:0000000000000000:a"), Some(&anchor)),
            None
        );
    }

    #[test]
    fn drift_at_the_threshold_is_not_marked_and_one_bit_more_is_rewritten() {
        let at_threshold = element(Some("fp1:00000000000000ff:b"), "active");
        assert_eq!(
            classify(Some("fp1:0000000000000000:a"), Some(&at_threshold)),
            None
        );
        let past_threshold = element(Some("fp1:00000000000001ff:b"), "active");
        assert_eq!(
            classify(Some("fp1:0000000000000000:a"), Some(&past_threshold)),
            Some(KnowledgeOutdatedV2 {
                reason: KnowledgeOutdatedReasonV2::Rewritten,
                distance: Some(9),
            })
        );
    }

    #[test]
    fn full_simhash_inversion_is_rewritten_with_distance_64() {
        let anchor = element(Some("fp1:ffffffffffffffff:c"), "active");
        assert_eq!(
            classify(Some("fp1:0000000000000000:a"), Some(&anchor)),
            Some(KnowledgeOutdatedV2 {
                reason: KnowledgeOutdatedReasonV2::Rewritten,
                distance: Some(64),
            })
        );
    }
}
