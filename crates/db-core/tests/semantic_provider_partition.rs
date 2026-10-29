//! Producer publication through the public Local and Centralized entrypoints.
use lumvise_db_core::{
    CentralizedPersistence, LocalPersistence, PersistenceResult, SemanticElement,
    SemanticOperation, SemanticPartition, SemanticPersistence, SemanticReadiness,
    SemanticRelationship, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::sync::Arc;

mod common;
use common::LocalPublicationServer;

struct ProviderPublication {
    local: Arc<LocalPersistence>,
    remote: Option<CentralizedPersistence>,
}

impl ProviderPublication {
    fn new(centralized: bool) -> Self {
        Self::with_local(
            Arc::new(LocalPersistence::in_memory().unwrap()),
            centralized,
        )
    }

    fn with_local(local: Arc<LocalPersistence>, centralized: bool) -> Self {
        let remote = centralized.then(|| {
            CentralizedPersistence::new(
                Arc::new(LocalPublicationServer(local.clone())),
                "producer-test",
            )
        });
        Self { local, remote }
    }

    fn apply(&self, operation: SemanticOperation) -> SemanticResult {
        self.execute(operation, &InvocationControl::sixty_seconds())
            .unwrap()
    }

    fn publish(&self, provider: &str, paths: &[&str], elements: Vec<SemanticElement>) {
        self.apply(partition(provider, paths, elements));
    }

    fn record(&self, id: &str) -> Option<SemanticElement> {
        let SemanticResult::Element(record) = self.apply(SemanticOperation::Element {
            semantic_element_id: id.into(),
        }) else {
            panic!("expected element")
        };
        record
    }

    fn begin(&self) {
        self.apply(SemanticOperation::BeginProjectSnapshot {
            snapshot_id: "refresh".into(),
            project_root: "/repo".into(),
            provider_instance_id: Some("source".into()),
            partition_paths: vec![],
            page_count: 1,
        });
        self.apply(SemanticOperation::StageProjectSnapshot {
            snapshot_id: "refresh".into(),
            page_index: 0,
            elements: vec![record("source", "source", 2)],
            relationships: vec![],
        });
    }
}

impl SemanticPersistence for ProviderPublication {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        match &self.remote {
            Some(remote) => remote.execute(operation, control),
            None => self.local.execute(operation, control),
        }
    }
    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        self.local.readiness()
    }
}

fn record(provider: &str, id: &str, revision: u64) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: provider.into(),
        path: "shared.rs".into(),
        element_kind: "file".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"provider_instance_id": provider, "revision": revision}),
    }
}

fn partition(provider: &str, paths: &[&str], elements: Vec<SemanticElement>) -> SemanticOperation {
    SemanticOperation::SyncPartition {
        partition: SemanticPartition {
            project_root: "/repo".into(),
            provider_instance_id: Some(provider.into()),
            replace_paths: paths.iter().map(|path| (*path).into()).collect(),
        },
        elements,
        relationships: vec![],
    }
}

#[test]
fn staged_refresh_preserves_newer_foreign_records_and_tombstones_on_a_shared_path() {
    for remote in [false, true] {
        let db = ProviderPublication::new(remote);
        db.publish("documents", &[], vec![record("documents", "doc", 1)]);
        db.publish("source", &[], vec![record("source", "old-source", 1)]);
        db.begin();
        db.publish(
            "documents",
            &[],
            vec![
                record("documents", "doc", 2),
                record("documents", "new-doc", 1),
            ],
        );
        db.publish(
            "documents",
            &["shared.rs"],
            vec![record("documents", "new-doc", 2)],
        );
        db.apply(SemanticOperation::CommitProjectSnapshot {
            snapshot_id: "refresh".into(),
        });
        assert!(
            db.record("doc").is_none(),
            "foreign deletion must not be revived"
        );
        assert_eq!(db.record("new-doc").unwrap().metadata["revision"], 2);
        assert_eq!(db.record("source").unwrap().metadata["revision"], 2);
        assert!(db.record("old-source").is_none());
    }
}

