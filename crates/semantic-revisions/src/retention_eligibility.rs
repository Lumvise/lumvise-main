//! Age or ancestry-count eligibility. The retention owner applies protected dependency closure afterward.
use crate::revision_index::ensure_root;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn eligible_history_records(
    state: &SemanticRevisionState,
    policy: &SemanticRevisionRetentionPolicy,
    now_ms: i64,
) -> SemanticRevisionResult<BTreeSet<SemanticRecordRef>> {
    match policy {
        SemanticRevisionRetentionPolicy::Age {
            history_age_seconds,
        } => {
            let cutoff = history_age_cutoff(*history_age_seconds, now_ms)?;
            Ok(historical_bodies(state)
                .filter(|stored| {
                    stored
                        .unselected_at_ms
                        .is_some_and(|retired| retired <= cutoff)
                })
                .map(|stored| stored.record.clone())
                .collect())
        }
        SemanticRevisionRetentionPolicy::RevisionCount { revisions } => {
            if *revisions == 0 {
                return Err(SemanticRevisionError::invalid_value(
                    "0",
                    "a positive revision count",
                ));
            }
            CountedRevisionAncestry::new(state)?.eligible(state, *revisions)
        }
    }
}

fn historical_bodies(
    state: &SemanticRevisionState,
) -> impl Iterator<Item = &SemanticStoredRevision> {
    state.records.iter().filter(|stored| {
        stored.unselected_at_ms.is_some() && !matches!(stored.body, SemanticRevisionBody::Pruned)
    })
}

fn history_age_cutoff(seconds: u64, now_ms: i64) -> SemanticRevisionResult<i64> {
    let age_ms = i64::try_from(seconds)
        .ok()
        .filter(|age| *age > 0)
        .and_then(|age| age.checked_mul(1000))
        .ok_or_else(|| {
            SemanticRevisionError::invalid_value(
                seconds.to_string(),
                "a positive history age representable in signed milliseconds",
            )
        })?;
    now_ms.checked_sub(age_ms).ok_or_else(|| {
        SemanticRevisionError::invalid_value(
            now_ms.to_string(),
            "time representable after subtracting the history age",
        )
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ScopedRevisionAncestry {
    source_context: String,
    record: SemanticRecordRef,
}

#[derive(Default)]
struct CountedAncestryNode {
    predecessors: BTreeSet<ScopedRevisionAncestry>,
    successors: BTreeSet<ScopedRevisionAncestry>,
    pending_successors: usize,
    newer_revisions: u32,
}

#[derive(Default)]
struct CountedRevisionAncestry {
    nodes: BTreeMap<ScopedRevisionAncestry, CountedAncestryNode>,
}

impl CountedRevisionAncestry {
    fn new(state: &SemanticRevisionState) -> SemanticRevisionResult<Self> {
        // Payload collection must not reset the number of accepted revisions in a lineage.
        let known = state
            .records
            .iter()
            .map(|stored| &stored.record)
            .collect::<BTreeSet<_>>();
        let mut ancestry = Self::default();
        for link in &state.revision_links {
            ensure_root(&link.project_root, &state.project_root)?;
            if link.source_context.trim().is_empty()
                || link.older.kind != link.newer.kind
                || !known.contains(&link.older)
                || !known.contains(&link.newer)
            {
                return Err(SemanticRevisionError::invalid_value(
                    format!(
                        "{:?} -> {:?} in {:?}",
                        link.older, link.newer, link.source_context
                    ),
                    "scoped ancestry between known headers of the same record kind",
                ));
            }
            ancestry.insert(link);
        }
        Ok(ancestry)
    }

    fn insert(&mut self, link: &SemanticRevisionLink) {
        let older = ScopedRevisionAncestry {
            source_context: link.source_context.clone(),
            record: link.older.clone(),
        };
        let newer = ScopedRevisionAncestry {
            source_context: link.source_context.clone(),
            record: link.newer.clone(),
        };
        self.nodes
            .entry(older.clone())
            .or_default()
            .successors
            .insert(newer.clone());
        self.nodes
            .entry(newer)
            .or_default()
            .predecessors
            .insert(older);
    }

    fn eligible(
        mut self,
        state: &SemanticRevisionState,
        count: u32,
    ) -> SemanticRevisionResult<BTreeSet<SemanticRecordRef>> {
        self.measure(count)?;
        let linked = self
            .nodes
            .keys()
            .map(|key| &key.record)
            .collect::<BTreeSet<_>>();
        let protected = self
            .nodes
            .iter()
            .filter(|(_, node)| node.newer_revisions < count)
            .map(|(key, _)| &key.record)
            .collect::<BTreeSet<_>>();
        Ok(historical_bodies(state)
            .filter(|stored| linked.contains(&stored.record) && !protected.contains(&stored.record))
            .map(|stored| stored.record.clone())
            .collect())
    }

    fn measure(&mut self, count: u32) -> SemanticRevisionResult<()> {
        let mut ready = self.leaves(count);
        let mut visited = 0;
        while let Some(key) = ready.pop() {
            visited += 1;
            let node = &self.nodes[&key];
            let distance = node.newer_revisions.saturating_add(1).min(count);
            let predecessors = node.predecessors.iter().cloned().collect::<Vec<_>>();
            for predecessor in predecessors {
                self.advance(predecessor, distance, &mut ready);
            }
        }
        if visited != self.nodes.len() {
            let cycle = self
                .nodes
                .iter()
                .find(|(_, node)| node.pending_successors > 0)
                .unwrap();
            return Err(SemanticRevisionError::invalid_value(
                format!("{} in {}", cycle.0.record.record_id, cycle.0.source_context),
                "acyclic scoped ancestry for revision-count retention",
            ));
        }
        Ok(())
    }

    fn leaves(&mut self, count: u32) -> Vec<ScopedRevisionAncestry> {
        let mut leaves = vec![];
        for (key, node) in &mut self.nodes {
            node.pending_successors = node.successors.len();
            node.newer_revisions = if node.pending_successors == 0 {
                0
            } else {
                count
            };
            if node.pending_successors == 0 {
                leaves.push(key.clone());
            }
        }
        leaves
    }

    fn advance(
        &mut self,
        key: ScopedRevisionAncestry,
        distance: u32,
        ready: &mut Vec<ScopedRevisionAncestry>,
    ) {
        let node = self.nodes.get_mut(&key).unwrap();
        node.newer_revisions = node.newer_revisions.min(distance);
        node.pending_successors -= 1;
        if node.pending_successors == 0 {
            ready.push(key);
        }
    }
}
