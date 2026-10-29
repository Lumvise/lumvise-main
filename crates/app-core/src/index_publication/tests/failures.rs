use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct FailingCheckpointRows {
    local: Arc<LocalPersistence>,
    fail_seal: bool,
    armed: AtomicBool,
}

pub(super) fn fail_next_seal(local: Arc<LocalPersistence>) -> Arc<dyn RelationalPersistence> {
    Arc::new(FailingCheckpointRows {
        local,
        fail_seal: true,
        armed: AtomicBool::new(true),
    })
}

impl RelationalPersistence for FailingCheckpointRows {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        let target = match &operation {
            RelationalOperation::ApplyMutations { mutations, .. } => mutations.iter().any(|mutation| matches!(mutation,
                PluginDataMutation::Put { row_key, value, .. } if row_key.ends_with("/header") &&
                if self.fail_seal { value["pending"]["ready"] == true } else { value["pending"].is_null() && value["last_acknowledged_operation"] == "edit" })),
            _ => false,
        };
        let fails = target && self.armed.swap(false, Ordering::AcqRel);
        if fails && self.fail_seal {
            return Err(DbError::invalid_value(
                "interrupted preparation",
                "recovery of an unpublished draft",
            ));
        }
        let result = RelationalPersistence::execute(&*self.local, operation, control)?;
        if fails {
            return Err(DbError::invalid_value(
                "lost acknowledgement",
                "read-back before retry",
            ));
        }
        Ok(result)
    }

    fn readiness(&self) -> PersistenceResult<RelationalReadiness> {
        RelationalPersistence::readiness(&*self.local)
    }
}

fn failing_journal(fixture: &CheckoutFixture, fail_seal: bool) -> IndexPublicationJournal {
    let relational = Arc::new(FailingCheckpointRows {
        local: fixture.local.clone(),
        fail_seal,
        armed: AtomicBool::new(true),
    });
    IndexPublicationJournal::new(
        fixture.local.clone(),
        relational,
        fixture.binding("A"),
        ROOT.into(),
    )
    .unwrap()
}

#[test]
fn incomplete_preparation_is_never_publishable_and_can_be_recovered() {
    let fixture = CheckoutFixture::new();
    fixture.bootstrap();
    let journal = failing_journal(&fixture, true);
    let edit = fixture.publication(&journal, "edit", &[("a", "A2")], false);
    let before = journal.inventory(&control()).unwrap();
    assert!(journal.freeze(&edit, &control()).is_err());
    assert!(fixture.publish(&journal).is_err());
    journal.discard_unpublished_draft(&control()).unwrap();
    assert!(journal.pending(&control()).unwrap().is_none());
    assert_eq!(journal.inventory(&control()).unwrap(), before);
    journal.freeze(&edit, &control()).unwrap();
    let receipt = accepted(fixture.publish(&journal).unwrap());
    journal.acknowledge(&receipt, &control()).unwrap();
}

#[test]
fn a_lost_relational_acknowledgement_retries_without_reapplying_the_scope() {
    let fixture = CheckoutFixture::new();
    fixture.bootstrap();
    let journal = failing_journal(&fixture, false);
    let edit = fixture.publication(&journal, "edit", &[("a", "A2")], false);
    journal.freeze(&edit, &control()).unwrap();
    let receipt = accepted(fixture.publish(&journal).unwrap());
    assert!(journal.acknowledge(&receipt, &control()).is_err());
    let after = journal.inventory(&control()).unwrap();
    journal.acknowledge(&receipt, &control()).unwrap();
    assert_eq!(journal.inventory(&control()).unwrap(), after);
    assert!(journal.pending(&control()).unwrap().is_none());
}
