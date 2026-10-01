use super::{ChangeBuffer, ChangeDisposition, DirtyElement};
use std::time::Instant;

fn dirty(id: &str, revision: i64) -> DirtyElement {
    DirtyElement {
        element_id: id.into(),
        project_root: "/buffer-benchmark".into(),
        entity_kind: "function".into(),
        revision,
        disposition: ChangeDisposition::Upserted,
    }
}

#[test]
fn overflow_retains_newest_revisions_with_deterministic_id_ties() {
    let mut buffer = ChangeBuffer::with_capacity(0, 2);
    buffer.publish(
        5,
        vec![dirty("z", 1), dirty("c", 5), dirty("b", 5), dirty("a", 5)],
    );
    let snapshot = buffer.snapshot();
    let retained = snapshot
        .entries
        .iter()
        .map(|entry| entry.element_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(retained, ["b", "c"]);
    assert_eq!((snapshot.complete_after, snapshot.head_revision), (5, 5));
}

#[test]
fn replacement_inside_one_batch_does_not_evict_or_advance_overflow_floor() {
    let mut buffer = ChangeBuffer::with_capacity(0, 2);
    buffer.publish(7, vec![dirty("a", 1), dirty("b", 2), dirty("a", 7)]);
    let snapshot = buffer.snapshot();
    assert_eq!(snapshot.complete_after, 0);
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| entry.revision)
            .collect::<Vec<_>>(),
        [7, 2]
    );
    buffer.publish(8, vec![dirty("c", 8)]);
    assert_eq!(buffer.snapshot().complete_after, 2);
    assert_eq!(
        buffer
            .entries
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["a", "c"]
    );
}

#[test]
fn zero_capacity_keeps_no_entries_and_preserves_the_greatest_overflow_revision() {
    let mut buffer = ChangeBuffer::with_capacity(10, 0);
    buffer.publish(12, vec![dirty("a", 12), dirty("b", 11)]);
    buffer.publish(9, vec![dirty("c", 9)]);
    let snapshot = buffer.snapshot();
    assert!(snapshot.entries.is_empty());
    assert_eq!((snapshot.complete_after, snapshot.head_revision), (12, 12));
}

#[test]
#[ignore = "release performance evidence; run explicitly with --ignored --nocapture"]
fn change_buffer_bulk_publication_samples() {
    for count in [1_000, 10_000, 35_000] {
        for sample in 0..3 {
            let changes = (0..count)
                .map(|index| dirty(&format!("element-{index:06}"), 1))
                .collect();
            let mut buffer = ChangeBuffer::new(0);
            let started = Instant::now();
            buffer.publish(1, changes);
            let elapsed_ns = started.elapsed().as_nanos();
            assert_eq!(buffer.entries.len(), count.min(1_024));
            eprintln!(
                "{{\"workload\":\"change_buffer_bulk\",\"elements\":{count},\"sample\":{sample},\"elapsed_ns\":{elapsed_ns}}}"
            );
        }
    }
}
