use super::*;

impl<'db> SemanticStorage<'db> {
    #[cfg(test)]
    pub fn upsert_element(&self, element: &SemanticElement) -> Result<()> {
        validate_element(element)?;
        self.commit_preplanned_graph_write(
            |database| prepare_element_upsert(database, element),
            |graph, commit_version, collector, plan| {
                apply_element_upsert(graph, element, commit_version, plan)?;
                collector.record_element(element, crate::ChangeDisposition::Upserted);
                Ok(())
            },
        )
    }

    /// Synchronizes a complete semantic structure batch for one project.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::SyncStructure {
    ///         project_root: "/repo".into(),
    ///         elements: vec![],
    ///         relationships: vec![],
    ///     },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::SyncStructure(report) if report.elements_upserted == 0));
    /// ```
    pub fn sync_semantic_structure(
        &self,
        project_root: &str,
        elements: &[SemanticElement],
        relationships: &[SemanticRelationship],
    ) -> Result<SemanticBatchSyncReport> {
        require_non_empty(project_root, "non-empty project root")?;
        let _sync = self.graph.lock_structure_sync(&self.control)?;
        let initial = self.graph.plan(|database| {
            prepare_full_structure_sync(
                database,
                project_root,
                elements,
                relationships,
                &self.control,
            )
        })?;
        self.check_active()?;
        if initial.value().is_noop() {
            return Ok(initial.into_parts().1.report);
        }
        let report = self.commit_prepared_graph_write(
            initial,
            |database| {
                prepare_full_structure_sync(
                    database,
                    project_root,
                    elements,
                    relationships,
                    &self.control,
                )
            },
            |graph, commit_version, collector, prepared| {
                let report = prepared.report.clone();
                apply_prepared_structure_sync(
                    graph,
                    commit_version,
                    collector,
                    project_root,
                    prepared,
                )?;
                Ok(report)
            },
        )?;
        Ok(report)
    }

    /// Synchronizes one project path partition without replacing unrelated graph regions.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult, SemanticPartition};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let partition = SemanticPartition {
    ///     project_root: "/repo".into(),
    ///     replace_paths: vec!["src/lib.rs".into()],
    /// };
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::SyncPartition { partition, elements: vec![], relationships: vec![] },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::SyncPartition(report) if report.elements_upserted == 0));
    /// ```
    pub fn sync_semantic_partition(
        &self,
        partition: &SemanticPartition,
        elements: &[SemanticElement],
        relationships: &[SemanticRelationship],
    ) -> Result<SemanticBatchSyncReport> {
        validate_partition(partition)?;
        let _sync = self.graph.lock_structure_sync(&self.control)?;
        let initial = self.graph.plan(|database| {
            prepare_partition_structure_sync(database, partition, elements, relationships)
        })?;
        self.check_active()?;
        if initial.value().is_noop() {
            return Ok(initial.into_parts().1.report);
        }
        let report = self.commit_prepared_graph_write(
            initial,
            |database| {
                prepare_partition_structure_sync(database, partition, elements, relationships)
            },
            |graph, commit_version, collector, prepared| {
                let report = prepared.report.clone();
                apply_prepared_structure_sync(
                    graph,
                    commit_version,
                    collector,
                    &partition.project_root,
                    prepared,
                )?;
                Ok(report)
            },
        )?;
        Ok(report)
    }

    #[cfg(test)]
    pub fn upsert_media_annotation_for_path(
        &self,
        project_root: &str,
        target_path: &str,
        anchor_selector: &str,
        artifact: &SemanticArtifact,
    ) -> Result<()> {
        require_non_empty(project_root, "non-empty project root")?;
        require_non_empty(target_path, "non-empty media target path")?;
        require_non_empty(anchor_selector, "non-empty media anchor selector")?;
        validate_artifact_shell(artifact)?;
        self.commit_preplanned_graph_write(
            |database| {
                let media_element = active_media_element(database, project_root, target_path)?;
                let artifact = annotation_for_media_element(
                    artifact,
                    &media_element,
                    target_path,
                    anchor_selector,
                );
                let plan = prepare_artifact_upsert(database, &artifact, None)?;
                Ok((artifact, plan))
            },
            |graph, _commit_version, collector, (artifact, plan)| {
                let owner = apply_artifact_upsert(graph, &artifact, _commit_version, plan)?;
                collector.record_element(&owner, ChangeDisposition::Upserted);
                Ok(())
            },
        )
    }
    #[cfg(test)]
    pub fn link_elements(&self, relationship: &SemanticRelationship) -> Result<()> {
        validate_relationship(relationship)?;
        self.require_element_exists(&relationship.source_element_id)?;
        self.require_element_exists(&relationship.target_element_id)?;
        self.commit_preplanned_graph_write(
            |database| {
                if semantic_element_by_id(database, &relationship.source_element_id).is_none() {
                    return Err(DbError::invalid_value(
                        &relationship.source_element_id,
                        "existing semantic relationship source",
                    ));
                }
                if semantic_element_by_id(database, &relationship.target_element_id).is_none() {
                    return Err(DbError::invalid_value(
                        &relationship.target_element_id,
                        "existing semantic relationship target",
                    ));
                }
                prepare_relationship_upserts(
                    database,
                    std::slice::from_ref(relationship),
                    &BTreeMap::new(),
                )
            },
            |graph, _commit_version, collector, plan| {
                let changed_node_ids = plan.changed_element_node_ids().clone();
                apply_relationship_write(graph, plan)?;
                for node_id in changed_node_ids.values() {
                    if let Some(element) = graph
                        .get_node(*node_id)
                        .filter(|node| node.has_label("SemanticElement"))
                        .and_then(|node| semantic_element_from_node(&node))
                    {
                        collector.record_element(&element, ChangeDisposition::Upserted);
                    }
                }
                Ok(())
            },
        )
    }
    pub(crate) fn require_element_exists(&self, semantic_element_id: &str) -> Result<()> {
        if self.element(semantic_element_id)?.is_some() {
            return Ok(());
        }
        Err(DbError::invalid_value(
            semantic_element_id,
            "existing semantic element id",
        ))
    }
}

