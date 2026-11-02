use super::*;
use crate::index_publication::IndexSourceCheckout;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
enum InspectionFault {
    None,
    Error,
    Missing(SemanticRevisionRecordHeader),
    WrongProject,
    WrongOperation,
}

struct RecoveryPersistence {
    local: Arc<LocalPersistence>,
    unavailable: Mutex<Vec<SemanticRevisionRecordHeader>>,
    inspection_fault: Mutex<InspectionFault>,
}

impl RecoveryPersistence {
    fn new(local: Arc<LocalPersistence>) -> Self {
        Self {
            local,
            unavailable: Mutex::new(Vec::new()),
            inspection_fault: Mutex::new(InspectionFault::None),
        }
    }

    fn set_unavailable(&self, headers: Vec<SemanticRevisionRecordHeader>) {
        *self.unavailable.lock().unwrap() = headers;
    }

    fn set_inspection_fault(&self, fault: InspectionFault) {
        *self.inspection_fault.lock().unwrap() = fault;
    }
}

impl SemanticPersistence for RecoveryPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        match operation {
            SemanticOperation::UnavailableRevisions { .. } => Ok(
                SemanticResult::UnavailableRevisions(self.unavailable.lock().unwrap().clone()),
            ),
            operation @ SemanticOperation::InspectRevisionIntegrity { .. } => {
                self.inspect(operation, control)
            }
            operation => SemanticPersistence::execute(&*self.local, operation, control),
        }
    }

    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        SemanticPersistence::readiness(&*self.local)
    }
}

impl RecoveryPersistence {
    fn inspect(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        let fault = self.inspection_fault.lock().unwrap().clone();
        if matches!(&fault, InspectionFault::Error) {
            return Err(DbError::invalid_value(
                "inspection unavailable",
                "an integrity report",
            ));
        }
        let result = SemanticPersistence::execute(&*self.local, operation, control)?;
        add_unacknowledged_missing_body(result, fault)
    }
}

fn add_unacknowledged_missing_body(
    result: SemanticResult,
    fault: InspectionFault,
) -> PersistenceResult<SemanticResult> {
    match (result, fault) {
        (SemanticResult::RevisionIntegrity(mut report), InspectionFault::Missing(header)) => {
            report.missing_records.push(header);
            Ok(SemanticResult::RevisionIntegrity(report))
        }
        (SemanticResult::RevisionIntegrity(mut report), InspectionFault::WrongProject) => {
            report.project_id = "another-project".into();
            Ok(SemanticResult::RevisionIntegrity(report))
        }
        (SemanticResult::RevisionIntegrity(mut report), InspectionFault::WrongOperation) => {
            report
                .publication_inspection
                .as_mut()
                .unwrap()
                .operation_id
                .push_str("-mismatch");
            Ok(SemanticResult::RevisionIntegrity(report))
        }
        (result, _) => Ok(result),
    }
}

struct SupersessionFixture {
    store: CheckoutFixture,
    root: tempfile::TempDir,
    binding: IndexCheckoutBinding,
    journal: IndexPublicationJournal,
    publication: FrozenIndexPublication,
    accepted_commit: i64,
}

impl SupersessionFixture {
    fn with_pending(operation_id: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store =
            CheckoutFixture::from_local(LocalPersistence::open(root.path().join("store")).unwrap());
        let binding = source_binding(&store, root.path());
        let journal = journal(&store, &binding);
        journal.attach(&initial_inventory(), &control()).unwrap();
        let first = store.publication(
            &journal,
            "initial",
            &[("directory", ""), ("a", "A1"), ("b", "B1")],
            true,
        );
        journal.freeze(&first, &control()).unwrap();
        let receipt = accepted(store.publish(&journal).unwrap());
        journal.acknowledge(&receipt, &control()).unwrap();
        let publication = store.publication(&journal, operation_id, &[("a", "A2")], false);
        journal.freeze(&publication, &control()).unwrap();
        assert_eq!(
            journal.pending(&control()).unwrap(),
            Some(publication.clone())
        );
        Self {
            store,
            root,
            binding,
            journal,
            publication,
            accepted_commit: receipt.commit_version,
        }
    }

