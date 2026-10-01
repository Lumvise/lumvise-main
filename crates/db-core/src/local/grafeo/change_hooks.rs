use crate::interface::{
    ChangeBatch, ChangeDisposition, ChangeHookRegistration, ChangeHookScope, ChangedElement,
};
use crate::local::grafeo::graph_row_projection::{
    bool_property, i64_property, semantic_element_from_node,
};
#[cfg(test)]
use crate::local::grafeo::graph_rows::DELETED_AT_PROPERTY_FOR_HOOKS;
use crate::local::grafeo::graph_rows::{
    ACTIVE_PROPERTY_FOR_HOOKS, LAST_CHANGED_REVISION_PROPERTY_FOR_HOOKS,
    SEMANTIC_ELEMENT_ID_PROPERTY_FOR_HOOKS,
};
use crate::local::runtime::DbCore;
use crate::{DbError, Result};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

const CHANGE_BUFFER_CAPACITY: usize = 1_024;
mod projected_changes;

#[cfg(test)]
mod buffer_tests;

#[derive(Clone, Debug)]
pub(crate) struct DirtyElement {
    pub(crate) element_id: String,
    pub(crate) project_root: String,
    pub(crate) entity_kind: String,
    pub(crate) revision: i64,
    pub(crate) disposition: ChangeDisposition,
}

#[derive(Debug)]
pub(crate) struct ChangeBuffer {
    capacity: usize,
    /// Revisions before this cannot be reconstructed from the buffer alone.
    complete_after: i64,
    head_revision: i64,
    entries: BTreeMap<String, DirtyElement>,
}

impl ChangeBuffer {
    pub(crate) fn new(head_revision: i64) -> Self {
        Self::with_capacity(head_revision, CHANGE_BUFFER_CAPACITY)
    }

    pub(crate) fn with_capacity(head_revision: i64, capacity: usize) -> Self {
        Self {
            capacity,
            complete_after: head_revision,
            head_revision,
            entries: BTreeMap::new(),
        }
    }

    pub(crate) fn publish(&mut self, commit_version: i64, changes: Vec<DirtyElement>) {
        self.head_revision = self.head_revision.max(commit_version);
        for change in changes {
            self.entries.insert(change.element_id.clone(), change);
        }
        self.retain_newest_changes();
    }

    fn retain_newest_changes(&mut self) {
        let excess = self.entries.len().saturating_sub(self.capacity);
        if excess == 0 {
            return;
        }
        // Select the same (revision, id) eviction boundary once: repeated
        // minimum scans made a project-sized publication quadratic.
        let mut ordered = self
            .entries
            .iter()
            .map(|(id, change)| (change.revision, id.as_str()))
            .collect::<Vec<_>>();
        let (_, last_evicted, _) = ordered.select_nth_unstable(excess - 1);
        let cutoff = (last_evicted.0, last_evicted.1.to_owned());
        self.complete_after = self.complete_after.max(cutoff.0);
        self.entries
            .retain(|id, change| (change.revision, id.as_str()) > (cutoff.0, cutoff.1.as_str()));
    }

    fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            complete_after: self.complete_after,
            head_revision: self.head_revision,
            entries: self.entries.values().cloned().collect(),
        }
    }
}

#[derive(Clone)]
struct BufferSnapshot {
    complete_after: i64,
    head_revision: i64,
    entries: Vec<DirtyElement>,
}

/// Deep module for durable, level-triggered semantic graph reconciliation.
pub struct ChangeHooks<'db> {
    db: &'db DbCore,
}

impl<'db> ChangeHooks<'db> {
    pub(crate) fn new(db: &'db DbCore) -> Self {
        Self { db }
    }

