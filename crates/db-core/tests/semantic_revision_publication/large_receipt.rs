//! Trusted revision receipts keep the same format beyond Grafeo's default property cap.
use super::*;

#[test]
fn large_revision_receipt_survives_database_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let local = LocalPersistence::open(directory.path().join("large-receipt.db")).unwrap();
    let mut request = edit(
        &local,
        "large-receipt",
        vec![element("large", "large.rs", 1)],
    );
    request.source_anchor = Some(SemanticSourceAnchor {
        revision: "r".repeat(16 * 1024 * 1024 + 1),
        canonical_remote: None,
        dirty: false,
    });
    let receipt = publish(&local, request);
    assert!(serde_json::to_vec(&receipt).unwrap().len() > 16 * 1024 * 1024);
    let before = selection(&local).unwrap().selection;
    drop(local);
    let reopened = LocalPersistence::open(directory.path().join("large-receipt.db")).unwrap();
    assert!(selection(&reopened).unwrap().selection == before);
}
