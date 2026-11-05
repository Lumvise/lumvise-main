use super::*;
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq)]
enum ReconciliationBoundary {
    AcceptedPin,
    ConflictObservation,
}

#[derive(Clone, Copy)]
enum StopSignal {
    Cancel,
    Expire,
}

enum ConnectionReplyFault {
    Missing,
    Unexpected,
    Duplicate,
}

struct ReconciliationInterruption {
    boundary: ReconciliationBoundary,
    signal: StopSignal,
    invocation: InvocationControl,
}

struct AcknowledgementPersistence {
    semantic: Arc<RecoveryPinPersistence>,
    local: Arc<LocalPersistence>,
    deadlines: Mutex<Vec<Instant>>,
    connection_queries: Mutex<Vec<Vec<String>>>,
    pin_requests: Mutex<Vec<SemanticRevisionSelectionUpdate>>,
    scope_reads: AtomicUsize,
    conflicted: AtomicBool,
    interruption: Mutex<Option<ReconciliationInterruption>>,
    connection_fault: Mutex<Option<ConnectionReplyFault>>,
    checkpoint_race: Mutex<Option<(IndexPublicationJournal, CheckpointHeader)>>,
}

impl AcknowledgementPersistence {
    fn new(scenario: &PinRecovery) -> Arc<Self> {
        Arc::new(Self {
            semantic: scenario.proxy.clone(),
            local: scenario.source.fixture.store.local.clone(),
            deadlines: Mutex::new(Vec::new()),
            connection_queries: Mutex::new(Vec::new()),
            pin_requests: Mutex::new(Vec::new()),
            scope_reads: AtomicUsize::new(0),
            conflicted: AtomicBool::new(false),
            interruption: Mutex::new(None),
            connection_fault: Mutex::new(None),
            checkpoint_race: Mutex::new(None),
        })
    }

    fn checkout(self: &Arc<Self>, scenario: &PinRecovery) -> IndexSourceCheckout {
        let checkout = IndexSourceCheckout::open(
            self.clone(),
            self.clone(),
            "A",
            scenario.source.fixture.root.path(),
            ROOT,
            PROVIDER,
            &control(),
        )
        .unwrap()
        .for_background_job();
        self.deadlines.lock().unwrap().clear();
        checkout
    }

    fn record_query(&self, operation: &SemanticOperation) {
        if let SemanticOperation::RevisionConnections(query) = operation {
            self.connection_queries
                .lock()
                .unwrap()
                .push(query.connection_ids.clone());
        }
        if let SemanticOperation::SelectRevisions(update) = operation {
            self.pin_requests.lock().unwrap().push(update.clone());
        }
    }

    fn interrupt_at(&self, boundary: ReconciliationBoundary) {
        let mut interruption = self.interruption.lock().unwrap();
        if interruption
            .as_ref()
            .is_none_or(|stop| stop.boundary != boundary)
        {
            return;
        }
        let stop = interruption.take().unwrap();
        match stop.signal {
            StopSignal::Cancel => stop.invocation.cancel(),
            StopSignal::Expire => std::thread::sleep(stop.invocation.remaining()),
        }
    }

    fn adjust_connection_reply(&self, mut result: SemanticResult) -> SemanticResult {
        let SemanticResult::RevisionConnections(edges) = &mut result else {
            return result;
        };
        if edges.is_empty() {
            return result;
        }
        match self.connection_fault.lock().unwrap().take() {
            Some(ConnectionReplyFault::Missing) => {
                edges.pop();
            }
            Some(ConnectionReplyFault::Unexpected) => {
                edges[0].connection_id = "unrequested-edge".into()
            }
            Some(ConnectionReplyFault::Duplicate) => edges.push(edges[0].clone()),
            None => {}
        }
        result
    }

