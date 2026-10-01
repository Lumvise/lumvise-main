//! App Core-owned staging for atomically published semantic graph snapshots.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lumvise_db_core::{
    SemanticElement, SemanticOperation, SemanticPartition, SemanticPersistence,
    SemanticRelationship, SemanticResult,
};
use lumvise_plugin_runtime::HostCapabilityError;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

use super::host_capability_catalog::SEMANTIC_STORAGE;

const WRITE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_ACTIVE_WRITES: usize = 2;
const MAX_STAGED_RECORDS: usize = 200_000;
const MAX_STAGED_BYTES: usize = 256 * 1024 * 1024;

pub(super) struct SemanticSnapshotWrites {
    writes: Mutex<HashMap<String, StagedSemanticSnapshot>>,
}

struct StagedSemanticSnapshot {
    plugin_id: String,
    project_root: String,
    partition_paths: Vec<String>,
    page_count: usize,
    next_page_index: usize,
    created_at: Instant,
    serialized_bytes: usize,
    elements: BTreeMap<String, SemanticElement>,
    relationships: BTreeMap<String, SemanticRelationship>,
}

impl SemanticSnapshotWrites {
    pub(super) fn new() -> Self {
        Self {
            writes: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn begin(
        &self,
        plugin_id: &str,
        snapshot_id: &str,
        project_root: &str,
        partition_paths: Vec<String>,
        page_count: usize,
    ) -> Result<Value, HostCapabilityError> {
        validate_identity(snapshot_id, project_root, page_count)?;
        let mut writes = self.lock()?;
        discard_expired(&mut writes);
        if !writes.contains_key(snapshot_id) && writes.len() >= MAX_ACTIVE_WRITES {
            return Err(write_error(
                "semantic_snapshot_write_limit",
                format!(
                    "cannot begin semantic snapshot `{snapshot_id}`; expected at most {MAX_ACTIVE_WRITES} active writes"
                ),
                true,
            ));
        }
        writes.insert(
            snapshot_id.to_owned(),
            StagedSemanticSnapshot {
                plugin_id: plugin_id.to_owned(),
                project_root: project_root.to_owned(),
                partition_paths,
                page_count,
                next_page_index: 0,
                created_at: Instant::now(),
                serialized_bytes: 0,
                elements: BTreeMap::new(),
                relationships: BTreeMap::new(),
            },
        );
        Ok(json!({"snapshot_id": snapshot_id, "status": "staging"}))
    }

    pub(super) fn stage(
        &self,
        plugin_id: &str,
        snapshot_id: &str,
        page_index: usize,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    ) -> Result<Value, HostCapabilityError> {
        let page_bytes = serde_json::to_vec(&(&elements, &relationships))
            .map_err(|error| {
                write_error(
                    "semantic_snapshot_encoding_failed",
                    error.to_string(),
                    false,
                )
            })?
            .len();
        let mut writes = self.lock()?;
        discard_expired(&mut writes);
        let write = owned_write_mut(&mut writes, snapshot_id, plugin_id)?;
        if page_index != write.next_page_index {
            return Err(write_error(
                "semantic_snapshot_page_out_of_order",
                format!(
                    "invalid page index `{page_index}` for semantic snapshot `{snapshot_id}`; expected `{}`",
                    write.next_page_index
                ),
                false,
            ));
        }
        if page_index >= write.page_count {
            return Err(write_error(
                "semantic_snapshot_page_out_of_range",
                format!(
                    "invalid page index `{page_index}` for semantic snapshot `{snapshot_id}`; expected below `{}`",
                    write.page_count
                ),
                false,
            ));
        }
        let record_count =
            write.elements.len() + write.relationships.len() + elements.len() + relationships.len();
        let byte_count = write.serialized_bytes + page_bytes;
        validate_quota(snapshot_id, record_count, byte_count)?;
        for element in elements {
            write
                .elements
                .insert(element.semantic_element_id.clone(), element);
        }
        for relationship in relationships {
            write
                .relationships
                .insert(relationship_key(&relationship), relationship);
        }
        write.serialized_bytes = byte_count;
        write.next_page_index += 1;
        Ok(json!({
            "snapshot_id": snapshot_id,
            "staged_pages": write.next_page_index,
            "staged_elements": write.elements.len(),
            "staged_relationships": write.relationships.len()
        }))
    }

    pub(super) fn commit(
        &self,
        semantic: &dyn SemanticPersistence,
        plugin_id: &str,
        snapshot_id: &str,
    ) -> Result<Value, HostCapabilityError> {
        let write = {
            let mut writes = self.lock()?;
            discard_expired(&mut writes);
            let write = owned_write(&writes, snapshot_id, plugin_id)?;
            if write.next_page_index != write.page_count {
                return Err(write_error(
                    "semantic_snapshot_incomplete",
                    format!(
                        "cannot commit semantic snapshot `{snapshot_id}` with `{}` staged pages; expected `{}`",
                        write.next_page_index, write.page_count
                    ),
                    false,
                ));
            }
            writes
                .remove(snapshot_id)
                .ok_or_else(|| snapshot_not_found(snapshot_id, plugin_id))?
        };
        let elements = write.elements.into_values().collect::<Vec<_>>();
        let relationships = write.relationships.into_values().collect::<Vec<_>>();
        let operation = if write.partition_paths.is_empty() {
            SemanticOperation::SyncStructure {
                project_root: write.project_root,
                elements,
                relationships,
            }
        } else {
            SemanticOperation::SyncPartition {
                partition: SemanticPartition {
                    project_root: write.project_root,
                    replace_paths: write.partition_paths,
                },
                elements,
                relationships,
            }
        };
        let result = semantic
            .execute(operation, &InvocationControl::sixty_seconds())
            .map_err(storage_error)?;
        let report = match result {
            SemanticResult::SyncStructure(report) | SemanticResult::SyncPartition(report) => report,
            _ => {
                return Err(storage_error(lumvise_db_core::DbError::invalid_value(
                    "semantic result",
                    "snapshot sync result",
                )));
            }
        };
        Ok(json!({"snapshot_id": snapshot_id, "status": "committed", "report": report}))
    }

    pub(super) fn abort(
        &self,
        plugin_id: &str,
        snapshot_id: &str,
    ) -> Result<Value, HostCapabilityError> {
        let mut writes = self.lock()?;
        discard_expired(&mut writes);
        owned_write(&writes, snapshot_id, plugin_id)?;
        writes.remove(snapshot_id);
        Ok(json!({"snapshot_id": snapshot_id, "status": "aborted"}))
    }

    fn lock(
        &self,
    ) -> Result<
        std::sync::MutexGuard<'_, HashMap<String, StagedSemanticSnapshot>>,
        HostCapabilityError,
    > {
        self.writes.lock().map_err(|_| {
            write_error(
                "semantic_snapshot_write_lock_poisoned",
                "semantic snapshot write lock is poisoned; expected available App Core state",
                true,
            )
        })
    }
}

fn validate_identity(
    snapshot_id: &str,
    project_root: &str,
    page_count: usize,
) -> Result<(), HostCapabilityError> {
    if snapshot_id.trim().is_empty() || project_root.trim().is_empty() || page_count == 0 {
        return Err(write_error(
            "invalid_semantic_snapshot_write",
            format!(
                "invalid semantic snapshot identity id=`{snapshot_id}` root=`{project_root}` pages=`{page_count}`; expected non-empty id/root and positive page count"
            ),
            false,
        ));
    }
    Ok(())
}

fn validate_quota(
    snapshot_id: &str,
    records: usize,
    bytes: usize,
) -> Result<(), HostCapabilityError> {
    if records > MAX_STAGED_RECORDS || bytes > MAX_STAGED_BYTES {
        return Err(write_error(
            "semantic_snapshot_write_quota",
            format!(
                "semantic snapshot `{snapshot_id}` would stage `{records}` records and `{bytes}` bytes; expected at most `{MAX_STAGED_RECORDS}` records and `{MAX_STAGED_BYTES}` bytes"
            ),
            false,
        ));
    }
    Ok(())
}

fn relationship_key(relationship: &SemanticRelationship) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        relationship.source_element_id,
        relationship.target_element_id,
        relationship.relationship_kind,
        relationship.label
    )
}

