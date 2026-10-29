use super::*;
use crate::index_publication::tests::established_source_store;
use lumvise_db_core::{
    DbError, LocalPersistence, PersistenceResult, PluginDataMutation, RelationalOperation,
    RelationalReadiness, RelationalResult, SemanticReadiness,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct CountedSourceReads {
    local: Arc<LocalPersistence>,
    selections: AtomicUsize,
    lineages: AtomicUsize,
    reject_selections: AtomicBool,
}

impl CountedSourceReads {
    fn new(local: Arc<LocalPersistence>) -> Self {
        Self {
            local,
            selections: AtomicUsize::new(0),
            lineages: AtomicUsize::new(0),
            reject_selections: AtomicBool::new(false),
        }
    }
}

impl SemanticPersistence for CountedSourceReads {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        if matches!(&operation, SemanticOperation::ProjectIdentity { .. }) {
            self.lineages.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(&operation, SemanticOperation::RevisionSelection { .. }) {
            self.selections.fetch_add(1, Ordering::Relaxed);
            if self.reject_selections.load(Ordering::Relaxed) {
                return Err(DbError::invalid_value(
                    "unexpected selection hydration",
                    "a saved logical source binding",
                ));
            }
        }
        SemanticPersistence::execute(&*self.local, operation, control)
    }

    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        SemanticPersistence::readiness(&*self.local)
    }
}

fn source_binding(
    semantic: &dyn SemanticPersistence,
    relational: Arc<dyn RelationalPersistence>,
    directory: &Path,
    project: &str,
) -> Result<IndexCheckoutBinding> {
    resolve(
        semantic,
        relational,
        "client-A",
        directory,
        project,
        "source-index",
        &InvocationControl::sixty_seconds(),
    )
}

#[test]
fn reopening_the_same_checkout_reuses_durable_identity_without_hydrating_membership() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let counted = CountedSourceReads::new(local.clone());
    let directory = tempfile::tempdir().unwrap();
    let first = source_binding(&counted, local.clone(), directory.path(), "/project").unwrap();
    counted.reject_selections.store(true, Ordering::Relaxed);
    for root in [directory.path().to_path_buf(), directory.path().join(".")] {
        let reopened = source_binding(&counted, local.clone(), &root, "/project").unwrap();
        assert_eq!(reopened, first);
    }
    assert_eq!(counted.selections.load(Ordering::Relaxed), 1);
    assert_eq!(counted.lineages.load(Ordering::Relaxed), 3);
}

#[test]
fn a_new_checkout_reuses_the_existing_opaque_source_identity() {
    let local = established_source_store();
    let counted = CountedSourceReads::new(local.clone());
    let directory = tempfile::tempdir().unwrap();
    let binding = source_binding(&counted, local, directory.path(), "/shared-index").unwrap();
    assert_eq!(binding.semantic_source_id, "logical-source");
    assert_eq!(counted.selections.load(Ordering::Relaxed), 1);
}

#[test]
fn source_routes_are_isolated_by_client_provider_and_project_lineage() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let counted = CountedSourceReads::new(local.clone());
    let directory = tempfile::tempdir().unwrap();
    let first = source_binding(&counted, local.clone(), directory.path(), "/project").unwrap();
    let other = source_binding(&counted, local.clone(), directory.path(), "/other").unwrap();
    assert_ne!(other.project_id, first.project_id);
    assert_ne!(other.semantic_source_id, first.semantic_source_id);
    for (client, provider) in [("client-B", "source-index"), ("client-A", "other-provider")] {
        resolve(
            &counted,
            local.clone(),
            client,
            directory.path(),
            "/project",
            provider,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    }
    assert_eq!(counted.selections.load(Ordering::Relaxed), 4);
}

#[test]
fn missing_or_file_checkout_roots_fail_before_reading_graph_membership() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let counted = CountedSourceReads::new(local.clone());
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("a.rs");
    std::fs::write(&file, "fn a() {}\n").unwrap();
    for root in [file, directory.path().join("missing")] {
        assert!(source_binding(&counted, local.clone(), &root, "/project").is_err());
    }
    assert_eq!(counted.selections.load(Ordering::Relaxed), 0);
}