    fn race_checkpoint_completion(
        &self,
        operation: &RelationalOperation,
        control: &InvocationControl,
    ) {
        if !completes_checkpoint(operation) {
            return;
        }
        let Some((journal, header)) = self.checkpoint_race.lock().unwrap().take() else {
            return;
        };
        let mutation = journal.rows.put("header", &header).unwrap();
        journal.rows.apply(vec![mutation], control).unwrap();
    }

    fn assert_shared_deadline(&self, invocation: &InvocationControl) {
        let deadlines = self.deadlines.lock().unwrap();
        assert!(!deadlines.is_empty());
        assert!(
            deadlines
                .iter()
                .all(|deadline| *deadline == invocation.deadline())
        );
    }
}

impl SemanticPersistence for AcknowledgementPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        self.deadlines.lock().unwrap().push(control.deadline());
        self.record_query(&operation);
        let pin = is_recovery_pin_update(&operation);
        let refresh = matches!(&operation, SemanticOperation::RevisionSelectionMembers(_))
            && self.conflicted.load(Ordering::Acquire);
        let result = self.semantic.execute(operation, control)?;
        if pin && is_accepted_selection(&result) {
            self.interrupt_at(ReconciliationBoundary::AcceptedPin);
        } else if pin {
            self.conflicted.store(true, Ordering::Release);
        }
        if refresh {
            self.interrupt_at(ReconciliationBoundary::ConflictObservation);
        }
        Ok(self.adjust_connection_reply(result))
    }

    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        self.semantic.readiness()
    }
}

impl RelationalPersistence for AcknowledgementPersistence {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        self.deadlines.lock().unwrap().push(control.deadline());
        if matches!(&operation, RelationalOperation::PagePluginData { key_prefix: Some(prefix), .. } if prefix.ends_with("/path/"))
        {
            self.scope_reads.fetch_add(1, Ordering::AcqRel);
        }
        self.race_checkpoint_completion(&operation, control);
        RelationalPersistence::execute(&*self.local, operation, control)
    }

    fn readiness(&self) -> PersistenceResult<RelationalReadiness> {
        RelationalPersistence::readiness(&*self.local)
    }
}

fn completes_checkpoint(operation: &RelationalOperation) -> bool {
    matches!(operation, RelationalOperation::ApplyMutations { mutations, .. }
        if mutations.iter().any(|mutation| matches!(mutation,
            PluginDataMutation::Put { row_key, value, .. }
            if row_key.ends_with("/header") && value["pending"].is_null())))
}

#[test]
fn cancelled_accepted_pin_keeps_frozen_request_for_restart() {
    interrupted_pin_recovers(ReconciliationBoundary::AcceptedPin, StopSignal::Cancel);
}

#[test]
fn expired_accepted_pin_keeps_frozen_request_for_restart() {
    interrupted_pin_recovers(ReconciliationBoundary::AcceptedPin, StopSignal::Expire);
}

#[test]
fn cancelled_conflict_refresh_does_not_persist_a_retry() {
    interrupted_pin_recovers(
        ReconciliationBoundary::ConflictObservation,
        StopSignal::Cancel,
    );
}

#[test]
fn expired_conflict_refresh_does_not_persist_a_retry() {
    interrupted_pin_recovers(
        ReconciliationBoundary::ConflictObservation,
        StopSignal::Expire,
    );
}

fn interrupted_pin_recovers(boundary: ReconciliationBoundary, signal: StopSignal) {
    let scenario = interrupted_pin_scenario(boundary);
    let probe = AcknowledgementPersistence::new(&scenario);
    let checkout = probe.checkout(&scenario);
    let before = pin_journal(&scenario).header(&control()).unwrap();
    let invocation = InvocationControl::with_deadline(Duration::from_millis(500));
    *probe.interruption.lock().unwrap() = Some(ReconciliationInterruption {
        boundary,
        signal,
        invocation: invocation.child(),
    });
    assert!(
        checkout
            .acknowledge(&scenario.source_receipt, &invocation)
            .is_err()
    );
    assert_interrupted_checkpoint(&scenario, &probe, &before, &invocation);
    assert_interrupted_pin_replay(&scenario, &probe, boundary);
}