pub(super) fn normalized_relationships(
    project_root: &str,
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    remaps: &[crate::domain::matching::SemanticIdentityRemap],
) -> Vec<SemanticRelationship> {
    crate::domain::relationships::normalized_relationships(
        project_root,
        elements,
        relationships,
        remaps,
    )
}

pub(super) fn semantic_sync_report(
    reconciled: &crate::domain::matching::SemanticStructureReconciliation,
    relationship_count: usize,
) -> SemanticBatchSyncReport {
    SemanticBatchSyncReport {
        elements_upserted: reconciled.active_elements.len(),
        relationships_upserted: relationship_count,
        identities_reused: reconciled.remaps.len(),
        elements_marked_inactive: reconciled.inactive_element_ids.len(),
    }
}

pub(super) fn semantic_partition_plan(
    existing_partition: &[SemanticElement],
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
) -> PartitionSyncPlan {
    let reconciled = reconcile_semantic_elements(existing_partition, elements);
    let inactive_elements = inactive_elements(existing_partition, &reconciled.inactive_element_ids);
    let element_changes = changed_semantic_elements(
        existing_partition,
        &reconciled.active_elements,
        &inactive_elements,
    );
    let project_root = elements
        .first()
        .map(|element| element.project_root.as_str())
        .unwrap_or_default();
    let relationships = normalized_relationships(
        project_root,
        &reconciled.active_elements,
        relationships,
        &reconciled.remaps,
    );
    PartitionSyncPlan {
        reconciled,
        inactive_elements,
        element_changes,
        relationships,
    }
}

pub(super) fn prepare_snapshot_write(
    database: &grafeo::GrafeoDB,
    project_root: &str,
    active_elements: &[SemanticElement],
    inactive_elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
) -> Result<PreparedSnapshot> {
    let elements = active_elements
        .iter()
        .chain(inactive_elements.iter())
        .collect::<Vec<_>>();
    let element_plans = elements
        .iter()
        .map(|element| prepare_element_upsert(database, element))
        .collect::<Result<Vec<_>>>()?;
    let known_node_ids = planned_upsert_node_ids(elements.iter().copied(), &element_plans);
    Ok(PreparedSnapshot {
        relationship_reset: prepare_project_relationship_reset(database, project_root),
        element_plans,
        relationship_plan: prepare_relationship_upserts(database, relationships, &known_node_ids)?,
    })
}

