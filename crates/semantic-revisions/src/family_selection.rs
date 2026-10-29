//! Owns selected-family resolution; these header facts never form a publication basis.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Scope-complete link and membership facts for a read, with no payload requirement.
/// Example: native adapters load these facts through indexed memberships and links.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionFamilyBasis {
    pub project_root: String,
    pub commit_version: i64,
    pub selection: Option<SemanticRevisionSelection>,
    pub selected_records: Vec<SemanticRecordRef>,
    pub links: Vec<SemanticRevisionLink>,
}

impl SemanticRevisionFamilyBasis {
    /// Returns explicit remaps; absent or withdrawn families have no returned entry.
    /// Example: `basis.resolve(&query)?` preserves an explicitly pinned older revision.
    pub fn resolve(
        &self,
        query: &SemanticRevisionFamilyQuery,
    ) -> SemanticRevisionResult<Vec<SemanticRevisionRemap>> {
        self.validate(query)?;
        let Some(selection) = &self.selection else {
            return Ok(vec![]);
        };
        let successors =
            crate::revision_index::link_successors(&self.links, &selection.source_context);
        let selected = self
            .selected_records
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut remaps = vec![];
        for requested in query.records.iter().collect::<BTreeSet<_>>() {
            if let Some(accepted) =
                crate::revision_index::selected_successor(&successors, requested, &selected)?
            {
                remaps.push(SemanticRevisionRemap {
                    incoming: requested.clone(),
                    accepted,
                });
            }
        }
        Ok(remaps)
    }

    fn validate(&self, query: &SemanticRevisionFamilyQuery) -> SemanticRevisionResult<()> {
        crate::revision_index::ensure_root(&query.project_root, &self.project_root)?;
        if query
            .expected_commit_version
            .is_some_and(|expected| expected != self.commit_version)
        {
            return Err(SemanticRevisionError::invalid_value(
                self.commit_version.to_string(),
                format!(
                    "the captured read commit {:?}; reload the read view without changing a write base",
                    query.expected_commit_version
                ),
            ));
        }
        if query.selection_id.trim().is_empty()
            || self.selection.as_ref().is_some_and(|selection| {
                selection.selection_id != query.selection_id
                    || selection.project_root != query.project_root
            })
        {
            return Err(SemanticRevisionError::invalid_value(
                &query.selection_id,
                "the requested selection in this project",
            ));
        }
        for record in &query.records {
            if record.record_id.trim().is_empty() {
                return Err(SemanticRevisionError::invalid_value(
                    &record.record_id,
                    "a nonempty exact family reference",
                ));
            }
        }
        for link in &self.links {
            crate::revision_index::ensure_root(&link.project_root, &self.project_root)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "family_selection/tests.rs"]
mod tests;