fn interrupted_pin_scenario(boundary: ReconciliationBoundary) -> PinRecovery {
    if boundary == ReconciliationBoundary::AcceptedPin {
        return PinRecovery::new(PinFault::None);
    }
    let scenario = PinRecovery::new(PinFault::RaceWithOutsideAddition);
    select_document_in_pin(
        &scenario.source,
        &scenario.source.producer_selection_id(),
        &scenario.document,
    );
    scenario
}

fn assert_interrupted_checkpoint(
    scenario: &PinRecovery,
    probe: &AcknowledgementPersistence,
    before: &CheckpointHeader,
    invocation: &InvocationControl,
) {
    assert_eq!(pin_journal(scenario).header(&control()).unwrap(), *before);
    assert_eq!(
        scenario.checkout.pending(&control()).unwrap(),
        Some(scenario.frozen.clone())
    );
    let recovery = recovery_row(&pin_journal(scenario), &scenario.frozen.operation_id);
    assert_eq!(recovery["source_receipt"], json!(scenario.source_receipt));
    assert_eq!(
        recovery["pin_update"],
        json!(probe.pin_requests.lock().unwrap()[0])
    );
    assert_eq!(recovery["pin_conflicts"], json!([]));
    assert!(recovery["pin_receipt"].is_null());
    probe.assert_shared_deadline(invocation);
}

fn assert_interrupted_pin_replay(
    scenario: &PinRecovery,
    probe: &AcknowledgementPersistence,
    boundary: ReconciliationBoundary,
) {
    let accepted_generation = pin_selection(scenario).selection.generation;
    let generation = finish_recovery_after_reopen(scenario, &scenario.source_receipt);
    if boundary == ReconciliationBoundary::AcceptedPin {
        assert_eq!(
            generation, accepted_generation,
            "accepted pin must replay its original receipt"
        );
        assert_saved_attempt_receipt(scenario, probe);
    } else {
        assert_eq!(generation, accepted_generation + 1);
        assert_pin_conflict_audit(scenario);
    }
    assert_single_source_publication(scenario);
    assert!(scenario.checkout.pending(&control()).unwrap().is_none());
}

fn assert_saved_attempt_receipt(scenario: &PinRecovery, probe: &AcknowledgementPersistence) {
    let request = probe.pin_requests.lock().unwrap()[0].clone();
    let expected = accepted_publication(
        scenario
            .proxy
            .execute(SemanticOperation::SelectRevisions(request), &control())
            .unwrap(),
    );
    let recovery = recovery_row(&pin_journal(scenario), &scenario.frozen.operation_id);
    assert_eq!(recovery["pin_receipt"], json!(expected));
    assert_single_source_publication(scenario);
}

fn assert_single_source_publication(scenario: &PinRecovery) {
    assert_eq!(
        scenario.proxy.source_publications.load(Ordering::Acquire),
        1
    );
}

#[test]
fn recovery_batches_missing_connections_and_prepares_checkpoint_once() {
    let scenario = PinRecovery::new(PinFault::None);
    let missing = missing_checkpoint_connections(&scenario);
    assert!(
        missing.len() > 1,
        "fixture must exercise multiple retained edges absent from checkpoint"
    );
    let probe = AcknowledgementPersistence::new(&scenario);
    let checkout = probe.checkout(&scenario);
    let invocation = control();
    checkout
        .acknowledge(&scenario.source_receipt, &invocation)
        .unwrap();
    assert_acknowledgement_queries(&scenario, &probe, &missing);
    probe.assert_shared_deadline(&invocation);
    assert_recovered_pin_and_document(&scenario, &scenario.source_receipt);
    assert!(checkout.pending(&control()).unwrap().is_none());
}