    pub fn register(
        &self,
        hook_name: &str,
        scope: ChangeHookScope,
    ) -> Result<ChangeHookRegistration> {
        validate_name_and_scope(hook_name, &scope)?;
        let _operation = lock(&self.db.change_hook_operations);
        let scope_json = serde_json::to_string(&scope)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.db.conn.write_conn();
        let existing = registration_from_connection(&conn, hook_name)?;
        let registration = match existing {
            Some(existing) if existing.scope == scope => existing,
            Some(existing) => {
                let generation = existing.registration_generation + 1;
                conn.execute(
                    "UPDATE change_hook_registrations SET project_root = ?2, entity_kinds_json = ?3, watermark = 0, registration_generation = ?4, updated_at = ?5 WHERE hook_name = ?1",
                    params![hook_name, scope.project_root, scope_json, generation, now],
                )?;
                ChangeHookRegistration {
                    hook_name: hook_name.to_string(),
                    scope,
                    watermark: 0,
                    registration_generation: generation,
                }
            }
            None => {
                conn.execute(
                    "INSERT INTO change_hook_registrations(hook_name, project_root, entity_kinds_json, watermark, registration_generation, created_at, updated_at) VALUES (?1, ?2, ?3, 0, 1, ?4, ?4)",
                    params![hook_name, scope.project_root, scope_json, now],
                )?;
                ChangeHookRegistration {
                    hook_name: hook_name.to_string(),
                    scope,
                    watermark: 0,
                    registration_generation: 1,
                }
            }
        };
        Ok(registration)
    }

    pub fn deregister(&self, hook_name: &str) -> Result<bool> {
        validate_name(hook_name)?;
        let _operation = lock(&self.db.change_hook_operations);
        let changed = self.db.conn.write_conn().execute(
            "DELETE FROM change_hook_registrations WHERE hook_name = ?1",
            [hook_name],
        )?;
        Ok(changed != 0)
    }

