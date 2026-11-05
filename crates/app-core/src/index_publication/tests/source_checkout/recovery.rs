mod acknowledgement;
use super::*;
use lumvise_db_core::{DbError, PersistenceResult};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
struct InitialSource {
    fixture: SourceCheckoutFixture,
    frozen: FrozenIndexPublication,
    receipt: SemanticRevisionPublicationReceipt,
}

impl InitialSource {
    fn new() -> Self {
        let fixture = SourceCheckoutFixture::new();
        let checkout = fixture.checkout(fixture.root.path(), "A");
        let frozen = fixture.prepare(&checkout, fixture.root.path()).unwrap();
        let receipt = fixture.publish(&checkout);
        Self {
            fixture,
            frozen,
            receipt,
        }
    }
    fn producer_selection_id(&self) -> String {
        self.receipt
            .producer_base
            .as_ref()
            .unwrap()
            .selection_id
            .clone()
    }
    fn source_record_id(&self, path: &str) -> String {
        let incoming = self
            .frozen
            .batch
            .semantic_elements
            .iter()
            .find(|element| element.path == path)
            .unwrap()
            .semantic_element_id
            .clone();
        accepted_id(&self.receipt, &incoming)
    }
}

struct StaleRecovery {
    source: InitialSource,
    checkout: IndexSourceCheckout,
    stale: FrozenIndexPublication,
    stale_digest: String,
    stale_pages: Vec<Value>,
    unavailable: Vec<SemanticRevisionRecordHeader>,
}

impl StaleRecovery {
    fn new() -> Self {
        let source = InitialSource::new();
        let other_root = source.fixture.copy_checkout();
        let stale = stale_pending_after_other_change(&source, &other_root);
        let (stale_digest, stale_pages) = pending_snapshot(&source, &stale);
        let (unavailable, semantic) = stale_recovery_port(&source);
        let checkout = source.fixture.checkout_with_semantic(
            source.fixture.root.path(),
            "A",
            semantic.clone(),
        );
        Self {
            source,
            checkout,
            stale,
            stale_digest,
            stale_pages,
            unavailable,
        }
    }