fn owned_write<'a>(
    writes: &'a HashMap<String, StagedSemanticSnapshot>,
    snapshot_id: &str,
    plugin_id: &str,
) -> Result<&'a StagedSemanticSnapshot, HostCapabilityError> {
    writes
        .get(snapshot_id)
        .filter(|write| write.plugin_id == plugin_id)
        .ok_or_else(|| snapshot_not_found(snapshot_id, plugin_id))
}

fn owned_write_mut<'a>(
    writes: &'a mut HashMap<String, StagedSemanticSnapshot>,
    snapshot_id: &str,
    plugin_id: &str,
) -> Result<&'a mut StagedSemanticSnapshot, HostCapabilityError> {
    writes
        .get_mut(snapshot_id)
        .filter(|write| write.plugin_id == plugin_id)
        .ok_or_else(|| snapshot_not_found(snapshot_id, plugin_id))
}

fn discard_expired(writes: &mut HashMap<String, StagedSemanticSnapshot>) {
    writes.retain(|_, write| write.created_at.elapsed() < WRITE_TTL);
}

fn snapshot_not_found(snapshot_id: &str, plugin_id: &str) -> HostCapabilityError {
    write_error(
        "semantic_snapshot_write_not_found",
        format!(
            "semantic snapshot write `{snapshot_id}` is unavailable to plugin `{plugin_id}`; expected an active owned write"
        ),
        false,
    )
}

fn storage_error(error: lumvise_db_core::DbError) -> HostCapabilityError {
    write_error(
        "semantic_snapshot_commit_failed",
        format!("semantic snapshot commit failed: {error}"),
        false,
    )
}

fn write_error(code: &str, message: impl Into<String>, retryable: bool) -> HostCapabilityError {
    HostCapabilityError::new(SEMANTIC_STORAGE, code, message, retryable)
}
