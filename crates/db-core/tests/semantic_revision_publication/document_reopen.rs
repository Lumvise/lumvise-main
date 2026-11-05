//! Knowledge-shaped publication survives a cold locator index and a real database reopen.
use super::*;

const DOCUMENT_PATH: &str = "@documents/686f6d65";
const DOCUMENT_LOCATOR: &str = "knowledge-document:home";
const DOCUMENT_ARTIFACT: &str = "knowledge-document-body:home";
const DOCUMENT_CONTENT: &str = "The Home controls remain available after restart.";
const WRITER_DATABASE_ENV: &str = "LUMVISE_DOCUMENT_REOPEN_WRITER_DATABASE";
const WRITER_TEST: &str = "document_reopen::knowledge_document_writer_process";
const WRITER_RECEIPT: &str = "DOCUMENT_REOPEN_RECEIPT=";

#[test]
fn knowledge_document_locator_payload_and_receipt_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("knowledge-document.db");
    let (request, receipt) = save_document_and_unrelated_sources(&path);
    let reopened = LocalPersistence::open(&path).unwrap();
    assert_document_publication(&reopened, &request, &receipt);
    assert_eq!(publish(&reopened, request), receipt);
}

#[test]
fn knowledge_document_public_commit_survives_writer_exit_without_destructors() {
    let directory = tempfile::Builder::new()
        .prefix("lumvise-document-reopen-")
        .tempdir()
        .unwrap();
    let path = directory.path().join("knowledge-document.db");
    let (request, receipt) = run_document_writer(&path);
    let reopened = LocalPersistence::open(&path).unwrap();
    assert_document_publication(&reopened, &request, &receipt);
    assert_eq!(publish(&reopened, request), receipt);
}

#[test]
#[ignore = "launched only by the parent test with an explicit fresh fixture path"]
fn knowledge_document_writer_process() {
    use std::io::Write;
    let path = document_writer_path();
    let local = LocalPersistence::open(&path).unwrap();
    let evidence = publish_document_and_unrelated_sources(&local);
    let encoded = serde_json::to_string(&evidence).unwrap();
    let mut output = std::io::stdout().lock();
    writeln!(output, "{WRITER_RECEIPT}{encoded}").unwrap();
    output.flush().unwrap();
    // Keep LocalPersistence alive until exit so its destructor cannot checkpoint this proof.
    std::process::exit(0);
}

fn document_writer_path() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(
        std::env::var_os(WRITER_DATABASE_ENV).expect("expected an explicit writer fixture path"),
    );
    let valid_parent = path
        .parent()
        .and_then(std::path::Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.starts_with("lumvise-document-reopen-"));
    assert!(
        path.is_absolute() && valid_parent && !path.exists(),
        "fixture path {path:?}; expected a new absolute database inside the unique test directory"
    );
    path
}

fn run_document_writer(
    path: &std::path::Path,
) -> (
    SemanticRevisionPublication,
    SemanticRevisionPublicationReceipt,
) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", WRITER_TEST, "--ignored", "--nocapture"])
        .env(WRITER_DATABASE_ENV, path)
        .output()
        .unwrap();
    assert!(output.status.success(), "writer failed: {output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let encoded = stdout
        .lines()
        .find_map(|line| line.strip_prefix(WRITER_RECEIPT))
        .expect("successful writer must provide its verified public publication receipt");
    serde_json::from_str(encoded).unwrap()
}

fn save_document_and_unrelated_sources(
    path: &std::path::Path,
) -> (
    SemanticRevisionPublication,
    SemanticRevisionPublicationReceipt,
) {
    let local = LocalPersistence::open(path).unwrap();
    publish_document_and_unrelated_sources(&local)
}

fn publish_document_and_unrelated_sources(
    local: &LocalPersistence,
) -> (
    SemanticRevisionPublication,
    SemanticRevisionPublicationReceipt,
) {
    let request = document_publication(local);
    let receipt = publish(local, request.clone());
    assert_document_publication(local, &request, &receipt);
    publish_unrelated_source(local, ROOT);
    publish_unrelated_source(local, "/another-project");
    assert_document_publication(local, &request, &receipt);
    (request, receipt)
}

fn document_publication(local: &LocalPersistence) -> SemanticRevisionPublication {
    let mut locator = element(DOCUMENT_LOCATOR, DOCUMENT_PATH, 1);
    locator.element_kind = "document".into();
    locator.start_line = Some(1);
    locator.end_line = Some(1);
    locator.metadata = json!({"provider_instance_id":"knowledge.documents",
        "indexer_metadata":{"knowledge_document":{"schema":2,"document_id":"home",
            "document_kind":"report","revision":1,"status":"active"}}});
    let mut request = edit(local, "knowledge-document-create", vec![locator]);
    request.structure.as_mut().unwrap().partition = Some(document_partition());
    request.artifacts.push(document_artifact());
    request
}