pub(super) fn apply_prepared_snapshot(
    graph: &GraphTransaction<'_>,
    active_elements: &[SemanticElement],
    inactive_elements: &[SemanticElement],
    commit_version: i64,
    plan: PreparedSnapshot,
) -> Result<BTreeMap<String, NodeId>> {
    let relationship_changes = plan.relationship_plan.changed_element_node_ids().clone();
    // Rebuild only semantic relationships: native element identities anchor
    // artifact ownership/dependencies, and vectors follow delta-sync semantics.
    apply_project_relationship_reset(graph, plan.relationship_reset);
    for (element, element_plan) in active_elements
        .iter()
        .chain(inactive_elements.iter())
        .zip(plan.element_plans)
    {
        apply_element_upsert(graph, element, commit_version, element_plan)?;
    }
    apply_relationship_write(graph, plan.relationship_plan)?;
    Ok(relationship_changes)
}

pub(super) fn prepare_full_structure_sync(
    database: &grafeo::GrafeoDB,
    project_root: &str,
    elements: &[SemanticElement],
    input_relationships: &[SemanticRelationship],
    control: &lumvise_resource_routing::InvocationControl,
) -> Result<PreparedStructureSync> {
    crate::local::persistence::verify_control(control)?;
    validate_unique_element_ids(elements)?;
    let existing = semantic_elements_for_project_including_inactive(database, project_root);
    crate::local::persistence::verify_control(control)?;
    validate_duplicate_repair_inputs(&existing, elements)?;
    let existing_relationships = semantic_relationships_for_project_edges(database, project_root);
    crate::local::persistence::verify_control(control)?;
    let reconciled = reconcile_semantic_elements(&existing, elements);
    let relationships = normalized_relationships(
        project_root,
        &reconciled.active_elements,
        input_relationships,
        &reconciled.remaps,
    );
    crate::local::persistence::verify_control(control)?;
    let inactive_elements = inactive_elements(&existing, &reconciled.inactive_element_ids);
    let element_changes =
        changed_semantic_elements(&existing, &reconciled.active_elements, &inactive_elements);
    let changed_ids: HashSet<&str> = element_changes
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect();
    let changed_inactive_elements = inactive_elements
        .iter()
        .filter(|element| changed_ids.contains(element.semantic_element_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let mut changed_relationship_sources =
        changed_relationship_source_ids(&existing_relationships, &relationships);
    changed_relationship_sources.extend(
        duplicate_element_ids(&existing)
            .into_iter()
            .map(str::to_owned),
    );
    crate::local::persistence::verify_control(control)?;
    let report = semantic_sync_report(&reconciled, relationships.len());
    if element_changes.is_empty() && changed_relationship_sources.is_empty() {
        return Ok(PreparedStructureSync {
            report,
            remaps: reconciled.remaps,
            mutation: PreparedStructureMutation::NoOp,
        });
    }
    validate_semantic_sync_inputs(
        &reconciled.active_elements,
        &inactive_elements,
        &relationships,
    )?;
    // Duplicate repair owns attachment rebinding; the bulk path preserves unique native nodes.
    let mutation = if duplicate_element_ids(&existing).is_empty()
        && should_replace_snapshot(&existing, &element_changes)
    {
        PreparedStructureMutation::Snapshot {
            plan: prepare_snapshot_write(
                database,
                project_root,
                &reconciled.active_elements,
                &changed_inactive_elements,
                &relationships,
            )?,
            active_elements: reconciled.active_elements,
            inactive_elements: changed_inactive_elements,
        }
    } else {
        let changed_relationships =
            relationships_for_sources(&relationships, &changed_relationship_sources);
        PreparedStructureMutation::Delta {
            plan: prepare_delta_write(
                database,
                &element_changes,
                &changed_relationship_sources,
                &changed_relationships,
            )?,
            element_changes,
        }
    };
    Ok(PreparedStructureSync {
        report,
        remaps: reconciled.remaps,
        mutation,
    })
}

pub(super) fn prepare_partition_structure_sync(
    database: &grafeo::GrafeoDB,
    partition: &SemanticPartition,
    elements: &[SemanticElement],
    input_relationships: &[SemanticRelationship],
) -> Result<PreparedStructureSync> {
    validate_unique_element_ids(elements)?;
    let existing_partition = if partition_paths_are_exact(&partition.replace_paths, elements) {
        let paths = incoming_partition_paths(&partition.replace_paths, elements);
        semantic_elements_for_paths_including_inactive(database, &partition.project_root, &paths)
    } else {
        elements_in_partition(
            &semantic_elements_for_project_including_inactive(database, &partition.project_root),
            &partition.replace_paths,
        )
    };
    validate_duplicate_repair_inputs(&existing_partition, elements)?;
    let plan = semantic_partition_plan(&existing_partition, elements, input_relationships);
    let owned_sources = owned_partition_source_ids(&plan.reconciled, &plan.inactive_elements);
    let relationships = relationships_owned_by_sources(&plan.relationships, &owned_sources);
    let mut existing_relationships =
        semantic_relationships_from_many_native_edges(database, &owned_sources);
    if existing_relationships.is_empty() {
        existing_relationships =
            semantic_relationships_for_project_edges(database, &partition.project_root)
                .into_iter()
                .filter(|relationship| owned_sources.contains(&relationship.source_element_id))
                .collect();
    }
    let mut changed_relationship_sources =
        changed_relationship_source_ids(&existing_relationships, &relationships);
    changed_relationship_sources.extend(
        duplicate_element_ids(&existing_partition)
            .into_iter()
            .map(str::to_owned),
    );
    let report = semantic_sync_report(&plan.reconciled, relationships.len());
    if plan.element_changes.is_empty() && changed_relationship_sources.is_empty() {
        return Ok(PreparedStructureSync {
            report,
            remaps: plan.reconciled.remaps,
            mutation: PreparedStructureMutation::NoOp,
        });
    }
    validate_partition_write_inputs(&plan.element_changes, &relationships)?;
    Ok(PreparedStructureSync {
        report,
        remaps: plan.reconciled.remaps,
        mutation: PreparedStructureMutation::Delta {
            plan: prepare_delta_write(
                database,
                &plan.element_changes,
                &changed_relationship_sources,
                &relationships,
            )?,
            element_changes: plan.element_changes,
        },
    })
}

pub(super) fn apply_prepared_structure_sync(
    graph: &GraphTransaction<'_>,
    commit_version: i64,
    collector: &mut ChangeCollector,
    project_root: &str,
    prepared: PreparedStructureSync,
) -> Result<()> {
    match prepared.mutation {
        PreparedStructureMutation::NoOp => Ok(()),
        PreparedStructureMutation::Snapshot {
            active_elements,
            inactive_elements,
            plan,
        } => {
            let relationship_changes = apply_prepared_snapshot(
                graph,
                &active_elements,
                &inactive_elements,
                commit_version,
                plan,
            )?;
            record_element_upserts(collector, &active_elements)?;
            record_element_upserts(collector, &inactive_elements)?;
            record_relationship_endpoints(graph, collector, &relationship_changes);
            record_sync_changes(collector, project_root, &prepared.remaps)
        }
        PreparedStructureMutation::Delta {
            element_changes,
            plan,
        } => {
            let relationship_changes =
                apply_prepared_delta(graph, &element_changes, commit_version, plan)?;
            record_element_upserts(collector, &element_changes)?;
            record_relationship_endpoints(graph, collector, &relationship_changes);
            record_sync_changes(collector, project_root, &prepared.remaps)
        }
    }
}

/// Fires one `semantic.element.upserted` change per element actually written by
/// this sync. Callers rely on the diff `sync_semantic_structure`/
/// `sync_semantic_partition` already computed (the changed-elements set for a
/// `Delta`, or the full live set for a `Snapshot` replace) rather than
/// recomputing it - StorageTrigger consumers (e.g. lazy vector-index
/// maintenance) depend on this firing for bulk structural ingest, not just the
/// single-element `upsert_element` path.
pub(super) fn record_element_upserts(
    collector: &mut ChangeCollector,
    elements: &[SemanticElement],
) -> Result<()> {
    for element in elements {
        let disposition = if element.lifecycle == "inactive" {
            crate::ChangeDisposition::Removal
        } else {
            crate::ChangeDisposition::Upserted
        };
        collector.record_element(element, disposition);
    }
    Ok(())
}

pub(super) fn record_relationship_endpoints(
    graph: &GraphTransaction<'_>,
    collector: &mut ChangeCollector,
    changed_element_node_ids: &BTreeMap<String, NodeId>,
) {
    for node_id in changed_element_node_ids.values() {
        let element = graph
            .get_node(*node_id)
            .filter(|node| node.has_label("SemanticElement"))
            .and_then(|node| semantic_element_from_node(&node))
            .filter(|element| element.lifecycle != "inactive");
        if let Some(element) = element {
            collector.record_element(&element, ChangeDisposition::Upserted);
        }
    }
}

pub(super) fn prepare_delta_write(
    database: &grafeo::GrafeoDB,
    elements: &[SemanticElement],
    changed_relationship_sources: &BTreeSet<String>,
    relationships: &[SemanticRelationship],
) -> Result<PreparedDelta> {
    let element_plans = elements
        .iter()
        .map(|element| prepare_element_upsert(database, element))
        .collect::<Result<Vec<_>>>()?;
    let known_node_ids = planned_upsert_node_ids(elements.iter(), &element_plans);
    let duplicate_repair = DuplicateElementRepair::prepare(database, &element_plans);
    let relationship_plan = prepare_relationship_sync(
        database,
        changed_relationship_sources,
        relationships,
        &known_node_ids,
    )?;
    Ok(PreparedDelta {
        element_plans,
        relationship_plan,
        duplicate_repair,
    })
}

pub(super) fn apply_prepared_delta(
    graph: &GraphTransaction<'_>,
    elements: &[SemanticElement],
    commit_version: i64,
    plan: PreparedDelta,
) -> Result<BTreeMap<String, NodeId>> {
    let relationship_changes = plan.relationship_plan.changed_element_node_ids().clone();
    for (element, element_plan) in elements.iter().zip(plan.element_plans) {
        apply_element_upsert(graph, element, commit_version, element_plan)?;
    }
    apply_relationship_write(graph, plan.relationship_plan)?;
    plan.duplicate_repair.apply(graph)?;
    Ok(relationship_changes)
}

pub(super) fn planned_upsert_node_ids<'a>(
    elements: impl Iterator<Item = &'a SemanticElement>,
    plans: &[ElementUpsertPlan],
) -> BTreeMap<String, NodeId> {
    elements
        .zip(plans)
        .map(|(element, plan)| {
            (
                element.semantic_element_id.clone(),
                plan.relationship_node_id(&element.semantic_element_id),
            )
        })
        .collect()
}

pub(super) fn changed_relationship_source_ids(
    existing: &[SemanticRelationship],
    desired: &[SemanticRelationship],
) -> BTreeSet<String> {
    let existing_by_key = borrowed_relationships_by_key(existing);
    let desired_by_key = borrowed_relationships_by_key(desired);
    let mut sources: BTreeSet<String> = existing_by_key
        .keys()
        .chain(desired_by_key.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|key| existing_by_key.get(key) != desired_by_key.get(key))
        .map(|key| key.0.to_string())
        .collect();
    let mut seen = BTreeSet::new();
    for relationship in existing {
        if !seen.insert(borrowed_relationship_key(relationship)) {
            sources.insert(relationship.source_element_id.clone());
        }
    }
    sources
}

type BorrowedRelationshipKey<'a> = (&'a str, &'a str, &'a str, &'a str);

pub(super) fn borrowed_relationships_by_key(
    relationships: &[SemanticRelationship],
) -> BTreeMap<BorrowedRelationshipKey<'_>, &SemanticRelationship> {
    relationships
        .iter()
        .map(|relationship| (borrowed_relationship_key(relationship), relationship))
        .collect()
}