    pub fn registrations(&self) -> Result<Vec<ChangeHookRegistration>> {
        let conn = self.db.conn.read_conn();
        let mut statement = conn.prepare_cached("SELECT hook_name, project_root, entity_kinds_json, watermark, registration_generation FROM change_hook_registrations ORDER BY hook_name")?;
        statement
            .query_map([], registration_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn dirty_batch(&self, hook_name: &str, maximum_elements: usize) -> Result<ChangeBatch> {
        validate_name(hook_name)?;
        validate_maximum(maximum_elements)?;
        let registration = {
            let conn = self.db.conn.read_conn();
            registration_from_connection(&conn, hook_name)?
                .ok_or_else(|| DbError::invalid_value(hook_name, "registered change hook name"))?
        };
        let mut batch = self.derive_changes(
            &registration.scope,
            registration.watermark,
            maximum_elements,
        )?;
        batch.hook_name = Some(registration.hook_name);
        batch.registration_generation = registration.registration_generation;
        Ok(batch)
    }

    pub fn acknowledge(&self, batch: &ChangeBatch) -> Result<bool> {
        let Some(hook_name) = batch.hook_name.as_deref() else {
            return Err(DbError::invalid_value(
                "stateless batch",
                "batch returned by dirty_batch",
            ));
        };
        if batch.base_revision < 0 || batch.target_revision < batch.base_revision {
            return Err(DbError::invalid_value(
                batch.target_revision.to_string(),
                "non-decreasing non-negative watermark",
            ));
        }
        if batch.target_revision > lock(&self.db.change_buffer).head_revision {
            return Err(DbError::invalid_value(
                batch.target_revision.to_string(),
                "target revision no newer than published buffer head",
            ));
        }
        let _operation = lock(&self.db.change_hook_operations);
        let conn = self.db.conn.write_conn();
        let updated = conn.execute(
            "UPDATE change_hook_registrations SET watermark = ?4, updated_at = ?5 WHERE hook_name = ?1 AND registration_generation = ?2 AND watermark = ?3",
            params![hook_name, batch.registration_generation, batch.base_revision, batch.target_revision, Utc::now().to_rfc3339()],
        )?;
        if updated != 0 {
            return Ok(true);
        }
        let current = registration_from_connection(&conn, hook_name)?;
        Ok(current.is_some_and(|registration| {
            registration.registration_generation == batch.registration_generation
                && registration.watermark == batch.target_revision
        }))
    }

    pub fn changes_since(
        &self,
        scope: &ChangeHookScope,
        after_revision: i64,
        maximum_elements: usize,
    ) -> Result<ChangeBatch> {
        validate_scope(scope)?;
        if after_revision < 0 {
            return Err(DbError::invalid_value(
                after_revision.to_string(),
                "non-negative revision",
            ));
        }
        validate_maximum(maximum_elements)?;
        let head_revision = lock(&self.db.change_buffer).head_revision;
        if after_revision > head_revision {
            return Err(DbError::invalid_value(
                after_revision.to_string(),
                "revision no newer than published buffer head",
            ));
        }
        self.derive_changes(scope, after_revision, maximum_elements)
    }

    #[cfg(test)]
    pub fn gc_tombstones(&self) -> Result<usize> {
        let _operation = lock(&self.db.change_hook_operations);
        let floor = {
            let conn = self.db.conn.read_conn();
            conn.query_row(
                "SELECT MIN(watermark) FROM change_hook_registrations",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .unwrap_or_else(|| lock(&self.db.change_buffer).head_revision)
        };
        let candidates = self.db.graph.read(|graph| {
            graph
                .iter_nodes()
                .filter_map(|node| {
                    if !node.has_label("SemanticElement")
                        || bool_property(&node, ACTIVE_PROPERTY_FOR_HOOKS)
                    {
                        return None;
                    }
                    let deleted_at = i64_property(&node, DELETED_AT_PROPERTY_FOR_HOOKS)?;
                    (deleted_at >= 0 && deleted_at <= floor).then_some(node.id)
                })
                .collect::<Vec<_>>()
        });
        if candidates.is_empty() {
            return Ok(0);
        }
        self.db.graph.write(|graph| {
            let mut removed = 0;
            for node_id in candidates {
                let eligible = graph.get_node(node_id).is_some_and(|node| {
                    !bool_property(&node, ACTIVE_PROPERTY_FOR_HOOKS)
                        && i64_property(&node, DELETED_AT_PROPERTY_FOR_HOOKS)
                            .is_some_and(|deleted_at| deleted_at >= 0 && deleted_at <= floor)
                });
                if eligible {
                    graph.delete_node(node_id);
                    removed += 1;
                }
            }
            Ok(removed)
        })
    }

    fn derive_changes(
        &self,
        scope: &ChangeHookScope,
        after_revision: i64,
        maximum_elements: usize,
    ) -> Result<ChangeBatch> {
        let buffer = lock(&self.db.change_buffer).snapshot();
        let mut changed = if after_revision < buffer.complete_after {
            self.scan_changes(scope, after_revision, buffer.head_revision)
        } else {
            self.warm_changes(scope, after_revision, buffer.head_revision, &buffer.entries)
        }?;
        changed.sort_by(|left, right| {
            (left.revision, &left.element_id).cmp(&(right.revision, &right.element_id))
        });
        let target_revision =
            page_revision_groups(&mut changed, maximum_elements, buffer.head_revision);
        Ok(ChangeBatch {
            base_revision: after_revision,
            target_revision,
            changed,
            hook_name: None,
            registration_generation: 0,
        })
    }

    fn warm_changes(
        &self,
        scope: &ChangeHookScope,
        after_revision: i64,
        target: i64,
        entries: &[DirtyElement],
    ) -> Result<Vec<ChangedElement>> {
        let candidates = entries
            .iter()
            .filter(|entry| {
                entry.revision > after_revision
                    && entry.revision <= target
                    && matches_scope(scope, &entry.project_root, &entry.entity_kind)
            })
            .cloned()
            .collect::<Vec<_>>();
        self.db.graph.read(|graph| {
            Ok(candidates
                .into_iter()
                .filter_map(|entry| {
                    let node = graph
                        .find_nodes_by_property(
                            SEMANTIC_ELEMENT_ID_PROPERTY_FOR_HOOKS,
                            &grafeo::Value::from(entry.element_id.as_str()),
                        )
                        .into_iter()
                        .filter_map(|id| graph.get_node(id))
                        .find(|node| node.has_label("SemanticElement"));
                    match node {
                        Some(node)
                            if i64_property(&node, LAST_CHANGED_REVISION_PROPERTY_FOR_HOOKS)
                                .is_some_and(|revision| revision <= target) =>
                        {
                            Some(ChangedElement {
                                element_id: entry.element_id,
                                entity_kind: entry.entity_kind,
                                revision: entry.revision,
                                disposition: entry.disposition,
                            })
                        }
                        None if entry.disposition == ChangeDisposition::Removal => {
                            Some(ChangedElement {
                                element_id: entry.element_id,
                                entity_kind: entry.entity_kind,
                                revision: entry.revision,
                                disposition: entry.disposition,
                            })
                        }
                        _ => None,
                    }
                })
                .collect())
        })
    }

    fn scan_changes(
        &self,
        scope: &ChangeHookScope,
        after_revision: i64,
        target: i64,
    ) -> Result<Vec<ChangedElement>> {
        self.db.graph.read(|graph| {
            Ok(
                projected_changes::project_change_nodes(graph, &scope.project_root)
                    .into_iter()
                    .filter_map(|node| {
                        if !node.has_label("SemanticElement") {
                            return None;
                        }
                        let revision =
                            i64_property(&node, LAST_CHANGED_REVISION_PROPERTY_FOR_HOOKS)?;
                        if revision <= after_revision || revision > target {
                            return None;
                        }
                        let element = semantic_element_from_node(&node)?;
                        if !matches_scope(scope, &element.project_root, &element.element_kind) {
                            return None;
                        }
                        let disposition = if bool_property(&node, ACTIVE_PROPERTY_FOR_HOOKS) {
                            ChangeDisposition::Upserted
                        } else {
                            ChangeDisposition::Removal
                        };
                        Some(ChangedElement {
                            element_id: element.semantic_element_id,
                            entity_kind: element.element_kind,
                            revision,
                            disposition,
                        })
                    })
                    .collect(),
            )
        })
    }
}

fn page_revision_groups(changed: &mut Vec<ChangedElement>, maximum: usize, head: i64) -> i64 {
    if changed.is_empty() {
        return head;
    }
    let total = changed.len();
    let mut count = 0;
    let mut target = changed[0].revision;
    for group in changed.chunk_by(|left, right| left.revision == right.revision) {
        if count > 0 && count + group.len() > maximum {
            break;
        }
        count += group.len();
        target = group[0].revision;
    }
    changed.truncate(count);
    if count == total { head } else { target }
}

fn matches_scope(scope: &ChangeHookScope, project_root: &str, entity_kind: &str) -> bool {
    scope.project_root == project_root
        && (scope.entity_kinds.is_empty() || scope.entity_kinds.contains(entity_kind))
}

fn validate_name_and_scope(name: &str, scope: &ChangeHookScope) -> Result<()> {
    validate_name(name)?;
    validate_scope(scope)
}
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        Err(DbError::invalid_value(name, "non-empty change hook name"))
    } else {
        Ok(())
    }
}
fn validate_scope(scope: &ChangeHookScope) -> Result<()> {
    if scope.project_root.is_empty() {
        Err(DbError::invalid_value(
            &scope.project_root,
            "non-empty project root",
        ))
    } else {
        Ok(())
    }
}
fn validate_maximum(maximum: usize) -> Result<()> {
    if maximum == 0 {
        Err(DbError::invalid_value("0", "non-zero maximum elements"))
    } else {
        Ok(())
    }
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn registration_from_connection(
    conn: &rusqlite::Connection,
    hook_name: &str,
) -> Result<Option<ChangeHookRegistration>> {
    conn.query_row("SELECT hook_name, project_root, entity_kinds_json, watermark, registration_generation FROM change_hook_registrations WHERE hook_name = ?1", [hook_name], registration_from_row).optional().map_err(Into::into)
}
fn registration_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChangeHookRegistration> {
    let scope_json: String = row.get(2)?;
    let scope = serde_json::from_str(&scope_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(ChangeHookRegistration {
        hook_name: row.get(0)?,
        scope,
        watermark: row.get(3)?,
        registration_generation: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SemanticElement;
    use crate::local::runtime::DbCore;
    use std::collections::BTreeSet;

    fn element(id: &str) -> SemanticElement {
        SemanticElement {
            project_root: "/repo".into(),
            semantic_element_id: id.into(),
            semantic_source_id: "source".into(),
            path: format!("src/{id}.rs"),
            element_kind: "file".into(),
            name: id.into(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".into(),
            match_evidence: None,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn overflow_falls_back_to_the_authoritative_graph_scan() {
        let db = DbCore::in_memory().unwrap();
        *lock(&db.change_buffer) = ChangeBuffer::with_capacity(0, 2);
        let hooks = db.change_hooks();
        hooks
            .register(
                "overflow",
                ChangeHookScope {
                    project_root: "/repo".into(),
                    entity_kinds: BTreeSet::new(),
                },
            )
            .unwrap();
        let storage = db.storage_manager().semantic_storage();
        for id in ["a", "b", "c"] {
            storage.upsert_element(&element(id)).unwrap();
        }

        let batch = hooks.dirty_batch("overflow", 10).unwrap();
        assert_eq!(
            batch
                .changed
                .iter()
                .map(|change| change.element_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }
}