    fn assert_stale_audit(&self) {
        let journal = source_journal(&self.source.fixture, self.source.fixture.root.path(), "A");
        let audit = journal
            .rows
            .get::<Value>(
                &format!("superseded/{}", self.stale.operation_id),
                &control(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(audit["frozen_digest"], self.stale_digest);
        assert_eq!(audit["blocked_headers"], json!(self.unavailable));
        assert!(audit["scoped_conflict"].is_object());
        assert!(journal.header(&control()).unwrap().pending.is_none());
        assert_old_pending_pages_preserved(self, &journal);
    }
    fn freeze_recovery(&self) -> FrozenIndexPublication {
        let before = self.checkout.before_scan(&control()).unwrap();
        assert!(before.requires_full_scan);
        let batch = self
            .source
            .fixture
            .projected_batch(&self.checkout, self.source.fixture.root.path());
        self.checkout
            .freeze_batch(batch, true, before, &control())
            .unwrap()
            .unwrap()
    }

    fn assert_recovery_is_current(&self, frozen: &FrozenIndexPublication) {
        let working = scoped_selection(&self.source.fixture.store.local, "working", PROVIDER);
        let producer = scoped_selection(
            &self.source.fixture.store.local,
            &self.source.producer_selection_id(),
            PROVIDER,
        );
        assert_current_recovery_expectation(self, frozen, &working, &producer);
        self.assert_durable_recovery(frozen, &working, &producer);
    }

    fn assert_durable_recovery(
        &self,
        frozen: &FrozenIndexPublication,
        working: &SemanticRevisionSelectionObservation,
        producer: &SemanticRevisionSelectionObservation,
    ) {
        let journal = source_journal(&self.source.fixture, self.source.fixture.root.path(), "A");
        let header = journal.header(&control()).unwrap();
        let recovery = recovery_row(&journal, &frozen.operation_id);
        assert_recovery_observation_row(self, &header, &recovery, working, producer);
        assert_eq!(
            self.checkout.pending(&control()).unwrap(),
            Some(frozen.clone())
        );
        assert_old_pending_pages_preserved(self, &journal);
    }
}
fn assert_current_recovery_expectation(
    scenario: &StaleRecovery,
    frozen: &FrozenIndexPublication,
    working: &SemanticRevisionSelectionObservation,
    producer: &SemanticRevisionSelectionObservation,
) {
    let observation = frozen.observation().unwrap();
    assert!(observation.producer_base.is_none());
    assert_eq!(observation.expected, working.expected);
    assert_current_source_generations(scenario, frozen, working, producer);
}
fn assert_current_source_generations(
    scenario: &StaleRecovery,
    frozen: &FrozenIndexPublication,
    working: &SemanticRevisionSelectionObservation,
    producer: &SemanticRevisionSelectionObservation,
) {
    assert!(
        working.selection.generation > scenario.stale.observation().unwrap().expected.generation
    );
    assert_eq!(
        producer.selection.generation,
        scenario
            .source
            .receipt
            .producer_base
            .as_ref()
            .unwrap()
            .generation
    );
    assert_ne!(frozen.operation_id, scenario.stale.operation_id);
}
fn assert_old_pending_pages_preserved(scenario: &StaleRecovery, journal: &IndexPublicationJournal) {
    assert_eq!(
        pending_pages(journal, &scenario.stale_digest),
        scenario.stale_pages
    );
}
fn assert_recovery_observation_row(
    scenario: &StaleRecovery,
    header: &CheckpointHeader,
    recovery: &Value,
    working: &SemanticRevisionSelectionObservation,
    producer: &SemanticRevisionSelectionObservation,
) {
    assert_eq!(
        recovery["frozen_digest"],
        header.pending.as_ref().unwrap().digest
    );
    assert_eq!(recovery["observed"]["working"], json!(working));
    assert_eq!(recovery["observed"]["producer"], json!(producer));
    assert_eq!(recovery["unavailable_headers"], json!(scenario.unavailable));
}

#[test]
fn stale_request_is_audited_and_fresh_recovery_uses_current_membership() {
    let scenario = StaleRecovery::new();
    let pending = scenario.checkout.pending(&control()).unwrap();
    assert!(pending.is_none());
    scenario.assert_stale_audit();
    let fresh = scenario.freeze_recovery();
    scenario.assert_recovery_is_current(&fresh);
}

#[test]
fn foreign_provider_headers_do_not_trigger_source_recovery() {
    let source = InitialSource::new();
    let foreign = unavailable_element_header("foreign-provider-record", "a.rs", "documents");
    let semantic =
        unavailable_persistence_for_selections(&source, vec![foreign.clone()], vec![foreign]);
    let checkout = source
        .fixture
        .checkout_with_semantic(source.fixture.root.path(), "A", semantic);
    let before = checkout.before_scan(&control()).unwrap();
    assert!(!before.requires_full_scan);
    let batch = source
        .fixture
        .projected_batch(&checkout, source.fixture.root.path());
    assert!(
        checkout
            .freeze_batch(batch, true, before, &control())
            .unwrap()
            .is_none()
    );
}

#[test]
fn failed_unavailable_path_cannot_create_or_finalize_recovery() {
    let source = InitialSource::new();
    let checkout = checkout_with_unavailable_path(&source, "a.rs");
    let before = checkout.before_scan(&control()).unwrap();
    let batch = omit_path(
        &source
            .fixture
            .projected_batch(&checkout, source.fixture.root.path()),
        "a.rs",
    );
    let frozen = checkout
        .freeze_batch_with_failures(batch, true, before, &["a.rs".into()], &control())
        .unwrap();
    assert!(frozen.is_none());
    assert!(checkout.pending(&control()).unwrap().is_none());
    assert_no_recovery_intents(&source);
}

#[test]
fn recovery_republishes_without_a_failing_path_whose_records_survived() {
    // Records of a.rs were lost and membership changed outside, so this scan is a repair.
    let scenario = StaleRecovery::new();
    assert!(scenario.checkout.pending(&control()).unwrap().is_none());
    let (source, checkout) = (&scenario.source, &scenario.checkout);
    let before = checkout.before_scan(&control()).unwrap();
    assert!(before.requires_full_scan);
    let batch = omit_path(
        &source
            .fixture
            .projected_batch(checkout, source.fixture.root.path()),
        "b.rs",
    );
    let frozen = checkout
        .freeze_batch_with_failures(batch, true, before, &["b.rs".into()], &control())
        .unwrap()
        .unwrap();
    let journal = source_journal(&source.fixture, source.fixture.root.path(), "A");
    assert_eq!(
        recovery_row(&journal, &frozen.operation_id)["operation_id"],
        frozen.operation_id
    );
    assert!(frozen.batch.replace_paths.is_empty() && frozen.batch.removed_paths.is_empty());
    let paths: Vec<_> = frozen
        .inventory
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["a.rs"]);
    assert!(
        frozen
            .batch
            .semantic_elements
            .iter()
            .all(|element| element.path != "b.rs")
    );
}

#[test]
fn recovery_pin_drops_selected_connections_whose_definitions_were_lost() {
    let scenario = PinRecovery::new(PinFault::None);
    *scenario.proxy.dangling_producer_connection.lock().unwrap() =
        Some("connection:lost-with-its-publication:0".into());
    scenario
        .checkout
        .acknowledge(&scenario.source_receipt, &control())
        .unwrap();
    assert_eq!(
        scenario.proxy.project_member_reads.load(Ordering::Acquire),
        0
    );
    let pin = pin_selection(&scenario);
    assert_eq!(pin.selection.generation, scenario.pin_generation + 1);
    let kept: BTreeSet<_> = pin.expected.connection_ids.iter().collect();
    assert!(
        !scenario
            .source_receipt
            .accepted_scope
            .connection_ids
            .is_empty()
    );
    assert!(
        scenario
            .source_receipt
            .accepted_scope
            .connection_ids
            .iter()
            .all(|id| kept.contains(id))
    );
}

#[test]
fn stored_recovery_pin_naming_a_lost_connection_is_repaired_before_retry() {
    let scenario = PinRecovery::new(PinFault::BeforeExecution);
    let receipt = fail_recovery_ack_after_source_acceptance(&scenario);
    // The pin frozen by an earlier attempt still names a connection whose definition was lost.
    let journal = pin_journal(&scenario);
    let key = format!("recovery/{}", scenario.frozen.operation_id);
    let mut row: Value = journal.rows.get(&key, &control()).unwrap().unwrap();
    let ids = row["pin_update"]["connection_ids"].as_array_mut().unwrap();
    ids.push(json!("connection:lost-with-its-publication:0"));
    ids.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    let mutation = journal.rows.put(&key, &row).unwrap();
    journal.rows.apply(vec![mutation], &control()).unwrap();
    let after = finish_recovery_after_reopen(&scenario, &receipt);
    assert_eq!(after, scenario.pin_generation + 1);
}

#[test]
fn receipt_without_durable_recovery_intent_cannot_acknowledge_source_repair() {
    let scenario = PinRecovery::new(PinFault::None);
    let receipt = scenario.source_receipt.clone();
    assert!(receipt.producer_base.is_none());
    let journal = source_journal(
        &scenario.source.fixture,
        scenario.source.fixture.root.path(),
        "A",
    );
    remove_recovery_intent(&journal, &receipt.operation_id);
    let before = journal.header(&control()).unwrap();
    assert!(scenario.checkout.acknowledge(&receipt, &control()).is_err());
    assert_eq!(journal.header(&control()).unwrap(), before);
}
fn remove_recovery_intent(journal: &IndexPublicationJournal, operation: &str) {
    let mutation = journal.rows.delete(&format!("recovery/{operation}"));
    journal.rows.apply(vec![mutation], &control()).unwrap();
}

#[test]
fn pin_retry_before_select_preserves_document_and_advances_once() {
    let (before, after) = verify_pin_retry(PinFault::BeforeExecution);
    assert_eq!(after, before + 1);
}

#[test]
fn pin_retry_after_select_preserves_document_and_advances_once() {
    let (before, after) = verify_pin_retry(PinFault::AfterExecution);
    assert_eq!(after, before + 1);
}

#[test]
fn pin_conflict_rebases_and_audits_failed_request_after_outside_addition() {
    let scenario = PinRecovery::new(PinFault::RaceWithOutsideAddition);
    let receipt = scenario.source_receipt.clone();
    assert!(receipt.producer_base.is_none());
    let before = pin_selection(&scenario);
    assert!(!before.expected.records.contains(&scenario.document));
    select_document_in_pin(
        &scenario.source,
        &scenario.source.producer_selection_id(),
        &scenario.document,
    );
    scenario.checkout.acknowledge(&receipt, &control()).unwrap();
    scenario.checkout.acknowledge(&receipt, &control()).unwrap();
    assert_rebased_pin(&scenario, &receipt);
    assert_pin_conflict_audit(&scenario);
}

fn assert_rebased_pin(scenario: &PinRecovery, receipt: &SemanticRevisionPublicationReceipt) {
    let pin = pin_selection(scenario);
    assert_pin_members_are_source_plus_document(scenario, receipt, &pin);
    assert_eq!(pin.selection.generation, scenario.pin_generation + 2);
    assert_eq!(
        resolve_record(&scenario.source, &scenario.document),
        scenario.document_before
    );
    assert_eq!(
        scenario.proxy.source_publications.load(Ordering::Acquire),
        1
    );
}

fn assert_pin_conflict_audit(scenario: &PinRecovery) {
    let (request, conflict) = scenario
        .proxy
        .first_conflict
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert_eq!(request.expected_generation, scenario.pin_generation);
    assert_eq!(conflict.expected_generation, scenario.pin_generation);
    assert_eq!(conflict.actual_generation, scenario.pin_generation + 1);
    let audit = recovery_row(&pin_journal(scenario), &scenario.frozen.operation_id);
    assert_eq!(
        audit["pin_conflicts"],
        json!([{"request": request.clone(), "conflict": conflict.clone()}])
    );
    assert_ne!(audit["pin_update"]["operation_id"], request.operation_id);
}

fn verify_pin_retry(fault: PinFault) -> (i64, i64) {
    let scenario = PinRecovery::new(fault);
    let receipt = fail_recovery_ack_after_source_acceptance(&scenario);
    assert_first_pin_attempt(&scenario);
    let after = finish_recovery_after_reopen(&scenario, &receipt);
    (scenario.pin_generation, after)
}
fn fail_recovery_ack_after_source_acceptance(
    scenario: &PinRecovery,
) -> SemanticRevisionPublicationReceipt {
    let receipt = scenario.source_receipt.clone();
    assert!(receipt.producer_base.is_none());
    assert!(scenario.checkout.acknowledge(&receipt, &control()).is_err());
    receipt
}
fn assert_first_pin_attempt(scenario: &PinRecovery) {
    assert_pin_generation_after_fault(scenario);
    assert_recovery_receipt_waits_for_pin_receipt(scenario);
}
fn assert_pin_generation_after_fault(scenario: &PinRecovery) {
    let pin = pin_selection(scenario);
    let expected_generation = scenario.pin_generation
        + if scenario.fault == PinFault::AfterExecution {
            1
        } else {
            0
        };
    assert_eq!(pin.selection.generation, expected_generation);
}
fn assert_recovery_receipt_waits_for_pin_receipt(scenario: &PinRecovery) {
    let recovery = recovery_row(&pin_journal(scenario), &scenario.frozen.operation_id);
    assert_eq!(recovery["source_receipt"], json!(scenario.source_receipt));
    assert!(recovery["pin_update"].is_object());
    assert!(recovery["pin_receipt"].is_null());
}
fn finish_recovery_after_reopen(
    scenario: &PinRecovery,
    receipt: &SemanticRevisionPublicationReceipt,
) -> i64 {
    let reopened = scenario.source.fixture.checkout_with_semantic(
        scenario.source.fixture.root.path(),
        "A",
        scenario.proxy.clone(),
    );
    assert_eq!(
        reopened.pending(&control()).unwrap(),
        Some(scenario.frozen.clone())
    );
    reopened.acknowledge(receipt, &control()).unwrap();
    reopened.acknowledge(receipt, &control()).unwrap();
    assert_recovered_pin_and_document(scenario, receipt)
}
fn assert_recovered_pin_and_document(
    scenario: &PinRecovery,
    receipt: &SemanticRevisionPublicationReceipt,
) -> i64 {
    let pin = pin_selection(scenario);
    assert_pin_members_are_source_plus_document(scenario, receipt, &pin);
    assert_eq!(
        resolve_record(&scenario.source, &scenario.document),
        scenario.document_before
    );
    assert_saved_pin_receipt(scenario);
    pin.selection.generation
}
fn assert_pin_members_are_source_plus_document(
    scenario: &PinRecovery,
    receipt: &SemanticRevisionPublicationReceipt,
    pin: &SemanticRevisionSelectionObservation,
) {
    let mut expected = receipt.accepted_scope.records.clone();
    expected.push(scenario.document.clone());
    expected.sort();
    expected.dedup();
    assert_eq!(pin.expected.records, expected);
    assert_eq!(
        pin.expected.connection_ids,
        receipt.accepted_scope.connection_ids
    );
}
fn assert_saved_pin_receipt(scenario: &PinRecovery) {
    let recovery = recovery_row(&pin_journal(scenario), &scenario.frozen.operation_id);
    assert_eq!(
        recovery["pin_receipt"]["operation_id"],
        recovery["pin_update"]["operation_id"]
    );
    assert_eq!(recovery["pin_receipt"]["producer_base"], Value::Null);
}

struct PinRecovery {
    source: InitialSource,
    fault: PinFault,
    proxy: Arc<RecoveryPinPersistence>,
    checkout: IndexSourceCheckout,
    frozen: FrozenIndexPublication,
    document: SemanticRecordRef,
    document_before: SemanticRevisionResolution,
    pin_generation: i64,
    source_receipt: SemanticRevisionPublicationReceipt,
}

impl PinRecovery {
    fn new(fault: PinFault) -> Self {
        let source = source_with_working_drift();
        let include_document = fault != PinFault::RaceWithOutsideAddition;
        let (document, document_before, pin_generation) =
            seed_document_pin(&source, include_document);
        let (proxy, checkout) = recovery_checkout(&source, fault);
        let frozen = freeze_recovery(&source, &checkout);
        let source_receipt = publish_source_without_ack(&proxy, &checkout);
        Self {
            source,
            fault,
            proxy,
            checkout,
            frozen,
            document,
            document_before,
            pin_generation,
            source_receipt,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PinFault {
    None,
    BeforeExecution,
    AfterExecution,
    RaceWithOutsideAddition,
}

struct RecoveryPinPersistence {
    revisions: Arc<UnavailableRevisionsPersistence>,
    fault: Mutex<Option<PinFault>>,
    first_conflict: Mutex<Option<(SemanticRevisionSelectionUpdate, SemanticRevisionConflict)>>,
    source_publications: AtomicUsize,
    /// Mirrors a lost publication: producer history names a connection whose definition is gone.
    dangling_producer_connection: Mutex<Option<String>>,
    /// Whole-project membership reads decode every revision; acknowledgement must not need one.
    project_member_reads: AtomicUsize,
}

impl RecoveryPinPersistence {
    fn new(revisions: Arc<UnavailableRevisionsPersistence>, fault: PinFault) -> Self {
        Self {
            revisions,
            fault: Mutex::new((fault != PinFault::None).then_some(fault)),
            first_conflict: Mutex::new(None),
            source_publications: AtomicUsize::new(0),
            dangling_producer_connection: Mutex::new(None),
            project_member_reads: AtomicUsize::new(0),
        }
    }
    fn record_pin_conflict(
        &self,
        update: SemanticRevisionSelectionUpdate,
        result: &SemanticResult,
    ) {
        let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Conflict(
            conflict,
        )) = result
        else {
            panic!("expected a typed generation conflict from the raced pin update");
        };
        *self.first_conflict.lock().unwrap() = Some((update, conflict.clone()));
    }
    fn execute_pin_update(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
        fault: PinFault,
    ) -> PersistenceResult<SemanticResult> {
        if fault == PinFault::BeforeExecution {
            return Err(lost_pin_reply("before"));
        }
        let attempted = (fault == PinFault::RaceWithOutsideAddition).then(|| match &operation {
            SemanticOperation::SelectRevisions(update) => update.clone(),
            _ => panic!("expected SelectRevisions for the recovery pin"),
        });
        let result = self.revisions.execute(operation, control)?;
        if fault == PinFault::AfterExecution && is_accepted_selection(&result) {
            return Err(lost_pin_reply("after"));
        }
        if let Some(update) = attempted {
            self.record_pin_conflict(update, &result);
        }
        Ok(result)
    }
}

impl SemanticPersistence for RecoveryPinPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        if matches!(&operation, SemanticOperation::RevisionSelectionMembers(query)
            if query.structure_coverage == lumvise_db_core::SemanticRevisionStructureCoverage::Project)
        {
            self.project_member_reads.fetch_add(1, Ordering::AcqRel);
        }
        let dangling = self.dangling_producer_connection.lock().unwrap().clone();
        if let (Some(dangling), SemanticOperation::RevisionSelectionAt { selection_id, .. }) =
            (dangling, &operation)
            && selection_id.starts_with("producer:")
        {
            let mut result = self.revisions.execute(operation, control)?;
            if let SemanticResult::RevisionMembership(Some(state)) = &mut result {
                state.connection_ids.push(dangling);
                state.connection_ids.sort();
            }
            return Ok(result);
        }
        if !is_recovery_pin_update(&operation) {
            if matches!(&operation, SemanticOperation::PublishRevisions(_)) {
                self.source_publications.fetch_add(1, Ordering::AcqRel);
            }
            return self.revisions.execute(operation, control);
        }
        match self.fault.lock().unwrap().take() {
            Some(fault) => self.execute_pin_update(operation, control, fault),
            None => self.revisions.execute(operation, control),
        }
    }
    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        let result = self.revisions.readiness()?;
        Ok(result)
    }
}

