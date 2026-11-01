//! The flag union preserves first ownership/order while large imports remain practical.
use super::*;

#[test]
fn completing_flags_preserves_existing_values_and_first_candidate_order() {
    let existing = SemanticRecordRef::element("existing");
    let inserted = SemanticRecordRef::element("inserted");
    let adopted = SemanticRecordRef::element("adopted");
    let flags = vec![RevisionProjectionFlag {
        record: existing.clone(),
        selected: false,
        lifecycle: Some("retained".into()),
    }];
    let candidates = [&existing, &inserted, &inserted, &adopted, &existing];
    let selected = BTreeSet::from([existing.clone(), inserted.clone()]);
    let complete = RevisionProjection::complete_flags(
        flags,
        candidates.into_iter(),
        &selected,
        &BTreeMap::new(),
    );
    assert_eq!(
        complete.iter().map(|flag| &flag.record).collect::<Vec<_>>(),
        vec![&existing, &inserted, &adopted]
    );
    assert!(!complete[0].selected);
    assert_eq!(complete[0].lifecycle.as_deref(), Some("retained"));
    assert!(complete[1].selected);
    assert!(!complete[2].selected);
}

#[test]
#[ignore = "large-data deadline regression; run explicitly outside the fast suite"]
fn completing_four_hundred_thousand_existing_flags_fits_the_invocation_budget() {
    let records = (0..400_000)
        .map(|index| SemanticRecordRef::element(format!("record-{index:08}")))
        .collect::<Vec<_>>();
    let flags = records
        .iter()
        .map(|record| RevisionProjectionFlag {
            record: record.clone(),
            selected: true,
            lifecycle: Some("active".into()),
        })
        .collect::<Vec<_>>();
    let started = std::time::Instant::now();
    let complete = RevisionProjection::complete_flags(
        flags,
        records.iter(),
        &BTreeSet::new(),
        &BTreeMap::new(),
    );
    let elapsed = started.elapsed();
    assert_eq!(complete.len(), records.len());
    assert!(
        complete
            .iter()
            .all(|flag| flag.selected && flag.lifecycle.as_deref() == Some("active"))
    );
    assert!(
        elapsed < lumvise_resource_routing::InvocationControl::DEADLINE,
        "flag completion took {elapsed:?}"
    );
    eprintln!(
        "{{\"records\":{},\"elapsed_ms\":{}}}",
        records.len(),
        elapsed.as_millis()
    );
}