#[test]
fn shared_path_replacement_keeps_foreign_source_edges_and_cross_provider_references() {
    for remote in [false, true] {
        let db = ProviderPublication::new(remote);
        db.publish("documents", &[], vec![record("documents", "doc", 1)]);
        db.publish("source", &[], vec![record("source", "source", 1)]);
        for (provider, source, target) in
            [("documents", "doc", "source"), ("source", "source", "doc")]
        {
            let mut operation = partition(provider, &[], vec![record(provider, source, 2)]);
            if let SemanticOperation::SyncPartition { relationships, .. } = &mut operation {
                relationships.push(SemanticRelationship {
                    project_root: "/repo".into(),
                    source_element_id: source.into(),
                    target_element_id: target.into(),
                    relationship_kind: "references".into(),
                    label: "references".into(),
                    metadata: json!({}),
                });
            }
            db.apply(operation);
        }
        db.publish(
            "source",
            &["shared.rs"],
            vec![record("source", "source", 3)],
        );
        let SemanticResult::Relationships(edges) = db.apply(SemanticOperation::RelationshipsFrom {
            semantic_element_id: "doc".into(),
        }) else {
            panic!("expected relationships")
        };
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_element_id, "source");
        assert_eq!(db.record("doc").unwrap().metadata["revision"], 2);
    }
}

#[test]
fn provider_scope_rejects_foreign_input_and_identity_takeover_without_partial_writes() {
    for remote in [false, true] {
        let db = ProviderPublication::new(remote);
        db.publish("documents", &[], vec![record("documents", "doc", 1)]);
        for invalid in [
            record("documents", "wrong-owner", 1),
            record("source", "doc", 99),
        ] {
            let error = db
                .execute(
                    partition("source", &[], vec![record("source", "valid", 1), invalid]),
                    &InvocationControl::sixty_seconds(),
                )
                .unwrap_err();
            assert!(error.to_string().contains("provider"));
            assert!(db.record("valid").is_none());
            assert_eq!(db.record("doc").unwrap().metadata["revision"], 1);
        }
        assert!(
            db.execute(
                partition(" ", &[], vec![]),
                &InvocationControl::sixty_seconds()
            )
            .is_err()
        );
    }
}

#[test]
fn scoped_commit_can_retry_after_cancellation_and_survives_reopen() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("portable.db");
    {
        let db =
            ProviderPublication::with_local(Arc::new(LocalPersistence::open(&path).unwrap()), true);
        db.publish("documents", &[], vec![record("documents", "doc", 1)]);
        db.begin();
        db.publish("documents", &[], vec![record("documents", "doc", 2)]);
        let control = InvocationControl::sixty_seconds();
        control.cancel();
        assert!(
            db.execute(
                SemanticOperation::CommitProjectSnapshot {
                    snapshot_id: "refresh".into()
                },
                &control
            )
            .is_err()
        );
        db.apply(SemanticOperation::CommitProjectSnapshot {
            snapshot_id: "refresh".into(),
        });
    }
    let db =
        ProviderPublication::with_local(Arc::new(LocalPersistence::open(&path).unwrap()), false);
    assert_eq!(db.record("doc").unwrap().metadata["revision"], 2);
    assert_eq!(db.record("source").unwrap().metadata["revision"], 2);
}

#[test]
fn failed_scoped_publication_keeps_the_staged_page_and_foreign_record_for_retry() {
    for remote in [false, true] {
        let db = ProviderPublication::new(remote);
        db.publish("documents", &[], vec![record("documents", "doc", 1)]);
        db.begin();
        db.apply(SemanticOperation::StageProjectSnapshot {
            snapshot_id: "refresh".into(),
            page_index: 0,
            elements: vec![record("source", "doc", 99)],
            relationships: vec![],
        });
        assert!(
            db.execute(
                SemanticOperation::CommitProjectSnapshot {
                    snapshot_id: "refresh".into(),
                },
                &InvocationControl::sixty_seconds()
            )
            .is_err()
        );
        assert_eq!(db.record("doc").unwrap().metadata["revision"], 1);
        db.apply(SemanticOperation::StageProjectSnapshot {
            snapshot_id: "refresh".into(),
            page_index: 0,
            elements: vec![record("source", "source", 2)],
            relationships: vec![],
        });
        db.apply(SemanticOperation::CommitProjectSnapshot {
            snapshot_id: "refresh".into(),
        });
        assert_eq!(db.record("source").unwrap().metadata["revision"], 2);
        assert_eq!(db.record("doc").unwrap().metadata["revision"], 1);
    }
}

#[test]
fn provider_cannot_take_an_inactive_or_other_project_identity() {
    let db = ProviderPublication::new(false);
    db.publish(
        "documents",
        &[],
        vec![record("documents", "inactive-doc", 1)],
    );
    db.publish("documents", &[], vec![]);
    let mut other = record("source", "other-project", 1);
    other.project_root = "/other".into();
    db.apply(SemanticOperation::SyncStructure {
        project_root: "/other".into(),
        elements: vec![other],
        relationships: vec![],
    });
    for id in ["inactive-doc", "other-project"] {
        assert!(
            db.execute(
                partition("source", &[], vec![record("source", id, 2)]),
                &InvocationControl::sixty_seconds()
            )
            .is_err(),
            "identity {id}"
        );
    }
}