fn lost_pin_reply(phase: &str) -> DbError {
    DbError::invalid_value(
        format!("pin reply lost {phase} execution"),
        "a producer selection receipt",
    )
}
fn is_recovery_pin_update(operation: &SemanticOperation) -> bool {
    matches!(operation, SemanticOperation::SelectRevisions(update) if update.operation_id.starts_with("source-recovery-pin:"))
}
fn is_accepted_selection(result: &SemanticResult) -> bool {
    matches!(
        result,
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(_))
    )
}

fn publish_source_without_ack(
    proxy: &RecoveryPinPersistence,
    checkout: &IndexSourceCheckout,
) -> SemanticRevisionPublicationReceipt {
    let pending = checkout.pending(&control()).unwrap().unwrap();
    let result = SemanticPersistence::execute(
        proxy,
        SemanticOperation::PublishRevisions(normalized_request(&pending)),
        &control(),
    )
    .unwrap();
    accepted_publication(result)
}
fn accepted_publication(result: SemanticResult) -> SemanticRevisionPublicationReceipt {
    let SemanticResult::RevisionPublication(outcome) = result else {
        panic!("expected a source publication receipt");
    };
    accepted(outcome)
}
fn freeze_recovery(
    source: &InitialSource,
    checkout: &IndexSourceCheckout,
) -> FrozenIndexPublication {
    let before = checkout.before_scan(&control()).unwrap();
    assert!(before.requires_full_scan);
    let batch = source
        .fixture
        .projected_batch(checkout, source.fixture.root.path());
    checkout
        .freeze_batch(batch, true, before, &control())
        .unwrap()
        .unwrap()
}

