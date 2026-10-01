use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use lumvise_resource_routing::InvocationControl;

use crate::interface::{
    ChangesSinceRevisionPage, RelationalOperation, RelationalPersistence, RelationalReadiness,
    RelationalResult, SemanticOperation, SemanticPersistence, SemanticReadiness, SemanticResult,
};
use crate::local::runtime::DbCore;
use crate::local::sql::mcp_instances::{self, NewMcpInstance};
use crate::local::sql::plugin_data::PluginDataRepository;
use crate::local::sql::plugin_deliveries::PluginDeliveryRepository;
use crate::local::sql::plugin_settings::PluginSettingsRepository;
use crate::local::sql::project_lineages::ProjectLineageRepository;
use crate::local::sql::settings::PersistentSettingsRepository;
use crate::{DbError, Result};

/// One local persistence composition over a single private runtime.
///
/// Semantic and relational trait implementations intentionally share this
/// runtime, including its SQLite writer coordination and staged snapshots.
pub struct LocalPersistence {
    pub(crate) core: Arc<DbCore>,
    staged_snapshots: Mutex<HashMap<String, StagedProjectSnapshot>>,
}

impl LocalPersistence {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Ok(Self {
            core: Arc::new(DbCore::open(path)?),
            staged_snapshots: Mutex::new(HashMap::new()),
        })
    }

    pub fn in_memory() -> Result<Self> {
        Ok(Self {
            core: Arc::new(DbCore::in_memory()?),
            staged_snapshots: Mutex::new(HashMap::new()),
        })
    }
}

struct StagedProjectSnapshot {
    project_root: String,
    partition_paths: Vec<String>,
    page_count: usize,
    pages: HashMap<
        usize,
        (
            Vec<crate::SemanticElement>,
            Vec<crate::SemanticRelationship>,
        ),
    >,
}

fn coalesce_staged_elements(
    elements: Vec<crate::SemanticElement>,
) -> Result<Vec<crate::SemanticElement>> {
    let mut unique = std::collections::BTreeMap::new();
    for element in elements {
        if let Some(previous) = unique.get(&element.semantic_element_id) {
            if previous != &element {
                return Err(DbError::invalid_value(
                    &element.semantic_element_id,
                    "identical records when a semantic id repeats across snapshot pages",
                ));
            }
            continue;
        }
        unique.insert(element.semantic_element_id.clone(), element);
    }
    Ok(unique.into_values().collect())
}

pub(super) fn verify_control(control: &InvocationControl) -> Result<()> {
    if control.is_cancelled() {
        return Err(DbError::invalid_value(
            "cancelled invocation",
            "active invocation",
        ));
    }
    if control.is_expired() {
        return Err(DbError::invalid_value(
            "expired invocation",
            "invocation before deadline",
        ));
    }
    Ok(())
}