    fn source(&self, semantic: Arc<dyn SemanticPersistence>) -> IndexSourceCheckout {
        IndexSourceCheckout::open(
            semantic,
            self.store.local.clone(),
            "A",
            self.root.path(),
            ROOT,
            PROVIDER,
            &control(),
        )
        .unwrap()
    }

    fn reopened_journal(&self) -> IndexPublicationJournal {
        journal(&self.store, &self.binding)
    }

    fn external_headers(&self, recovery_id: &str) -> Vec<SemanticRevisionRecordHeader> {
        let directory = self.publication.batch.semantic_elements[0]
            .parent_element_id
            .clone()
            .unwrap();
        let b = self.external_b_reference();
        vec![
            unavailable_header(&directory, "src", PROVIDER, recovery_id),
            unavailable_header(&b, "src/b.rs", PROVIDER, recovery_id),
        ]
    }

    fn external_b_reference(&self) -> String {
        self.publication
            .batch
            .semantic_relationships
            .iter()
            .find(|edge| edge.relationship_label == "calls")
            .and_then(|edge| edge.target_element_id.clone())
            .unwrap()
    }
}

fn source_binding(store: &CheckoutFixture, root: &Path) -> IndexCheckoutBinding {
    let canonical = root.canonicalize().unwrap();
    IndexCheckoutBinding {
        client_instance_id: "A".into(),
        checkout_id: format!(
            "{:x}",
            Sha256::digest(canonical.as_os_str().as_encoded_bytes())
        ),
        project_id: store.project_id.clone(),
        provider_instance_id: PROVIDER.into(),
        semantic_source_id: "logical-source".into(),
        selection_id: "working".into(),
        source_context: "local".into(),
    }
}

fn journal(store: &CheckoutFixture, binding: &IndexCheckoutBinding) -> IndexPublicationJournal {
    IndexPublicationJournal::new(
        store.local.clone(),
        store.local.clone(),
        binding.clone(),
        ROOT.into(),
    )
    .unwrap()
}

fn unavailable_header(
    record_id: &str,
    path: &str,
    provider: &str,
    recovery_id: &str,
) -> SemanticRevisionRecordHeader {
    SemanticRevisionRecordHeader {
        record: SemanticRecordRef::element(record_id),
        element_scope: Some(SemanticRevisionElementScope {
            path: path.into(),
            provider_instance_id: Some(provider.into()),
        }),
        canonical_lifecycle: Some("active".into()),
        artifact_state: None,
        created_at_ms: 1,
        unselected_at_ms: None,
        pruned_at_ms: None,
        unavailable: Some(SemanticRevisionUnavailable {
            recovery_operation_id: recovery_id.into(),
            unavailable_at_ms: 700,
            canonical_lifecycle: Some("active".into()),
        }),
    }
}

fn missing_header(record_id: &str, path: &str, provider: &str) -> SemanticRevisionRecordHeader {
    let mut header = unavailable_header(record_id, path, provider, "not-yet-recovered");
    header.unavailable = None;
    header
}

#[test]
fn source_pending_supersedes_external_recovery_and_preserves_audited_frozen_pages() {
    let fixture = SupersessionFixture::with_pending("unaccepted-source-op");
    let recovery_headers = fixture.external_headers("approved-recovery");
    let semantic = Arc::new(RecoveryPersistence::new(fixture.store.local.clone()));
    semantic.set_unavailable(recovery_headers.clone());
    let checkout = fixture.source(semantic);
    let header_before = fixture.journal.header(&control()).unwrap();
    let inventory_before = fixture.journal.inventory(&control()).unwrap();
    let pages_before = fixture
        .journal
        .rows
        .scan::<serde_json::Value>("pending/", &control())
        .unwrap();
    let digest = header_before.pending.as_ref().unwrap().digest.clone();

    assert!(checkout.pending(&control()).unwrap().is_none());

    assert_superseded_checkpoint(&fixture, &header_before, &inventory_before, &pages_before);
    assert_supersession_audit(&fixture, &recovery_headers, &digest);
    assert_frozen_pages(&fixture.journal, &fixture.publication, &digest);
    assert_restart_retry_is_read_only(fixture, checkout, &recovery_headers, &pages_before);
}