fn publish_other_checkout(fixture: &SourceCheckoutFixture, root: &tempfile::TempDir) {
    let checkout = fixture.checkout(root.path(), "B");
    fixture.prepare(&checkout, root.path()).unwrap();
    fixture.publish(&checkout);
    fs::write(
        root.path().join("a.rs"),
        "fn a() { let current = 2; b(); }\n",
    )
    .unwrap();
    fixture.prepare(&checkout, root.path()).unwrap();
    fixture.publish(&checkout);
}
fn drift_working_selection(source: &InitialSource) {
    let other_root = source.fixture.copy_checkout();
    publish_other_checkout(&source.fixture, &other_root);
}
fn source_with_working_drift() -> InitialSource {
    let source = InitialSource::new();
    drift_working_selection(&source);
    source
}
fn freeze_source_edit(
    fixture: &SourceCheckoutFixture,
    root: &Path,
    marker: &str,
) -> FrozenIndexPublication {
    fs::write(
        root.join("a.rs"),
        format!("fn a() {{ let {marker} = 1; b(); }}\n"),
    )
    .unwrap();
    let checkout = fixture.checkout(root, "A");
    fixture.prepare(&checkout, root).unwrap()
}
fn stale_pending_after_other_change(
    source: &InitialSource,
    other_root: &tempfile::TempDir,
) -> FrozenIndexPublication {
    let stale = freeze_source_edit(&source.fixture, source.fixture.root.path(), "A2");
    publish_other_checkout(&source.fixture, other_root);
    stale
}