impl SemanticPersistence for LocalPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> Result<SemanticResult> {
        verify_control(control)?;
        let storage = self
            .core
            .storage_manager()
            .semantic_storage()
            .with_control(control.clone());
        match operation {
            SemanticOperation::CreatePzSnapshot {
                project_root,
                output_path,
            } => Ok(SemanticResult::PzSnapshot(
                self.core.create_semantic_snapshot_controlled(
                    project_root,
                    output_path,
                    control,
                )?,
            )),
            SemanticOperation::ImportPzSnapshot {
                project_root,
                input_path,
            } => Ok(SemanticResult::PzImport(
                self.core
                    .import_semantic_snapshot_controlled(project_root, input_path, control)?,
            )),
            SemanticOperation::SyncStructure {
                project_root,
                elements,
                relationships,
            } => Ok(SemanticResult::SyncStructure(
                storage.sync_semantic_structure(&project_root, &elements, &relationships)?,
            )),
            SemanticOperation::BeginProjectSnapshot {
                snapshot_id,
                project_root,
                partition_paths,
                page_count,
            } => {
                if page_count == 0 {
                    return Err(DbError::invalid_value(
                        "0",
                        "positive project snapshot page count",
                    ));
                }
                let mut snapshots = self
                    .staged_snapshots
                    .lock()
                    .map_err(|_| DbError::Grafeo("snapshot staging registry poisoned".into()))?;
                if snapshots.contains_key(&snapshot_id) {
                    return Err(DbError::invalid_value(
                        snapshot_id,
                        "new project snapshot id",
                    ));
                }
                snapshots.insert(
                    snapshot_id.clone(),
                    StagedProjectSnapshot {
                        project_root,
                        partition_paths,
                        page_count,
                        pages: HashMap::new(),
                    },
                );
                Ok(SemanticResult::SnapshotStaging {
                    snapshot_id,
                    staged_pages: 0,
                    page_count,
                })
            }
            SemanticOperation::StageProjectSnapshot {
                snapshot_id,
                page_index,
                elements,
                relationships,
            } => {
                let mut snapshots = self
                    .staged_snapshots
                    .lock()
                    .map_err(|_| DbError::Grafeo("snapshot staging registry poisoned".into()))?;
                let snapshot = snapshots.get_mut(&snapshot_id).ok_or_else(|| {
                    DbError::invalid_value(&snapshot_id, "active project snapshot")
                })?;
                if page_index >= snapshot.page_count {
                    return Err(DbError::invalid_value(
                        page_index.to_string(),
                        "project snapshot page index",
                    ));
                }
                snapshot.pages.insert(page_index, (elements, relationships));
                Ok(SemanticResult::SnapshotStaging {
                    snapshot_id,
                    staged_pages: snapshot.pages.len(),
                    page_count: snapshot.page_count,
                })
            }
            SemanticOperation::CommitProjectSnapshot { snapshot_id } => {
                let (project_root, partition_paths, page_count, pages) = {
                    let snapshots = self.staged_snapshots.lock().map_err(|_| {
                        DbError::Grafeo("snapshot staging registry poisoned".into())
                    })?;
                    let snapshot = snapshots.get(&snapshot_id).ok_or_else(|| {
                        DbError::invalid_value(&snapshot_id, "active project snapshot")
                    })?;
                    (
                        snapshot.project_root.clone(),
                        snapshot.partition_paths.clone(),
                        snapshot.page_count,
                        snapshot.pages.clone(),
                    )
                };
                if pages.len() != page_count {
                    return Err(DbError::invalid_value(
                        pages.len().to_string(),
                        format!("all {page_count} staged project snapshot pages"),
                    ));
                }
                let mut ordered_pages = pages.into_iter().collect::<Vec<_>>();
                ordered_pages.sort_by_key(|(page_index, _)| *page_index);
                let mut elements = Vec::new();
                let mut relationships = Vec::new();
                for (_, (page_elements, page_relationships)) in ordered_pages {
                    elements.extend(page_elements);
                    relationships.extend(page_relationships);
                }
                let elements = coalesce_staged_elements(elements)?;
                let report = if partition_paths.is_empty() {
                    storage.sync_semantic_structure(&project_root, &elements, &relationships)?
                } else {
                    storage.sync_semantic_partition(
                        &crate::SemanticPartition {
                            project_root,
                            replace_paths: partition_paths,
                        },
                        &elements,
                        &relationships,
                    )?
                };
                self.staged_snapshots
                    .lock()
                    .map_err(|_| DbError::Grafeo("snapshot staging registry poisoned".into()))?
                    .remove(&snapshot_id);
                Ok(SemanticResult::SnapshotCommitted(report))
            }
            SemanticOperation::AbortProjectSnapshot { snapshot_id } => {
                let discarded_pages = self
                    .staged_snapshots
                    .lock()
                    .map_err(|_| DbError::Grafeo("snapshot staging registry poisoned".into()))?
                    .remove(&snapshot_id)
                    .map(|snapshot| snapshot.pages.len())
                    .unwrap_or(0);
                Ok(SemanticResult::SnapshotAborted {
                    snapshot_id,
                    discarded_pages,
                })
            }
            SemanticOperation::SyncPartition {
                partition,
                elements,
                relationships,
            } => Ok(SemanticResult::SyncPartition(
                storage.sync_semantic_partition(&partition, &elements, &relationships)?,
            )),
            SemanticOperation::Element {
                semantic_element_id,
            } => Ok(SemanticResult::Element(
                storage.element(&semantic_element_id)?,
            )),
            SemanticOperation::ElementsByIds {
                project_root,
                semantic_element_ids,
            } => Ok(SemanticResult::Elements(
                storage.elements_by_ids(&project_root, &semantic_element_ids)?,
            )),
            SemanticOperation::ElementsByIdsIncludingInactive {
                project_root,
                semantic_element_ids,
            } => Ok(SemanticResult::Elements(
                storage.elements_by_ids_including_inactive(&project_root, &semantic_element_ids)?,
            )),
            SemanticOperation::ArtifactsByIds { artifact_ids } => Ok(SemanticResult::Artifacts(
                storage.artifacts_by_ids(&artifact_ids)?,
            )),
            SemanticOperation::SearchElementCandidates {
                project_root,
                query,
                limit,
            } => Ok(SemanticResult::Elements(
                storage.search_element_candidates(project_root.as_deref(), &query, limit)?,
            )),
            SemanticOperation::SearchElementNameVectors {
                project_root,
                query,
                k,
                engine_id,
                model,
            } => Ok(SemanticResult::ElementVectorSearch(
                storage.search_element_name_vectors(
                    &project_root,
                    &query,
                    k,
                    &engine_id,
                    model.as_deref(),
                )?,
            )),
            SemanticOperation::StoreElementNameVectors {
                project_root,
                vectors,
            } => {
                storage.store_element_name_vectors(&project_root, &vectors)?;
                Ok(SemanticResult::StoredElementNameVectors {
                    count: vectors.len(),
                })
            }
            SemanticOperation::SearchArtifactTextVectors {
                project_root,
                query,
                k,
                engine_id,
                model,
            } => Ok(SemanticResult::ArtifactVectorSearch(
                storage.search_artifact_text_vectors(
                    &project_root,
                    &query,
                    k,
                    &engine_id,
                    model.as_deref(),
                )?,
            )),
            SemanticOperation::StoreArtifactTextVectors {
                project_root,
                vectors,
            } => {
                storage.store_artifact_text_vectors(&project_root, &vectors)?;
                Ok(SemanticResult::StoredArtifactTextVectors {
                    count: vectors.len(),
                })
            }
            SemanticOperation::RelationshipsFrom {
                semantic_element_id,
            } => Ok(SemanticResult::Relationships(
                storage.relationships_from(&semantic_element_id)?,
            )),
            SemanticOperation::RelationshipsTouchingElements {
                semantic_element_ids,
            } => Ok(SemanticResult::Relationships(
                storage.relationships_touching_elements(&semantic_element_ids)?,
            )),
            SemanticOperation::Artifact { artifact_id } => {
                Ok(SemanticResult::Artifact(storage.artifact(&artifact_id)?))
            }
            SemanticOperation::ArtifactBlobGet { content_ref } => Ok(SemanticResult::ArtifactBlob(
                self.core.artifact_blobs().blob(&content_ref)?,
            )),
            SemanticOperation::ArtifactBlobPut {
                content_ref,
                artifact_id,
                media_type,
                content,
            } => {
                self.core.artifact_blobs().put_blob(
                    &content_ref,
                    &artifact_id,
                    &media_type,
                    &content,
                )?;
                Ok(SemanticResult::ArtifactBlob(
                    self.core.artifact_blobs().blob(&content_ref)?,
                ))
            }
            SemanticOperation::ArtifactsForElements {
                semantic_element_ids,
            } => Ok(SemanticResult::Artifacts(
                storage.artifacts_for_elements(&semantic_element_ids)?,
            )),
            SemanticOperation::ArtifactsForElementWithInheritance {
                semantic_element_id,
            } => Ok(SemanticResult::Artifacts(
                storage.artifacts_for_element_with_inheritance(&semantic_element_id)?,
            )),
            SemanticOperation::ArtifactDependents {
                target_kind,
                target_id,
            } => Ok(SemanticResult::Artifacts(
                storage.artifact_dependents(&target_kind, &target_id)?,
            )),
            SemanticOperation::UpsertArtifact {
                mut artifact,
                media_type,
            } => {
                let artifact_id = artifact.artifact_id.clone();
                if let Some(content) = artifact.content.take() {
                    storage.upsert_artifact_content(&artifact, &media_type, content.as_bytes())?;
                } else {
                    storage.upsert_artifact(&artifact)?;
                }
                Ok(SemanticResult::UpsertedArtifact { artifact_id })
            }
            SemanticOperation::RemoveArtifact { artifact_id } => Ok(SemanticResult::Removed {
                removed: storage.remove_artifact(&artifact_id)?,
            }),
            SemanticOperation::RemoveElement {
                semantic_element_id,
            } => Ok(SemanticResult::Removed {
                removed: storage.remove_element(&semantic_element_id)?,
            }),
            SemanticOperation::ProjectRoots => Ok(SemanticResult::ProjectRoots(
                storage.semantic_project_roots()?,
            )),
            SemanticOperation::ProjectIdentity { project_root } => {
                Ok(SemanticResult::ProjectIdentity(
                    ProjectLineageRepository::new(&self.core.conn).get_or_create(&project_root)?,
                ))
            }
            SemanticOperation::SemanticRevision => Ok(SemanticResult::SemanticRevision {
                commit_version: self.core.latest_published_commit_version()?,
            }),
            SemanticOperation::ProjectArtifacts {
                project_root,
                artifact_namespace,
            } => Ok(SemanticResult::Artifacts(storage.project_artifacts(
                &project_root,
                artifact_namespace.as_deref(),
            )?)),
            SemanticOperation::ProjectElementCounts { project_root } => {
                storage.project_element_counts(&project_root)
            }
            SemanticOperation::ScopedRead(request) => {
                Ok(SemanticResult::ScopedGraph(storage.scoped_read(&request)?))
            }
            SemanticOperation::ProjectSnapshot {
                scope,
                artifact_namespace,
            } => Ok(SemanticResult::ProjectSnapshot(
                storage.project_snapshot(&scope, artifact_namespace.as_deref())?,
            )),
            SemanticOperation::SelectiveSubgraph {
                project_root,
                root_element_id,
                artifact_namespace,
            } => Ok(SemanticResult::SelectiveSubgraph(
                storage.selective_subgraph(
                    &project_root,
                    &root_element_id,
                    artifact_namespace.as_deref(),
                )?,
            )),
            SemanticOperation::ProjectRendererGraph(request) => Ok(
                SemanticResult::RendererGraphProjection(storage.project_renderer_graph(&request)?),
            ),
            SemanticOperation::ChangesSinceRevision {
                scope,
                after_revision,
                limit,
            } => {
                let batch =
                    self.core
                        .change_hooks()
                        .changes_since(&scope, after_revision, limit)?;
                let head = self.core.latest_published_commit_version()?;
                Ok(SemanticResult::ChangesSinceRevision(
                    ChangesSinceRevisionPage {
                        base_revision: batch.base_revision,
                        target_revision: batch.target_revision,
                        head_revision: head,
                        has_more: batch.target_revision < head,
                        changed: batch.changed,
                    },
                ))
            }
            SemanticOperation::RegisterChangeHook { hook_name, scope } => {
                Ok(SemanticResult::ChangeHookRegistration(
                    self.core.change_hooks().register(&hook_name, scope)?,
                ))
            }
            SemanticOperation::ChangeHookRegistrations => Ok(
                SemanticResult::ChangeHookRegistrations(self.core.change_hooks().registrations()?),
            ),
            SemanticOperation::ChangeHookDirtyBatch {
                hook_name,
                maximum_elements,
            } => Ok(SemanticResult::ChangeHookBatch(
                self.core
                    .change_hooks()
                    .dirty_batch(&hook_name, maximum_elements)?,
            )),
            SemanticOperation::DeregisterChangeHook { hook_name } => {
                Ok(SemanticResult::ChangeHookDeregistered {
                    removed: self.core.change_hooks().deregister(&hook_name)?,
                })
            }
            SemanticOperation::AcknowledgeChangeHook { batch } => {
                Ok(SemanticResult::ChangeHookAcknowledged {
                    advanced: self.core.change_hooks().acknowledge(&batch)?,
                })
            }
            SemanticOperation::WaitForSemanticRevision {
                after_revision,
                timeout_ms,
            } => Ok(SemanticResult::SemanticRevision {
                commit_version: self.core.wait_for_semantic_revision(
                    after_revision,
                    timeout_ms.map(Duration::from_millis),
                )?,
            }),
            SemanticOperation::Maintenance => {
                self.core.maintenance()?;
                Ok(SemanticResult::MaintenanceCompleted)
            }
            SemanticOperation::CandidateSourceElements {
                content_fingerprints,
                kind_name_keys,
                kind_file_name_keys,
            } => Ok(SemanticResult::Elements(
                storage.candidate_source_elements(
                    &content_fingerprints,
                    &kind_name_keys,
                    &kind_file_name_keys,
                )?,
            )),
        }
    }

    fn readiness(&self) -> Result<SemanticReadiness> {
        Ok(SemanticReadiness {
            ready: self.core.semantic_recovery_status()?.is_none(),
        })
    }
}