pub(super) fn borrowed_relationship_key(
    relationship: &SemanticRelationship,
) -> BorrowedRelationshipKey<'_> {
    (
        &relationship.source_element_id,
        &relationship.target_element_id,
        &relationship.relationship_kind,
        &relationship.label,
    )
}

pub(super) fn relationships_for_sources(
    relationships: &[SemanticRelationship],
    source_ids: &BTreeSet<String>,
) -> Vec<SemanticRelationship> {
    relationships
        .iter()
        .filter(|relationship| source_ids.contains(&relationship.source_element_id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod snapshot_rollback_tests {
    use super::*;
    use crate::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    use lumvise_resource_routing::InvocationControl;

    #[test]
    fn cancellation_after_bulk_snapshot_apply_rolls_back_native_nodes_and_attachments() {
        let persistence = LocalPersistence::in_memory().unwrap();
        let control = InvocationControl::sixty_seconds();
        let original = snapshot_element();
        persistence
            .execute(
                SemanticOperation::SyncStructure {
                    project_root: "/snapshot-rollback".into(),
                    elements: vec![original.clone()],
                    relationships: vec![],
                },
                &control,
            )
            .unwrap();
        let artifact = crate::SemanticArtifact {
            artifact_id: "note".into(),
            semantic_element_id: "owner".into(),
            artifact_kind: "note".into(),
            title: "Note".into(),
            content_ref: None,
            content: Some("Note".into()),
            searchable_text: None,
            content_size_bytes: None,
            dependencies: vec![crate::ArtifactDependency {
                target: crate::ArtifactDependencyTarget::SemanticElement {
                    semantic_element_id: "owner".into(),
                },
            }],
            metadata: serde_json::json!({}),
        };
        persistence
            .execute(
                SemanticOperation::UpsertArtifact {
                    artifact,
                    media_type: "text/plain".into(),
                },
                &control,
            )
            .unwrap();
        let storage = persistence
            .core
            .storage_manager()
            .semantic_storage()
            .with_control(control.clone());
        let before = native_attachment_state(&storage);
        let mut renamed = original.clone();
        renamed.name = "Changed".into();
        let elements = vec![renamed];
        let initial = storage
            .graph
            .plan(|database| {
                prepare_snapshot_write(database, "/snapshot-rollback", &elements, &[], &[])
            })
            .unwrap();
        let failure = storage.commit_prepared_graph_write(
            initial,
            |database| prepare_snapshot_write(database, "/snapshot-rollback", &elements, &[], &[]),
            |graph, revision, _collector, plan| {
                apply_prepared_snapshot(graph, &elements, &[], revision, plan)?;
                assert!(
                    graph.get_node(before.0).is_some(),
                    "bulk sync must retain the native owner node"
                );
                // Cancel after native writes, so the enclosing commit must roll back.
                control.cancel();
                Ok(())
            },
        );
        assert!(failure.is_err());
        assert_eq!(native_attachment_state(&storage), before);
        let observed = persistence
            .execute(
                SemanticOperation::Element {
                    semantic_element_id: "owner".into(),
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
        assert!(matches!(observed, SemanticResult::Element(Some(element)) if element == original));
    }
    fn native_attachment_state(
        storage: &SemanticStorage<'_>,
    ) -> (NodeId, Vec<(NodeId, NodeId, String)>) {
        storage.graph.read(|database| {
            let owner = database
                .find_nodes_by_property("semantic_element_id", &grafeo::Value::from("owner"))
                .into_iter()
                .find(|node_id| {
                    database
                        .get_node(*node_id)
                        .is_some_and(|node| node.has_label("SemanticElement"))
                })
                .unwrap();
            let edges = database
                .iter_edges()
                .map(|edge| (edge.src, edge.dst, edge.edge_type.to_string()))
                .collect();
            (owner, edges)
        })
    }
    fn snapshot_element() -> SemanticElement {
        SemanticElement {
            project_root: "/snapshot-rollback".into(),
            semantic_element_id: "owner".into(),
            semantic_source_id: "fixture".into(),
            path: "owner.rs".into(),
            element_kind: "file".into(),
            name: "Original".into(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".into(),
            match_evidence: None,
            metadata: serde_json::json!({}),
        }
    }
}