fn stale_recovery_port(
    source: &InitialSource,
) -> (
    Vec<SemanticRevisionRecordHeader>,
    Arc<UnavailableRevisionsPersistence>,
) {
    let unavailable = vec![source.header("a.rs")];
    let semantic = unavailable_persistence_for_selections(source, unavailable.clone(), vec![]);
    (unavailable, semantic)
}

fn pending_snapshot(
    source: &InitialSource,
    publication: &FrozenIndexPublication,
) -> (String, Vec<Value>) {
    let journal = source_journal(&source.fixture, source.fixture.root.path(), "A");
    let pending = journal.header(&control()).unwrap().pending.unwrap();
    assert_eq!(pending.operation_id, publication.operation_id);
    let pages = pending_pages(&journal, &pending.digest);
    (pending.digest, pages)
}

fn unavailable_persistence_for_selections(
    source: &InitialSource,
    producer: Vec<SemanticRevisionRecordHeader>,
    working: Vec<SemanticRevisionRecordHeader>,
) -> Arc<UnavailableRevisionsPersistence> {
    let semantic = Arc::new(UnavailableRevisionsPersistence::new(
        source.fixture.store.local.clone(),
    ));
    semantic.set_headers(producer, working);
    semantic
}
fn checkout_with_unavailable_path(source: &InitialSource, path: &str) -> IndexSourceCheckout {
    let semantic = unavailable_persistence_for_selections(
        source,
        vec![source.header(path)],
        vec![source.header(path)],
    );
    source
        .fixture
        .checkout_with_semantic(source.fixture.root.path(), "A", semantic)
}