fn assert_superseded_checkpoint(
    fixture: &SupersessionFixture,
    before: &CheckpointHeader,
    inventory: &[IndexPathDigest],
    pages: &[serde_json::Value],
) {
    let after = fixture.journal.header(&control()).unwrap();
    assert_eq!(after.pending, None);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.producer_generation, before.producer_generation);
    assert_eq!(
        after.last_acknowledged_operation,
        before.last_acknowledged_operation
    );
    assert_eq!(fixture.journal.inventory(&control()).unwrap(), inventory);
    assert_eq!(
        fixture
            .journal
            .rows
            .scan::<serde_json::Value>("pending/", &control())
            .unwrap(),
        pages
    );
}

fn assert_supersession_audit(
    fixture: &SupersessionFixture,
    headers: &[SemanticRevisionRecordHeader],
    digest: &str,
) {
    let audit: RecoveredSourceSupersession = fixture
        .journal
        .rows
        .get(
            &format!("superseded/{}", fixture.publication.operation_id),
            &control(),
        )
        .unwrap()
        .unwrap();
    let mut expected_headers = headers.to_vec();
    expected_headers.sort_by(|left, right| left.record.cmp(&right.record));
    assert_eq!(audit.blocked_headers, expected_headers);
    assert_eq!(audit.operation_id, fixture.publication.operation_id);
    assert_eq!(audit.frozen_digest, digest);
    assert_eq!(audit.inspected_commit_version, fixture.accepted_commit);
}

