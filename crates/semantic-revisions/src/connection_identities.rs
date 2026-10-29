//! Exact connection content preserves known IDs and duplicate occurrences across project-routing aliases.
use crate::canonical_json::CanonicalMetadata;
use crate::revision_index::digest;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

struct ConnectionIdentityIndex<'a> {
    existing: BTreeMap<String, Vec<&'a str>>,
    reserved: BTreeSet<&'a str>,
    occurrences: BTreeMap<String, usize>,
    next_ordinal: BTreeMap<String, usize>,
}

impl<'a> ConnectionIdentityIndex<'a> {
    fn new(
        known: &'a [SemanticRevisionConnection],
        selected: &[String],
    ) -> SemanticRevisionResult<Self> {
        let selected = selected.iter().collect::<BTreeSet<_>>();
        let mut ordered = known.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|row| (!selected.contains(&row.connection_id), &row.connection_id));
        let mut existing = BTreeMap::<String, Vec<&str>>::new();
        for row in ordered {
            existing
                .entry(connection_content_key(&row.relationship)?)
                .or_default()
                .push(&row.connection_id);
        }
        Ok(Self {
            existing,
            reserved: known.iter().map(|row| row.connection_id.as_str()).collect(),
            occurrences: BTreeMap::new(),
            next_ordinal: BTreeMap::new(),
        })
    }

    fn next_identity(
        &mut self,
        relationship: &SemanticRelationship,
    ) -> SemanticRevisionResult<String> {
        let key = connection_content_key(relationship)?;
        let occurrence = self.occurrences.entry(key.clone()).or_default();
        let existing = self
            .existing
            .get(&key)
            .and_then(|rows| rows.get(*occurrence));
        *occurrence += 1;
        if let Some(id) = existing {
            return Ok((*id).into());
        }
        let ordinal = self.next_ordinal.entry(key.clone()).or_default();
        loop {
            let id = format!("connection:{key}:{}", *ordinal);
            *ordinal += 1;
            if !self.reserved.contains(id.as_str()) {
                return Ok(id);
            }
        }
    }
}

pub(crate) fn reconcile_connections(
    known: &[SemanticRevisionConnection],
    selected: &[String],
    relationships: &[SemanticRelationship],
) -> SemanticRevisionResult<Vec<SemanticRevisionConnection>> {
    let mut identities = ConnectionIdentityIndex::new(known, selected)?;
    relationships
        .iter()
        .map(|relationship| {
            Ok(SemanticRevisionConnection {
                connection_id: identities.next_identity(relationship)?,
                relationship: relationship.clone(),
            })
        })
        .collect()
}

pub(crate) fn connection_records(
    relationships: &[SemanticRelationship],
) -> SemanticRevisionResult<Vec<SemanticRevisionConnection>> {
    reconcile_connections(&[], &[], relationships)
}

fn connection_content_key(relationship: &SemanticRelationship) -> SemanticRevisionResult<String> {
    // Routing paths are not connection content; metadata order must agree in native and WebAssembly builds.
    digest(&(
        &relationship.source_element_id,
        &relationship.target_element_id,
        &relationship.relationship_kind,
        &relationship.label,
        CanonicalMetadata(&relationship.metadata),
    ))
}
