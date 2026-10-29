use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ConcurrentAcknowledgementRows {
    local: Arc<LocalPersistence>,
    completing: IndexPublicationJournal,
    receipt: SemanticRevisionPublicationReceipt,
    header_reads: AtomicUsize,
}

impl RelationalPersistence for ConcurrentAcknowledgementRows {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        if matches!(&operation, RelationalOperation::GetPluginData { row_key, .. } if row_key.ends_with("/header"))
            && self.header_reads.fetch_add(1, Ordering::AcqRel) == 1
        {
            self.completing.acknowledge(&self.receipt, control).unwrap();
        }
        RelationalPersistence::execute(&*self.local, operation, control)
    }

    fn readiness(&self) -> PersistenceResult<RelationalReadiness> {
        RelationalPersistence::readiness(&*self.local)
    }
}

#[test]
fn concurrent_completion_between_header_and_pending_reads_is_idempotent() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let edit = fixture.publication(&journal, "edit", &[("a", "A2")], false);
    journal.freeze(&edit, &control()).unwrap();
    let receipt = accepted(fixture.publish(&journal).unwrap());
    let raced = IndexPublicationJournal::new(
        fixture.local.clone(),
        Arc::new(ConcurrentAcknowledgementRows {
            local: fixture.local.clone(),
            completing: journal,
            receipt: receipt.clone(),
            header_reads: AtomicUsize::new(0),
        }),
        fixture.binding("A"),
        ROOT.into(),
    )
    .unwrap();
    raced.acknowledge(&receipt, &control()).unwrap();
    assert!(raced.pending(&control()).unwrap().is_none());
    assert_eq!(raced.header(&control()).unwrap().producer_generation, 2);
    assert_eq!(
        raced.accepted_element("a", &control()).unwrap(),
        Some(
            receipt
                .remaps
                .iter()
                .find(|remap| remap.incoming.record_id == "a")
                .unwrap()
                .accepted
                .record_id
                .clone()
        )
    );
}

#[test]
fn acknowledgement_requires_the_producer_pin_from_the_same_atomic_receipt() {
    let fixture = CheckoutFixture::new();
    let journal = fixture.bootstrap();
    let edit = fixture.publication(&journal, "edit", &[("a", "A2")], false);
    journal.freeze(&edit, &control()).unwrap();
    let receipt = accepted(fixture.publish(&journal).unwrap());
    let before = journal.inventory(&control()).unwrap();
    let mut forged = receipt.clone();
    forged.producer_base = None;
    assert!(journal.acknowledge(&forged, &control()).is_err());
    let mut forged = receipt.clone();
    forged.producer_base.as_mut().unwrap().generation += 1;
    assert!(journal.acknowledge(&forged, &control()).is_err());
    assert_eq!(journal.inventory(&control()).unwrap(), before);
    assert_eq!(journal.pending(&control()).unwrap().unwrap(), edit);
    journal.acknowledge(&receipt, &control()).unwrap();
    assert_eq!(journal.header(&control()).unwrap().producer_generation, 2);
}