fn document_partition() -> SemanticPartition {
    SemanticPartition {
        project_root: ROOT.into(),
        provider_instance_id: Some("knowledge.documents".into()),
        replace_paths: vec![DOCUMENT_PATH.into()],
    }
}

fn document_artifact() -> SemanticArtifactRevisionWrite {
    let mut write = evidence_artifact(
        DOCUMENT_ARTIFACT,
        DOCUMENT_CONTENT,
        ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: DOCUMENT_LOCATOR.into(),
        },
    );
    write.artifact.semantic_element_id = DOCUMENT_LOCATOR.into();
    write.artifact.artifact_kind = "document".into();
    write.artifact.metadata = json!({"document":{"kind":"report","revision":1}});
    write
}

fn publish_unrelated_source(local: &LocalPersistence, root: &str) {
    let mut source = element(&format!("source:{root}"), "src/main.rs", 1);
    source.project_root = root.into();
    source.metadata = json!({"provider_instance_id":"builtin-canvas"});
    let mut request = RevisionTestClient::new(local, root, "git:main")
        .publication(&format!("source-import:{root}"), vec![source]);
    request.structure.as_mut().unwrap().partition = Some(SemanticPartition {
        project_root: root.into(),
        provider_instance_id: Some("builtin-canvas".into()),
        replace_paths: vec![],
    });
    publish(local, request);
}

fn assert_document_publication(
    local: &LocalPersistence,
    request: &SemanticRevisionPublication,
    receipt: &SemanticRevisionPublicationReceipt,
) {
    assert_document_location(local, &request.structure.as_ref().unwrap().elements[0]);
    assert_exact_document_records(local);
    assert_document_membership(local);
    let SemanticResult::RevisionIntegrity(report) = apply(
        local,
        SemanticOperation::InspectRevisionIntegrity {
            project_root: ROOT.into(),
            publication_operation_id: Some(request.operation_id.clone()),
        },
    ) else {
        panic!("expected the accepted document publication audit");
    };
    let accepted = report.publication_inspection.unwrap().accepted.unwrap();
    assert_eq!(&accepted.receipt, receipt);
}

fn assert_document_location(local: &LocalPersistence, expected: &SemanticElement) {
    let SemanticResult::ScopedGraph(found) = apply(
        local,
        SemanticOperation::ScopedRead(ScopedSemanticRead::Location {
            project_root: ROOT.into(),
            path: Some(DOCUMENT_PATH.into()),
            line: 1,
            include_inactive: true,
        }),
    ) else {
        panic!("expected the selected Knowledge locator at its document path");
    };
    assert_eq!(found.elements, vec![expected.clone()]);
}

fn assert_exact_document_records(local: &LocalPersistence) {
    let element = document_record(local, SemanticRecordRef::element(DOCUMENT_LOCATOR));
    let SemanticRevisionBody::Element(locator) = element.body else {
        panic!("expected the available exact document locator");
    };
    assert_eq!(locator.semantic_element_id, DOCUMENT_LOCATOR);
    assert_eq!(locator.path, DOCUMENT_PATH);
    let artifact = document_record(local, SemanticRecordRef::artifact(DOCUMENT_ARTIFACT));
    let SemanticRevisionBody::Artifact(body) = artifact.body else {
        panic!("expected the available exact document artifact");
    };
    assert_eq!(body.semantic_element_id, DOCUMENT_LOCATOR);
    assert_eq!(body.content.as_deref(), Some(DOCUMENT_CONTENT));
}

fn document_record(
    local: &LocalPersistence,
    record: SemanticRecordRef,
) -> SemanticRevisionResolution {
    match apply(
        local,
        SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record,
        },
    ) {
        SemanticResult::RevisionResolution(Some(found)) => found,
        other => panic!("{other:?}; expected the exact document record"),
    }
}

fn assert_document_membership(local: &LocalPersistence) {
    let SemanticResult::RevisionObservation(Some(selected)) = apply(
        local,
        SemanticOperation::RevisionSelectionMembers(SemanticRevisionSelectionQuery {
            project_root: ROOT.into(),
            selection_id: "working".into(),
            structure_coverage: SemanticRevisionStructureCoverage::Partition(document_partition()),
            artifact_ids: vec![DOCUMENT_ARTIFACT.into()],
        }),
    ) else {
        panic!("expected the scoped working document membership");
    };
    assert_eq!(
        selected.expected.records,
        vec![
            SemanticRecordRef::element(DOCUMENT_LOCATOR),
            SemanticRecordRef::artifact(DOCUMENT_ARTIFACT),
        ]
    );
}