impl RelationalPersistence for LocalPersistence {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> Result<RelationalResult> {
        verify_control(control)?;
        let settings = PersistentSettingsRepository::new(&self.core.conn);
        let plugin_settings = PluginSettingsRepository::new(&self.core.conn);
        let plugin_data = PluginDataRepository::new(&self.core.conn);
        let deliveries = PluginDeliveryRepository::new(&self.core.conn);
        match operation {
            RelationalOperation::GetPersistentSetting { scope, key } => Ok(
                RelationalResult::PersistentSetting(settings.get_json(&scope, &key)?),
            ),
            RelationalOperation::SetPersistentSetting { scope, key, value } => {
                settings.set_json(&scope, &key, &value)?;
                Ok(RelationalResult::PersistentSettingUpdated(
                    settings
                        .get_json(&scope, &key)?
                        .ok_or_else(|| DbError::invalid_value(key, "persisted setting"))?,
                ))
            }
            RelationalOperation::GetPluginSetting { plugin_id } => Ok(
                RelationalResult::PluginSetting(plugin_settings.plugin_settings(&plugin_id)?),
            ),
            RelationalOperation::SetPluginSetting {
                plugin_id,
                enabled,
                config,
            } => {
                plugin_settings.set_config(&plugin_id, enabled, &config)?;
                Ok(RelationalResult::PluginSettingUpdated(
                    plugin_settings
                        .plugin_settings(&plugin_id)?
                        .ok_or_else(|| {
                            DbError::invalid_value(plugin_id, "persisted plugin setting")
                        })?,
                ))
            }
            RelationalOperation::EnsurePluginDataTable {
                plugin_id,
                table_name,
                schema,
            } => Ok(RelationalResult::PluginDataTable(
                plugin_data.ensure_table(&plugin_id, &table_name, &schema)?,
            )),
            RelationalOperation::UpdatePluginDataTable {
                plugin_id,
                table_name,
                schema,
            } => Ok(RelationalResult::PluginDataTable(
                plugin_data.ensure_table(&plugin_id, &table_name, &schema)?,
            )),
            RelationalOperation::ListPluginDataTables { plugin_id } => Ok(
                RelationalResult::PluginDataTables(plugin_data.tables(&plugin_id)?),
            ),
            RelationalOperation::PutPluginData {
                plugin_id,
                table_name,
                row_key,
                value,
            } => Ok(RelationalResult::PluginDataRow(Some(plugin_data.put_row(
                &plugin_id,
                &table_name,
                &row_key,
                &value,
            )?))),
            RelationalOperation::GetPluginData {
                plugin_id,
                table_name,
                row_key,
            } => Ok(RelationalResult::PluginDataRow(plugin_data.row(
                &plugin_id,
                &table_name,
                &row_key,
            )?)),
            RelationalOperation::DeletePluginData {
                plugin_id,
                table_name,
                row_key,
            } => Ok(RelationalResult::PluginDataMutations(
                plugin_data.apply_mutations(
                    &plugin_id,
                    &[crate::PluginDataMutation::Delete {
                        table_name,
                        row_key,
                    }],
                )?,
            )),
            RelationalOperation::ListPluginData {
                plugin_id,
                table_name,
            } => Ok(RelationalResult::PluginDataRows(
                plugin_data.rows(&plugin_id, &table_name)?,
            )),
            RelationalOperation::PagePluginData {
                plugin_id,
                table_name,
                key_prefix,
                after_key,
                limit,
            } => Ok(RelationalResult::PluginDataPage(plugin_data.rows_page(
                &plugin_id,
                &table_name,
                key_prefix.as_deref(),
                after_key.as_deref(),
                limit,
            )?)),
            RelationalOperation::TrimPluginData {
                plugin_id,
                table_name,
                retained_rows,
            } => Ok(RelationalResult::PluginDataTrimmed {
                removed_rows: plugin_data.trim_rows_by_key(
                    &plugin_id,
                    &table_name,
                    retained_rows,
                )?,
            }),
            RelationalOperation::ApplyMutations {
                plugin_id,
                mutations,
            } => Ok(RelationalResult::PluginDataMutations(
                plugin_data.apply_mutations(&plugin_id, &mutations)?,
            )),
            RelationalOperation::SyncBackgroundRegistrations { registrations } => {
                deliveries.sync_registrations(&registrations)?;
                Ok(RelationalResult::BackgroundRegistrations(
                    deliveries.active_registrations()?,
                ))
            }
            RelationalOperation::ListBackgroundRegistrations => Ok(
                RelationalResult::BackgroundRegistrations(deliveries.active_registrations()?),
            ),
            RelationalOperation::EnqueueRecurring {
                plugin_id,
                export_id,
                scheduled_at,
                now_unix_seconds,
                interval_seconds,
                payload,
            } => Ok(RelationalResult::BackgroundDelivery(
                deliveries.enqueue_recurring(
                    &plugin_id,
                    &export_id,
                    scheduled_at,
                    now_unix_seconds,
                    interval_seconds,
                    &payload,
                )?,
            )),
            RelationalOperation::EnqueueChangeHookBatch {
                plugin_id,
                export_id,
                now_unix_seconds,
                deliveries: batch,
            } => Ok(RelationalResult::ChangeHookBatchEnqueued {
                inserted: deliveries.enqueue_change_hook_batch(
                    &plugin_id,
                    &export_id,
                    now_unix_seconds,
                    &batch,
                )?,
            }),
            RelationalOperation::DueDeliveriesByKind {
                delivery_kind,
                now_unix_seconds,
                limit,
            } => Ok(RelationalResult::BackgroundDeliveries(
                deliveries.due_deliveries_by_kind(&delivery_kind, now_unix_seconds, limit)?,
            )),
            RelationalOperation::ClaimDelivery {
                delivery_id,
                now_unix_seconds,
                lease_seconds,
            } => Ok(RelationalResult::BackgroundDelivery(deliveries.claim(
                &delivery_id,
                now_unix_seconds,
                lease_seconds,
            )?)),
            RelationalOperation::CompleteDelivery { delivery_id } => {
                Ok(RelationalResult::DeliveryTransition {
                    changed: deliveries.complete(&delivery_id)?,
                })
            }
            RelationalOperation::RetryDelivery {
                delivery_id,
                next_attempt_at,
                error,
            } => Ok(RelationalResult::DeliveryTransition {
                changed: deliveries.retry(&delivery_id, next_attempt_at, &error)?,
            }),
            RelationalOperation::DeadLetterDelivery { delivery_id, error } => {
                Ok(RelationalResult::DeliveryTransition {
                    changed: deliveries.dead_letter(&delivery_id, &error)?,
                })
            }
            RelationalOperation::PruneDeadLetters {
                plugin_id,
                export_id,
                updated_before,
                max_entries,
            } => Ok(RelationalResult::DeadLettersPruned {
                removed_deliveries: deliveries.prune_dead_letters(
                    &plugin_id,
                    &export_id,
                    &updated_before,
                    max_entries,
                )?,
            }),
            RelationalOperation::DeliveryDiagnostics { delivery_id } => Ok(
                RelationalResult::BackgroundDelivery(deliveries.delivery(&delivery_id)?),
            ),
            RelationalOperation::RegisterMcpInstance {
                instance_id,
                project_root,
                display_name,
                capabilities_json,
                control_channel_json,
            } => {
                let instance = NewMcpInstance {
                    instance_id: &instance_id,
                    project_root: &project_root,
                    display_name: &display_name,
                    capabilities_json: &capabilities_json,
                    control_channel_json: control_channel_json.as_deref(),
                };
                let conn = self.core.conn.write_conn();
                mcp_instances::register_mcp_instance(&conn, &instance)?;
                Ok(RelationalResult::McpInstance(
                    mcp_instances::required_mcp_instance(&conn, &instance_id)?,
                ))
            }
            RelationalOperation::HeartbeatMcpInstance {
                instance_id,
                project_root,
                status,
            } => {
                let conn = self.core.conn.write_conn();
                mcp_instances::heartbeat_mcp_instance(&conn, &instance_id, &project_root, &status)?;
                Ok(RelationalResult::McpInstance(
                    mcp_instances::required_mcp_instance(&conn, &instance_id)?,
                ))
            }
            RelationalOperation::ActiveMcpInstances => Ok(RelationalResult::McpInstances(
                mcp_instances::active_mcp_instances(&self.core.conn.read_conn())?,
            )),
            RelationalOperation::AllMcpInstances => Ok(RelationalResult::McpInstances(
                mcp_instances::all_mcp_instances(&self.core.conn.read_conn())?,
            )),
            RelationalOperation::DeregisterMcpInstance { instance_id } => {
                mcp_instances::deregister_mcp_instance(&self.core.conn.write_conn(), &instance_id)?;
                Ok(RelationalResult::McpInstanceDeregistered { instance_id })
            }
        }
    }

    fn readiness(&self) -> Result<RelationalReadiness> {
        Ok(RelationalReadiness { ready: true })
    }
}

#[cfg(test)]
mod control_tests;
