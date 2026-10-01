use crate::local::grafeo::change_hooks::DirtyElement;
use crate::local::grafeo::graph_rows::ChangeCollector;
use crate::local::grafeo::graph_store::{
    ConditionalGraphWrite, GraphPostCommit, GraphTransaction, VersionedGraphPlan,
};
use crate::local::grafeo::semantic_storage::SemanticStorage;
use crate::local::sql::commits::{
    begin_semantic_commit, fail_semantic_commit, publish_semantic_commit,
};
use crate::{DbError, Result};
use grafeo::GrafeoDB;
use rusqlite::Transaction;
use std::cell::RefCell;
use std::sync::atomic::Ordering;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static GRAPH_PUBLICATION_SQL_COMMITS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_graph_publication_sql_commits() {
    GRAPH_PUBLICATION_SQL_COMMITS.with(|count| count.set(0));
}

#[cfg(test)]
fn graph_publication_sql_commits() -> usize {
    GRAPH_PUBLICATION_SQL_COMMITS.with(Cell::get)
}

impl<'db> SemanticStorage<'db> {
    #[cfg(test)]
    pub(crate) fn commit_graph_write<R>(
        &self,
        write: impl FnOnce(&GraphTransaction<'_>, i64, &mut ChangeCollector) -> Result<R>,
    ) -> Result<R> {
        let mut conn = self.conn.write_conn();
        let transaction = conn.transaction()?;
        let commit_version = begin_semantic_commit(&transaction)?;
        let mut transaction = Some(transaction);
        let mut collector = ChangeCollector::new(commit_version);
        let result = self.graph.write_semantic_through_post_commit(
            |graph| {
                let value = write(graph, commit_version, &mut collector)?;
                let dirty_elements = collector.into_parts();
                Ok((value, dirty_elements))
            },
            |(value, dirty_elements)| {
                let transaction = transaction
                    .take()
                    .expect("publication transaction remains open until Grafeo commits");
                publish_graph_commit_sql(transaction, commit_version)?;
                Ok((value, dirty_elements))
            },
            |(_, dirty_elements)| dirty_elements.len(),
        );
        match result {
            Ok(GraphPostCommit::Completed((value, dirty_elements))) => {
                self.notify_graph_commit(commit_version, dirty_elements);
                Ok(value)
            }
            Ok(GraphPostCommit::Failed(error)) => Err(error),
            Err(error) => {
                if let Some(transaction) = transaction {
                    fail_semantic_commit(&transaction, commit_version, &error.to_string())?;
                    transaction.commit()?;
                }
                Err(error)
            }
        }
    }

    /// Runs graph-dependent preparation outside the Grafeo writer gate, then
    /// applies only the prepared IDs while holding it. A conflicting graph
    /// write rolls back its uncommitted SQLite begin row.
    pub(crate) fn commit_preplanned_graph_write<P, R>(
        &self,
        prepare: impl Fn(&GrafeoDB) -> Result<P>,
        apply: impl Fn(&GraphTransaction<'_>, i64, &mut ChangeCollector, P) -> Result<R>,
    ) -> Result<R> {
        self.check_active()?;
        let initial = self.graph.plan(&prepare)?;
        self.commit_prepared_graph_write(initial, prepare, apply)
    }

    /// Reuses a prepared graph diff while its revision remains current.
    /// For example, structure synchronization passes its no-op inspection plan.
    pub(crate) fn commit_prepared_graph_write<P, R>(
        &self,
        initial: VersionedGraphPlan<P>,
        prepare: impl Fn(&GrafeoDB) -> Result<P>,
        apply: impl Fn(&GraphTransaction<'_>, i64, &mut ChangeCollector, P) -> Result<R>,
    ) -> Result<R> {
        const MAX_STALE_PLAN_RETRIES: usize = 8;
        let mut initial = Some(initial);

        for _ in 0..MAX_STALE_PLAN_RETRIES {
            self.check_active()?;
            let plan = match initial.take() {
                Some(plan) => plan,
                None => self.graph.plan(&prepare)?,
            };
            self.check_active()?;
            let (revision, plan) = plan.into_parts();
            let mut conn = self.conn.write_conn();
            let publication = RefCell::new(None);
            let outcome = self.graph.write_if_revision_semantic_through_post_commit(
                revision,
                |graph| {
                    self.check_active()?;
                    let transaction = conn.transaction()?;
                    let commit_version = begin_semantic_commit(&transaction)?;
                    publication.replace(Some((transaction, commit_version)));
                    let mut collector = ChangeCollector::new(commit_version);
                    let value = apply(graph, commit_version, &mut collector, plan)?;
                    self.check_active()?;
                    let dirty_elements = collector.into_parts();
                    Ok((value, commit_version, dirty_elements))
                },
                |(value, commit_version, dirty_elements)| {
                    let (transaction, _) = publication
                        .borrow_mut()
                        .take()
                        .expect("publication transaction remains open until Grafeo commits");
                    publish_graph_commit_sql(transaction, commit_version)?;
                    Ok((value, commit_version, dirty_elements))
                },
                |(_, _, dirty_elements)| dirty_elements.len(),
            );

            match outcome {
                Ok(ConditionalGraphWrite::Stale) => continue,
                Ok(ConditionalGraphWrite::Applied(GraphPostCommit::Completed((
                    value,
                    commit_version,
                    dirty_elements,
                )))) => {
                    self.notify_graph_commit(commit_version, dirty_elements);
                    return Ok(value);
                }
                Ok(ConditionalGraphWrite::Applied(GraphPostCommit::Failed(error))) => {
                    return Err(error);
                }
                Err(error) => {
                    if let Some((transaction, commit_version)) = publication.into_inner() {
                        fail_semantic_commit(&transaction, commit_version, &error.to_string())?;
                        transaction.commit()?;
                    }
                    return Err(error);
                }
            }
        }
        Err(DbError::Grafeo(
            "semantic graph write remained stale after bounded retries".to_string(),
        ))
    }

    fn notify_graph_commit(&self, commit_version: i64, dirty_elements: Vec<DirtyElement>) {
        self.change_buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .publish(commit_version, dirty_elements);
        self.storage_change_generation
            .fetch_add(1, Ordering::Release);
        let generation = self.storage_change_generation.load(Ordering::Acquire);
        let (notified_generation, notifier) = self.storage_change_signal.as_ref();
        if let Ok(mut seen) = notified_generation.lock() {
            *seen = generation;
            notifier.notify_all();
        }
        let _ = self.storage_change_sender.send(generation);
    }
}

fn publish_graph_commit_sql(transaction: Transaction<'_>, commit_version: i64) -> Result<()> {
    publish_semantic_commit(&transaction, commit_version)?;
    transaction.commit()?;
    #[cfg(test)]
    GRAPH_PUBLICATION_SQL_COMMITS.with(|count| count.set(count.get() + 1));
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::local::runtime::DbCore;
    use crate::{SemanticElement, SemanticRelationship};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn migrated_element_upsert_has_constant_gate_scan_work() {
        let small = gate_scan_work_after_upsert(0);
        let large = gate_scan_work_after_upsert(64);
        assert_eq!(small, large);
        assert_eq!(small.find_nodes_by_property, 0);
    }

    #[test]
    fn migrated_relationship_snapshot_and_lifecycle_writes_have_no_gate_scans() {
        for unrelated_count in [0, 64] {
            assert_no_gate_scans(gate_scan_work_after_relationship(unrelated_count));
            assert_no_gate_scans(gate_scan_work_after_snapshot(unrelated_count));
            assert_no_gate_scans(gate_scan_work_after_lifecycle_delete(unrelated_count));
        }
    }

    fn assert_no_gate_scans(work: super::super::graph_store::GraphGateWork) {
        assert_eq!(work.find_nodes_by_property, 0);
    }

    #[test]
    fn stale_relationship_plan_retries_before_direct_id_apply() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage
            .upsert_element(&element("source", "source"))
            .unwrap();
        storage
            .upsert_element(&element("target", "target"))
            .unwrap();
        let relationship = relationship("source", "target");
        let plans = AtomicUsize::new(0);

        storage
            .commit_preplanned_graph_write(
                |database| {
                    let plan = crate::local::grafeo::graph_rows::prepare_relationship_upserts(
                        database,
                        std::slice::from_ref(&relationship),
                        &std::collections::BTreeMap::new(),
                    )?;
                    if plans.fetch_add(1, Ordering::SeqCst) == 0 {
                        storage.graph.write(|graph| {
                            graph.create_node_with_props(&["Intervening"], [])?;
                            Ok(())
                        })?;
                    }
                    Ok(plan)
                },
                |graph, _commit_version, _collector, plan| {
                    crate::local::grafeo::graph_rows::apply_relationship_write(graph, plan)
                },
            )
            .unwrap();

        assert_eq!(plans.load(Ordering::SeqCst), 2);
        assert_eq!(
            storage.relationships_from("source").unwrap(),
            vec![relationship]
        );
    }

    #[test]
    fn prepared_write_reuses_current_plan_and_replans_stale_revision() {
        for invalidate in [false, true] {
            let db = DbCore::in_memory().unwrap();
            let storage = db.storage_manager().semantic_storage();
            let preparations = AtomicUsize::new(0);
            let prepare =
                |_database: &grafeo::GrafeoDB| Ok(preparations.fetch_add(1, Ordering::SeqCst));
            let initial = storage.graph.plan(prepare).unwrap();
            if invalidate {
                storage
                    .upsert_element(&element("intervening", "intervening"))
                    .unwrap();
            }
            let applied = storage
                .commit_prepared_graph_write(
                    initial,
                    prepare,
                    |_graph, _revision, _collector, plan| Ok(plan),
                )
                .unwrap();
            assert_eq!(applied, usize::from(invalidate));
            assert_eq!(
                preparations.load(Ordering::SeqCst),
                1 + usize::from(invalidate)
            );
        }
    }

    #[test]
    fn graph_publication_uses_one_sql_commit() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        super::reset_graph_publication_sql_commits();

        storage
            .upsert_element(&element("one-commit", "one-commit"))
            .unwrap();

        assert_eq!(super::graph_publication_sql_commits(), 1);
    }

    #[test]
    fn stale_preplanning_attempts_commit_no_sql_transaction() {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        super::reset_graph_publication_sql_commits();

        let error = storage
            .commit_preplanned_graph_write(
                |_database| {
                    storage.graph.write(|graph| {
                        graph.create_node_with_props(&["AlwaysIntervening"], [])?;
                        Ok(())
                    })?;
                    Ok(())
                },
                |_graph, _commit_version, _collector, ()| Ok(()),
            )
            .unwrap_err();

        assert!(error.to_string().contains("remained stale"));
        assert_eq!(super::graph_publication_sql_commits(), 0);
    }

    fn gate_scan_work_after_relationship(
        unrelated_count: usize,
    ) -> super::super::graph_store::GraphGateWork {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage
            .upsert_element(&element("source", "source"))
            .unwrap();
        storage
            .upsert_element(&element("target", "target"))
            .unwrap();
        seed_unrelated(&storage, unrelated_count);
        storage.graph.reset_gate_work();
        storage
            .link_elements(&relationship("source", "target"))
            .unwrap();
        storage.graph.gate_work()
    }

    fn gate_scan_work_after_snapshot(
        unrelated_count: usize,
    ) -> super::super::graph_store::GraphGateWork {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        seed_unrelated(&storage, unrelated_count);
        let source = element_in("/snapshot", "snapshot-source", "source");
        let target = element_in("/snapshot", "snapshot-target", "target");
        storage.graph.reset_gate_work();
        storage
            .sync_semantic_structure(
                "/snapshot",
                &[source, target],
                &[relationship_in(
                    "/snapshot",
                    "snapshot-source",
                    "snapshot-target",
                )],
            )
            .unwrap();
        storage.graph.gate_work()
    }

    fn gate_scan_work_after_lifecycle_delete(
        unrelated_count: usize,
    ) -> super::super::graph_store::GraphGateWork {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage
            .upsert_element(&element("deleted", "deleted"))
            .unwrap();
        seed_unrelated(&storage, unrelated_count);
        storage.graph.reset_gate_work();
        assert!(storage.remove_element("deleted").unwrap());
        storage.graph.gate_work()
    }

    fn seed_unrelated(
        storage: &crate::local::grafeo::semantic_storage::SemanticStorage<'_>,
        unrelated_count: usize,
    ) {
        for index in 0..unrelated_count {
            storage
                .upsert_element(&element(&format!("unrelated-{index}"), "unrelated"))
                .unwrap();
        }
    }

    fn relationship(source_element_id: &str, target_element_id: &str) -> SemanticRelationship {
        relationship_in("/repo", source_element_id, target_element_id)
    }

    fn relationship_in(
        project_root: &str,
        source_element_id: &str,
        target_element_id: &str,
    ) -> SemanticRelationship {
        SemanticRelationship {
            project_root: project_root.to_string(),
            source_element_id: source_element_id.to_string(),
            target_element_id: target_element_id.to_string(),
            relationship_kind: "contains".to_string(),
            label: "contains".to_string(),
            metadata: json!({}),
        }
    }

    fn gate_scan_work_after_upsert(
        unrelated_count: usize,
    ) -> super::super::graph_store::GraphGateWork {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage
            .upsert_element(&element("target", "before"))
            .unwrap();
        for index in 0..unrelated_count {
            storage
                .upsert_element(&element(&format!("unrelated-{index}"), "unrelated"))
                .unwrap();
        }
        storage.graph.reset_gate_work();
        storage.upsert_element(&element("target", "after")).unwrap();
        storage.graph.gate_work()
    }

    #[test]
    fn cancelled_preparation_never_applies_or_publishes() {
        let db = DbCore::in_memory().unwrap();
        let control = lumvise_resource_routing::InvocationControl::sixty_seconds();
        let storage = db
            .storage_manager()
            .semantic_storage()
            .with_control(control.clone());
        super::reset_graph_publication_sql_commits();
        let failure = storage
            .commit_preplanned_graph_write(
                |_graph| {
                    control.cancel();
                    Ok(())
                },
                |_graph, _revision, _changes, ()| -> crate::Result<()> {
                    panic!("cancelled plan must not apply")
                },
            )
            .unwrap_err();
        assert!(failure.to_string().contains("cancelled invocation"));
        assert_eq!(super::graph_publication_sql_commits(), 0);
    }

    #[test]
    fn cancellation_during_apply_rolls_back_before_publication() {
        let db = DbCore::in_memory().unwrap();
        let control = lumvise_resource_routing::InvocationControl::sixty_seconds();
        let storage = db
            .storage_manager()
            .semantic_storage()
            .with_control(control.clone());
        let failure = storage
            .commit_preplanned_graph_write(
                |_graph| Ok(()),
                |graph, _revision, _changes, ()| {
                    graph.create_node_with_props(&["CancelledWrite"], [])?;
                    control.cancel();
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(failure.to_string().contains("cancelled invocation"));
        assert!(storage.graph.read(|graph| {
            graph
                .graph_store()
                .nodes_by_label("CancelledWrite")
                .into_iter()
                .all(|id| graph.get_node(id).is_none())
        }));
    }

    fn element(id: &str, name: &str) -> SemanticElement {
        element_in("/repo", id, name)
    }

    fn element_in(project_root: &str, id: &str, name: &str) -> SemanticElement {
        SemanticElement {
            project_root: project_root.to_string(),
            semantic_element_id: id.to_string(),
            semantic_source_id: "source".to_string(),
            path: format!("src/{name}.rs"),
            element_kind: "function".to_string(),
            name: name.to_string(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".to_string(),
            match_evidence: None,
            metadata: json!({}),
        }
    }
}