fn assert_no_recovery_intents(source: &InitialSource) {
    let journal = source_journal(&source.fixture, source.fixture.root.path(), "A");
    let recoveries = journal.rows.scan::<Value>("recovery/", &control()).unwrap();
    assert!(recoveries.is_empty());
    assert!(journal.header(&control()).unwrap().pending.is_none());
}

fn source_journal(
    fixture: &SourceCheckoutFixture,
    root: &Path,
    client: &str,
) -> IndexPublicationJournal {
    let binding = resolved_source_binding(fixture, root, client);
    IndexPublicationJournal::new(
        fixture.store.local.clone(),
        fixture.store.local.clone(),
        binding,
        ROOT.into(),
    )
    .unwrap()
}
fn pin_journal(scenario: &PinRecovery) -> IndexPublicationJournal {
    source_journal(
        &scenario.source.fixture,
        scenario.source.fixture.root.path(),
        "A",
    )
}

fn resolved_source_binding(
    fixture: &SourceCheckoutFixture,
    root: &Path,
    client: &str,
) -> IndexCheckoutBinding {
    crate::index_publication::source_binding::resolve(
        fixture.store.local.as_ref(),
        fixture.store.local.clone(),
        client,
        root,
        ROOT,
        PROVIDER,
        &control(),
    )
    .unwrap()
}