#[test]
fn different_checkout_paths_keep_different_producer_identities() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let directory = tempfile::tempdir().unwrap();
    let mut bindings = Vec::new();
    for folder in ["source-a", "source-β"] {
        let root = directory.path().join(folder);
        std::fs::create_dir(&root).unwrap();
        bindings.push(source_binding(&*local, local.clone(), &root, "/project").unwrap());
    }
    assert_ne!(bindings[0].checkout_id, bindings[1].checkout_id);
    assert_eq!(
        bindings[0].semantic_source_id,
        bindings[1].semantic_source_id
    );
}

#[test]
fn a_saved_binding_with_a_different_route_fails_closed() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let directory = tempfile::tempdir().unwrap();
    let binding = source_binding(&*local, local.clone(), directory.path(), "/project").unwrap();
    let rows = CheckpointRows::source_route(local.clone(), &binding).unwrap();
    let mut forged = binding.clone();
    forged.provider_instance_id = "different-provider".into();
    rows.apply(
        vec![rows.put("binding", &forged).unwrap()],
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    let error = source_binding(&*local, local.clone(), directory.path(), "/project").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("original checkout/lineage source route")
    );
}

#[derive(Clone, Copy)]
enum BindingWriteEffect {
    LostAcknowledgement,
    ConcurrentIdentical,
    ConcurrentDifferent,
    Reject,
}

struct InterleavedBindingRows {
    local: Arc<LocalPersistence>,
    effect: BindingWriteEffect,
    armed: AtomicBool,
}

impl RelationalPersistence for InterleavedBindingRows {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        let binding_write = matches!(&operation, RelationalOperation::ApplyMutations { mutations, .. }
            if mutations.iter().any(|mutation| matches!(mutation,
                PluginDataMutation::Put { row_key, .. } if row_key.ends_with("/binding"))));
        if !binding_write || !self.armed.swap(false, Ordering::AcqRel) {
            return RelationalPersistence::execute(&*self.local, operation, control);
        }
        self.interleave(operation, control)
    }

    fn readiness(&self) -> PersistenceResult<RelationalReadiness> {
        RelationalPersistence::readiness(&*self.local)
    }
}

impl InterleavedBindingRows {
    fn interleave(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        if matches!(self.effect, BindingWriteEffect::Reject) {
            return Err(DbError::invalid_value(
                "rejected binding write",
                "a durable binding",
            ));
        }
        let mut concurrent = operation.clone();
        if matches!(self.effect, BindingWriteEffect::ConcurrentDifferent)
            && let RelationalOperation::ApplyMutations { mutations, .. } = &mut concurrent
        {
            for mutation in mutations {
                if let PluginDataMutation::Put { value, .. } = mutation {
                    value["semantic_source_id"] = serde_json::json!("a-different-source");
                }
            }
        }
        RelationalPersistence::execute(&*self.local, concurrent, control)?;
        if matches!(self.effect, BindingWriteEffect::LostAcknowledgement) {
            return Err(DbError::invalid_value(
                "lost binding acknowledgement",
                "read-back of the identical binding",
            ));
        }
        RelationalPersistence::execute(&*self.local, operation, control)
    }
}

#[test]
fn identical_concurrent_binding_and_lost_acknowledgement_are_idempotent() {
    for effect in [
        BindingWriteEffect::LostAcknowledgement,
        BindingWriteEffect::ConcurrentIdentical,
    ] {
        let local = Arc::new(LocalPersistence::in_memory().unwrap());
        let directory = tempfile::tempdir().unwrap();
        let rows = Arc::new(InterleavedBindingRows {
            local: local.clone(),
            effect,
            armed: AtomicBool::new(true),
        });
        let accepted = source_binding(&*local, rows, directory.path(), "/project").unwrap();
        let reopened =
            source_binding(&*local, local.clone(), directory.path(), "/project").unwrap();
        assert_eq!(accepted, reopened);
    }
}

#[test]
fn a_rejected_or_conflicting_binding_write_never_returns_a_different_source() {
    for effect in [
        BindingWriteEffect::Reject,
        BindingWriteEffect::ConcurrentDifferent,
    ] {
        let local = Arc::new(LocalPersistence::in_memory().unwrap());
        let directory = tempfile::tempdir().unwrap();
        let rows = Arc::new(InterleavedBindingRows {
            local: local.clone(),
            effect,
            armed: AtomicBool::new(true),
        });
        assert!(source_binding(&*local, rows, directory.path(), "/project").is_err());
    }
}