fn assert_acknowledgement_queries(
    scenario: &PinRecovery,
    probe: &AcknowledgementPersistence,
    missing: &[String],
) {
    let queries = probe.connection_queries.lock().unwrap().clone();
    let accepted = scenario
        .source_receipt
        .accepted_scope
        .connection_ids
        .clone();
    assert_eq!(queries, vec![missing.to_vec(), accepted]);
    assert_eq!(probe.scope_reads.load(Ordering::Acquire), 1);
    println!(
        "missing_connections={}, connection_queries={}, checkpoint_scope_reads={}",
        missing.len(),
        queries.len(),
        probe.scope_reads.load(Ordering::Acquire)
    );
}

fn missing_checkpoint_connections(scenario: &PinRecovery) -> Vec<String> {
    let journal = pin_journal(scenario);
    let paths = journal
        .paths_in_partition(&scenario.frozen.partition(), &control())
        .unwrap();
    let checkpoint_ids = paths
        .iter()
        .flat_map(|path| path.connections.keys())
        .collect::<BTreeSet<_>>();
    scenario
        .frozen
        .observation()
        .unwrap()
        .expected
        .connection_ids
        .iter()
        .filter(|id| !checkpoint_ids.contains(id))
        .cloned()
        .collect()
}

#[test]
fn missing_batch_connection_cannot_prepare_or_accept_pin() {
    invalid_connection_reply_preserves_checkpoint(ConnectionReplyFault::Missing);
}

#[test]
fn unrequested_batch_connection_cannot_prepare_or_accept_pin() {
    invalid_connection_reply_preserves_checkpoint(ConnectionReplyFault::Unexpected);
}

#[test]
fn duplicate_batch_connection_cannot_prepare_or_accept_pin() {
    invalid_connection_reply_preserves_checkpoint(ConnectionReplyFault::Duplicate);
}

fn invalid_connection_reply_preserves_checkpoint(fault: ConnectionReplyFault) {
    let scenario = PinRecovery::new(PinFault::None);
    let probe = AcknowledgementPersistence::new(&scenario);
    let checkout = probe.checkout(&scenario);
    let before = pin_journal(&scenario).header(&control()).unwrap();
    *probe.connection_fault.lock().unwrap() = Some(fault);
    assert!(
        checkout
            .acknowledge(&scenario.source_receipt, &control())
            .is_err()
    );
    assert_eq!(pin_journal(&scenario).header(&control()).unwrap(), before);
    assert_unprepared_pin(&scenario, &probe);
    finish_recovery_after_reopen(&scenario, &scenario.source_receipt);
}

fn assert_unprepared_pin(scenario: &PinRecovery, probe: &AcknowledgementPersistence) {
    assert_eq!(
        pin_selection(&scenario).selection.generation,
        scenario.pin_generation
    );
    assert!(probe.pin_requests.lock().unwrap().is_empty());
    let recovery = recovery_row(&pin_journal(scenario), &scenario.frozen.operation_id);
    assert!(recovery["source_receipt"].is_null());
}

#[test]
fn prepared_checkpoint_still_requires_original_header_after_pin_acceptance() {
    let scenario = PinRecovery::new(PinFault::None);
    let probe = AcknowledgementPersistence::new(&scenario);
    let checkout = probe.checkout(&scenario);
    let journal = pin_journal(&scenario);
    let before = journal
        .rows
        .scan::<CheckpointPath>("path/", &control())
        .unwrap();
    let mut competing = journal.header(&control()).unwrap();
    competing.generation += 1;
    *probe.checkpoint_race.lock().unwrap() = Some((pin_journal(&scenario), competing.clone()));
    assert!(
        checkout
            .acknowledge(&scenario.source_receipt, &control())
            .is_err()
    );
    assert!(probe.checkpoint_race.lock().unwrap().is_none());
    assert_eq!(journal.header(&control()).unwrap(), competing);
    assert_eq!(
        journal
            .rows
            .scan::<CheckpointPath>("path/", &control())
            .unwrap(),
        before
    );
    assert_eq!(
        pin_selection(&scenario).selection.generation,
        scenario.pin_generation + 1
    );
    assert_saved_pin_receipt(&scenario);
}