fn scoped_selection(
    local: &LocalPersistence,
    selection_id: &str,
    provider: &str,
) -> SemanticRevisionSelectionObservation {
    let query = SemanticRevisionSelectionQuery {
        project_root: ROOT.into(),
        selection_id: selection_id.into(),
        structure_coverage: SemanticRevisionStructureCoverage::Partition(SemanticPartition {
            project_root: ROOT.into(),
            provider_instance_id: Some(provider.into()),
            replace_paths: vec![],
        }),
        artifact_ids: vec![],
    };
    selection_observation(local, query)
}
fn project_selection(
    local: &LocalPersistence,
    selection_id: &str,
) -> SemanticRevisionSelectionObservation {
    let query = SemanticRevisionSelectionQuery {
        project_root: ROOT.into(),
        selection_id: selection_id.into(),
        structure_coverage: SemanticRevisionStructureCoverage::Project,
        artifact_ids: vec![],
    };
    selection_observation(local, query)
}
fn pin_selection(scenario: &PinRecovery) -> SemanticRevisionSelectionObservation {
    project_selection(
        &scenario.source.fixture.store.local,
        &scenario.source.producer_selection_id(),
    )
}
fn selection_observation(
    local: &LocalPersistence,
    query: SemanticRevisionSelectionQuery,
) -> SemanticRevisionSelectionObservation {
    let result = SemanticPersistence::execute(
        local,
        SemanticOperation::RevisionSelectionMembers(query),
        &control(),
    )
    .unwrap();
    let SemanticResult::RevisionObservation(Some(observation)) = result else {
        panic!("expected independent selected-membership read");
    };
    observation
}

fn accepted_id(receipt: &SemanticRevisionPublicationReceipt, incoming: &str) -> String {
    receipt
        .remaps
        .iter()
        .find(|remap| remap.incoming == SemanticRecordRef::element(incoming))
        .unwrap()
        .accepted
        .record_id
        .clone()
}

fn publish_document(source: &InitialSource) -> SemanticRecordRef {
    let observation = scoped_selection(&source.fixture.store.local, "working", "documents");
    let request = document_publication(source, observation, document_element());
    let result = SemanticPersistence::execute(
        &*source.fixture.store.local,
        SemanticOperation::PublishRevisions(request),
        &control(),
    )
    .unwrap();
    accepted_publication(result).accepted_scope.records[0].clone()
}

fn seed_document_pin(
    source: &InitialSource,
    include_document: bool,
) -> (SemanticRecordRef, SemanticRevisionResolution, i64) {
    let document = publish_document(source);
    let producer_id = source.producer_selection_id();
    if include_document {
        select_document_in_pin(source, &producer_id, &document);
    }
    let generation = project_selection(&source.fixture.store.local, &producer_id)
        .selection
        .generation;
    let resolved = resolve_record(source, &document);
    (document, resolved, generation)
}

fn recovery_checkout(
    source: &InitialSource,
    fault: PinFault,
) -> (Arc<RecoveryPinPersistence>, IndexSourceCheckout) {
    let unavailable = vec![source.header("a.rs")];
    let revisions = unavailable_persistence_for_selections(source, unavailable, vec![]);
    let proxy = Arc::new(RecoveryPinPersistence::new(revisions, fault));
    let checkout =
        source
            .fixture
            .checkout_with_semantic(source.fixture.root.path(), "A", proxy.clone());
    (proxy, checkout)
}

fn document_publication(
    source: &InitialSource,
    observation: SemanticRevisionSelectionObservation,
    document: SemanticElement,
) -> SemanticRevisionPublication {
    SemanticRevisionPublication {
        operation_id: "recovery-test-document".into(),
        project_root: ROOT.into(),
        project_id: source.fixture.store.project_id.clone(),
        selection_id: "working".into(),
        source_context: observation.selection.source_context,
        producer_base: None,
        expected: observation.expected,
        structure: Some(document_structure(document)),
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    }
}
fn document_structure(document: SemanticElement) -> SemanticRevisionStructure {
    SemanticRevisionStructure {
        partition: Some(SemanticPartition {
            project_root: ROOT.into(),
            provider_instance_id: Some("documents".into()),
            replace_paths: vec![],
        }),
        elements: vec![document],
        relationships: vec![],
    }
}