fn assert_frozen_pages(
    journal: &IndexPublicationJournal,
    publication: &FrozenIndexPublication,
    digest: &str,
) {
    let descriptor: FrozenDescriptor = journal
        .rows
        .get(&storage::pending_key(digest, "descriptor"), &control())
        .unwrap()
        .unwrap();
    assert_eq!(
        descriptor.publication.operation_id,
        publication.operation_id
    );
    assert_eq!(descriptor.element_pages, 1);
    assert_eq!(descriptor.relationship_pages, 1);
    let elements: Vec<SemanticElementUpsert> = journal
        .rows
        .get(
            &storage::pending_key(digest, "elements/0000000000000000"),
            &control(),
        )
        .unwrap()
        .unwrap();
    let relationships: Vec<SemanticRelationshipUpsert> = journal
        .rows
        .get(
            &storage::pending_key(digest, "relationships/0000000000000000"),
            &control(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(elements, publication.batch.semantic_elements);
    assert_eq!(relationships, publication.batch.semantic_relationships);
}

fn assert_restart_retry_is_read_only(
    fixture: SupersessionFixture,
    checkout: IndexSourceCheckout,
    headers: &[SemanticRevisionRecordHeader],
    pages: &[serde_json::Value],
) {
    let database = fixture.root.path().join("store");
    let binding = fixture.binding.clone();
    let publication = fixture.publication.clone();
    let header_before = fixture.journal.header(&control()).unwrap();
    let inventory_before = fixture.journal.inventory(&control()).unwrap();
    let audit_before: RecoveredSourceSupersession = fixture
        .journal
        .rows
        .get(
            &format!("superseded/{}", publication.operation_id),
            &control(),
        )
        .unwrap()
        .unwrap();
    drop(checkout);
    let SupersessionFixture {
        store,
        root,
        journal: previous_journal,
        ..
    } = fixture;
    drop(previous_journal);
    drop(store);

    let reopened_store = CheckoutFixture::from_local(LocalPersistence::open(database).unwrap());
    let reopened = journal(&reopened_store, &binding);
    assert_eq!(reopened.header(&control()).unwrap(), header_before);
    assert_eq!(reopened.inventory(&control()).unwrap(), inventory_before);
    assert_eq!(
        reopened
            .rows
            .get::<RecoveredSourceSupersession>(
                &format!("superseded/{}", publication.operation_id),
                &control()
            )
            .unwrap(),
        Some(audit_before.clone())
    );
    assert_eq!(
        reopened
            .rows
            .scan::<serde_json::Value>("pending/", &control())
            .unwrap(),
        pages
    );
    assert!(
        reopened
            .supersede_recovered_pending(&publication, headers, &control())
            .unwrap()
    );
    assert!(reopened.pending(&control()).unwrap().is_none());
    assert_eq!(reopened.header(&control()).unwrap(), header_before);
    assert_eq!(reopened.inventory(&control()).unwrap(), inventory_before);
    assert_eq!(
        reopened
            .rows
            .get::<RecoveredSourceSupersession>(
                &format!("superseded/{}", publication.operation_id),
                &control()
            )
            .unwrap(),
        Some(audit_before)
    );
    assert_eq!(
        reopened
            .rows
            .scan::<serde_json::Value>("pending/", &control())
            .unwrap(),
        pages
    );
    let rejected = reopened.freeze(&publication, &control()).unwrap_err();
    assert!(rejected.to_string().contains("audited source supersession"));
    assert_eq!(reopened.header(&control()).unwrap(), header_before);
    assert_eq!(
        reopened
            .rows
            .scan::<serde_json::Value>("pending/", &control())
            .unwrap(),
        pages
    );
    drop(reopened);
    drop(reopened_store);
    drop(root);
}

#[test]
fn no_external_recovery_keeps_pending_and_accepted_receipt_keeps_replay_precedence() {
    let fixture = SupersessionFixture::with_pending("accepted-source-op");
    let headers = fixture.external_headers("approved-recovery");
    let semantic = Arc::new(RecoveryPersistence::new(fixture.store.local.clone()));
    let checkout = fixture.source(semantic.clone());
    let pages = fixture
        .journal
        .rows
        .scan::<serde_json::Value>("pending/", &control())
        .unwrap();

    assert_eq!(
        checkout.pending(&control()).unwrap(),
        Some(fixture.publication.clone())
    );
    assert!(
        fixture
            .journal
            .rows
            .get::<RecoveredSourceSupersession>("superseded/accepted-source-op", &control())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .journal
            .rows
            .scan::<serde_json::Value>("pending/", &control())
            .unwrap(),
        pages
    );

    let receipt = accepted(fixture.store.publish(&fixture.journal).unwrap());
    semantic.set_unavailable(headers);
    semantic.set_inspection_fault(InspectionFault::Missing(missing_header(
        "unrelated-missing-body",
        "src/unrelated.rs",
        PROVIDER,
    )));
    assert_eq!(
        checkout.pending(&control()).unwrap(),
        Some(fixture.publication.clone())
    );
    assert_eq!(
        accepted(fixture.store.publish(&fixture.journal).unwrap()),
        receipt
    );
    assert_eq!(
        fixture.journal.pending(&control()).unwrap(),
        Some(fixture.publication)
    );
    assert!(
        fixture
            .journal
            .rows
            .get::<RecoveredSourceSupersession>("superseded/accepted-source-op", &control())
            .unwrap()
            .is_none()
    );
}

#[test]
fn wrong_provider_incoming_and_in_scope_headers_do_not_supersede() {
    for blocker in ["wrong_provider", "incoming", "in_scope"] {
        let fixture = SupersessionFixture::with_pending("scoped-source-op");
        let b = fixture.external_b_reference();
        let (record, path, provider) = match blocker {
            "wrong_provider" => (b.as_str(), "src/b.rs", "another-provider"),
            "incoming" => ("a", "src/a.rs", PROVIDER),
            _ => (b.as_str(), "src/a.rs", PROVIDER),
        };
        let semantic = Arc::new(RecoveryPersistence::new(fixture.store.local.clone()));
        semantic.set_unavailable(vec![unavailable_header(
            record,
            path,
            provider,
            "approved-recovery",
        )]);
        let checkout = fixture.source(semantic);
        let before = fixture.journal.pending(&control()).unwrap();
        assert_eq!(checkout.pending(&control()).unwrap(), before);
        assert!(
            fixture
                .journal
                .rows
                .get::<RecoveredSourceSupersession>("superseded/scoped-source-op", &control())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn stale_frozen_digest_cannot_clear_the_ready_pending_pointer() {
    let fixture = SupersessionFixture::with_pending("stale-digest-op");
    let headers = fixture.external_headers("approved-recovery");
    let mut stale = fixture.publication.clone();
    stale.inventory[0].projection_sha256 = Some(sha("different frozen projection"));

    assert!(
        fixture
            .journal
            .supersede_recovered_pending(&stale, &headers, &control())
            .unwrap_err()
            .to_string()
            .contains("identical ready unacknowledged source operation")
    );
    assert_eq!(
        fixture.journal.pending(&control()).unwrap(),
        Some(fixture.publication.clone())
    );
    assert!(
        fixture
            .journal
            .rows
            .get::<RecoveredSourceSupersession>("superseded/stale-digest-op", &control())
            .unwrap()
            .is_none()
    );
}

#[test]
fn stale_checkpoint_cas_cannot_clear_a_newer_valid_pending_request() {
    let fixture = SupersessionFixture::with_pending("superseded-by-race");
    let headers = fixture.external_headers("approved-recovery");
    let newer = fixture
        .store
        .publication(&fixture.journal, "newer-pending", &[("b", "B2")], false);
    let racing = Arc::new(RacingCheckpointPersistence::new(
        fixture.store.local.clone(),
        fixture.binding.clone(),
        newer.clone(),
        "superseded-by-race",
    ));
    let journal = IndexPublicationJournal::new(
        fixture.store.local.clone(),
        racing.clone(),
        fixture.binding.clone(),
        ROOT.into(),
    )
    .unwrap();

    assert!(
        journal
            .supersede_recovered_pending(&fixture.publication, &headers, &control())
            .is_err()
    );

    let current = fixture.reopened_journal();
    assert!(racing.raced.load(Ordering::Acquire));
    assert_eq!(current.pending(&control()).unwrap(), Some(newer));
    assert!(
        current
            .rows
            .get::<RecoveredSourceSupersession>("superseded/superseded-by-race", &control())
            .unwrap()
            .is_none()
    );
}

struct RacingCheckpointPersistence {
    local: Arc<LocalPersistence>,
    binding: IndexCheckoutBinding,
    replacement: Mutex<Option<FrozenIndexPublication>>,
    superseded_operation: String,
    raced: AtomicBool,
}

impl RacingCheckpointPersistence {
    fn new(
        local: Arc<LocalPersistence>,
        binding: IndexCheckoutBinding,
        replacement: FrozenIndexPublication,
        superseded_operation: &str,
    ) -> Self {
        Self {
            local,
            binding,
            replacement: Mutex::new(Some(replacement)),
            superseded_operation: superseded_operation.into(),
            raced: AtomicBool::new(false),
        }
    }

    fn publish_newer_pending(
        &self,
        operation: &RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<()> {
        let (plugin_id, table_name, row_key, previous) = supersession_header(operation)?;
        let mut header: CheckpointHeader = serde_json::from_value(previous.clone()).unwrap();
        header.pending = None;
        RelationalPersistence::execute(
            &*self.local,
            RelationalOperation::ApplyMutations {
                plugin_id: plugin_id.clone(),
                mutations: vec![
                    PluginDataMutation::RequireValue {
                        table_name: table_name.clone(),
                        row_key: row_key.clone(),
                        expected: PluginDataExpectedValue::Present(previous),
                    },
                    PluginDataMutation::Put {
                        table_name,
                        row_key,
                        value: serde_json::to_value(header).unwrap(),
                    },
                ],
            },
            control,
        )?;
        let journal = IndexPublicationJournal::new(
            self.local.clone(),
            self.local.clone(),
            self.binding.clone(),
            ROOT.into(),
        )
        .unwrap();
        journal
            .freeze(&self.replacement.lock().unwrap().take().unwrap(), control)
            .unwrap();
        Ok(())
    }
}

impl RelationalPersistence for RacingCheckpointPersistence {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        if supersession_write(&operation, &self.superseded_operation)
            && !self.raced.swap(true, Ordering::AcqRel)
        {
            self.publish_newer_pending(&operation, control)?;
        }
        RelationalPersistence::execute(&*self.local, operation, control)
    }

    fn readiness(&self) -> PersistenceResult<RelationalReadiness> {
        RelationalPersistence::readiness(&*self.local)
    }
}

fn supersession_write(operation: &RelationalOperation, operation_id: &str) -> bool {
    let RelationalOperation::ApplyMutations { mutations, .. } = operation else {
        return false;
    };
    let audit_suffix = format!("/superseded/{operation_id}");
    mutations.iter().any(|mutation| {
        matches!(mutation, PluginDataMutation::Put { row_key, .. } if row_key.ends_with(&audit_suffix))
    })
}

fn supersession_header(
    operation: &RelationalOperation,
) -> PersistenceResult<(String, String, String, serde_json::Value)> {
    let RelationalOperation::ApplyMutations {
        plugin_id,
        mutations,
    } = operation
    else {
        return Err(DbError::invalid_value("non-mutation", "a supersession CAS"));
    };
    let header = mutations.iter().find_map(|mutation| match mutation {
        PluginDataMutation::RequireValue {
            table_name,
            row_key,
            expected: PluginDataExpectedValue::Present(value),
        } if row_key.ends_with("/header") => Some((
            plugin_id.clone(),
            table_name.clone(),
            row_key.clone(),
            value.clone(),
        )),
        _ => None,
    });
    header.ok_or_else(|| DbError::invalid_value("missing header CAS", "a pending checkpoint"))
}

#[test]
fn inspection_error_returns_unchanged_pending_for_ordinary_replay() {
    let fixture = SupersessionFixture::with_pending("inspection-error-op");
    let semantic = Arc::new(RecoveryPersistence::new(fixture.store.local.clone()));
    semantic.set_unavailable(fixture.external_headers("approved-recovery"));
    semantic.set_inspection_fault(InspectionFault::Error);
    let checkout = fixture.source(semantic);

    assert_eq!(
        checkout.pending(&control()).unwrap(),
        Some(fixture.publication.clone())
    );
    assert!(
        fixture
            .journal
            .rows
            .get::<RecoveredSourceSupersession>("superseded/inspection-error-op", &control())
            .unwrap()
            .is_none()
    );
    let receipt = accepted(fixture.store.publish(&fixture.journal).unwrap());
    assert_eq!(receipt.operation_id, fixture.publication.operation_id);
}

#[test]
fn unaccepted_pending_with_unacknowledged_loss_fails_closed_without_clearing() {
    let fixture = SupersessionFixture::with_pending("unacknowledged-loss-op");
    let semantic = Arc::new(RecoveryPersistence::new(fixture.store.local.clone()));
    semantic.set_unavailable(fixture.external_headers("approved-recovery"));
    semantic.set_inspection_fault(InspectionFault::Missing(missing_header(
        "unacknowledged-loss",
        "src/broken.rs",
        PROVIDER,
    )));
    let checkout = fixture.source(semantic);

    assert!(checkout.pending(&control()).is_err());
    assert_eq!(
        fixture.journal.pending(&control()).unwrap(),
        Some(fixture.publication)
    );
    assert!(
        fixture
            .journal
            .rows
            .get::<RecoveredSourceSupersession>("superseded/unacknowledged-loss-op", &control())
            .unwrap()
            .is_none()
    );
}

#[test]
fn mismatched_inspection_identity_or_operation_fails_closed() {
    for fault in [
        InspectionFault::WrongProject,
        InspectionFault::WrongOperation,
    ] {
        let fixture = SupersessionFixture::with_pending("mismatched-inspection-op");
        let semantic = Arc::new(RecoveryPersistence::new(fixture.store.local.clone()));
        semantic.set_unavailable(fixture.external_headers("approved-recovery"));
        semantic.set_inspection_fault(fault);
        let checkout = fixture.source(semantic);

        assert!(checkout.pending(&control()).is_err());
        assert_eq!(
            fixture.journal.pending(&control()).unwrap(),
            Some(fixture.publication)
        );
        assert!(
            fixture
                .journal
                .rows
                .get::<RecoveredSourceSupersession>(
                    "superseded/mismatched-inspection-op",
                    &control()
                )
                .unwrap()
                .is_none()
        );
    }
}