fn document_element() -> SemanticElement {
    SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: "recovery-document-note".into(),
        semantic_source_id: "document-source".into(),
        path: "notes.md".into(),
        element_kind: "document".into(),
        name: "Recovery test note".into(),
        parent_element_id: None,
        revision_dependencies: vec![],
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"provider_instance_id":"documents","producer_element_id":"recovery-document-note"}),
    }
}

fn select_document_in_pin(source: &InitialSource, producer_id: &str, document: &SemanticRecordRef) {
    let selection = project_selection(&source.fixture.store.local, producer_id);
    let update = document_pin_update(
        &source.fixture.store.project_id,
        "recovery-test-document-pin",
        selection,
        document,
    );
    let result = SemanticPersistence::execute(
        &*source.fixture.store.local,
        SemanticOperation::SelectRevisions(update),
        &control(),
    )
    .unwrap();
    assert!(is_accepted_selection(&result));
}
fn document_pin_update(
    project_id: &str,
    operation_id: &str,
    selection: SemanticRevisionSelectionObservation,
    document: &SemanticRecordRef,
) -> SemanticRevisionSelectionUpdate {
    let records = selected_with_document(&selection.expected.records, document);
    SemanticRevisionSelectionUpdate {
        operation_id: operation_id.into(),
        project_root: ROOT.into(),
        project_id: project_id.into(),
        selection_id: selection.selection.selection_id.clone(),
        source_context: selection.selection.source_context.clone(),
        expected_generation: selection.selection.generation,
        artifact_edits: selection.expected.artifact_edits,
        mode: selection.selection.mode,
        records,
        connection_ids: selection.expected.connection_ids,
        source_anchor: selection.selection.source_anchor,
    }
}
fn selected_with_document(
    selected: &[SemanticRecordRef],
    document: &SemanticRecordRef,
) -> Vec<SemanticRecordRef> {
    let mut records = selected.to_vec();
    records.push(document.clone());
    records.sort();
    records.dedup();
    records
}

fn resolve_record(
    source: &InitialSource,
    record: &SemanticRecordRef,
) -> SemanticRevisionResolution {
    let result = SemanticPersistence::execute(
        &*source.fixture.store.local,
        SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record: record.clone(),
        },
        &control(),
    )
    .unwrap();
    let SemanticResult::RevisionResolution(Some(revision)) = result else {
        panic!("expected the exact document revision");
    };
    revision
}
fn pending_pages(journal: &IndexPublicationJournal, digest: &str) -> Vec<Value> {
    journal
        .rows
        .scan(&format!("pending/{digest}/"), &control())
        .unwrap()
}
fn recovery_row(journal: &IndexPublicationJournal, operation: &str) -> Value {
    journal
        .rows
        .get(&format!("recovery/{operation}"), &control())
        .unwrap()
        .unwrap()
}

impl InitialSource {
    fn header(&self, path: &str) -> SemanticRevisionRecordHeader {
        let record = self.source_record_id(path);
        let header = unavailable_element_header(&record, path, PROVIDER);
        assert_eq!(header.element_scope.as_ref().unwrap().path, path);
        assert!(header.unavailable.is_some());
        header
    }
}

fn omit_path(
    batch: &lumvise_contracts::IndexBatchRequest,
    path: &str,
) -> lumvise_contracts::IndexBatchRequest {
    let mut partial = batch.clone();
    let failed = failed_element_ids(&partial, path);
    remove_failed_path(&mut partial, path, &failed);
    partial
}
fn failed_element_ids(
    batch: &lumvise_contracts::IndexBatchRequest,
    path: &str,
) -> BTreeSet<String> {
    batch
        .semantic_elements
        .iter()
        .filter(|element| element.path == path)
        .map(|element| element.semantic_element_id.clone())
        .collect()
}
fn remove_failed_path(
    batch: &mut lumvise_contracts::IndexBatchRequest,
    path: &str,
    failed: &BTreeSet<String>,
) {
    batch
        .semantic_elements
        .retain(|element| element.path != path);
    batch.semantic_relationships.retain(|edge| {
        !failed.contains(&edge.source_element_id)
            && !edge
                .target_element_id
                .as_ref()
                .is_some_and(|target| failed.contains(target))
    });
}
