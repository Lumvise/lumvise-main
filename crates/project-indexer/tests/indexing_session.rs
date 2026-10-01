use lumvise_contracts::IndexBatchRequest;
use lumvise_project_indexer::{
    FileRead, FileStamp, ProjectIndexingSession, ProjectSource, ScanError, ScanScope,
    SemanticIndexProjection, SemanticIndexPublisher, SourceEntry, SourceKind,
    TreeSitterProjectParser,
};
use std::{
    collections::BTreeMap,
    future::Future,
    path::Path,
    pin::{Pin, pin},
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct SourceFixture {
    files: BTreeMap<String, (u8, String)>,
    inventories: usize,
}

#[derive(Clone, Default)]
struct FakeSessionSource(Arc<Mutex<SourceFixture>>);

impl FakeSessionSource {
    fn put(&self, stamp: u8, text: &str) {
        self.0
            .lock()
            .unwrap()
            .files
            .insert("lib.rs".into(), (stamp, text.into()));
    }
}

impl ProjectSource for FakeSessionSource {
    fn inventory(&self, _: &ScanScope) -> Result<Vec<SourceEntry>, ScanError> {
        let mut source = self.0.lock().unwrap();
        source.inventories += 1;
        Ok(source
            .files
            .iter()
            .map(|(path, (stamp, _))| SourceEntry {
                path: path.clone(),
                kind: SourceKind::File,
                stamp: Some(FileStamp(vec![*stamp])),
            })
            .collect())
    }

    fn read_file(&self, path: &str) -> Result<FileRead, ScanError> {
        let source = self.0.lock().unwrap();
        let (stamp, text) = &source.files[path];
        Ok(FileRead {
            bytes: text.as_bytes().to_vec(),
            stamp: Some(FileStamp(vec![*stamp])),
        })
    }
}

struct PublicationFixture {
    batches: Vec<IndexBatchRequest>,
    outcomes: Vec<Option<bool>>,
    next_outcome: Option<bool>,
}

#[derive(Clone)]
struct FakeSemanticPublisher(Arc<Mutex<PublicationFixture>>);

impl Default for FakeSemanticPublisher {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(PublicationFixture {
            batches: vec![],
            outcomes: vec![],
            next_outcome: Some(true),
        })))
    }
}

struct PublicationGate {
    fixture: Arc<Mutex<PublicationFixture>>,
    index: usize,
}

impl Future for PublicationGate {
    type Output = Result<(), ScanError>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        match self.fixture.lock().unwrap().outcomes[self.index] {
            None => Poll::Pending,
            Some(true) => Poll::Ready(Ok(())),
            Some(false) => Poll::Ready(Err(ScanError {
                path: format!("publication {}", self.index),
                reason: "rejected batch; expected successful ingestion".into(),
            })),
        }
    }
}

impl SemanticIndexPublisher for FakeSemanticPublisher {
    fn publish(
        &mut self,
        batch: IndexBatchRequest,
    ) -> impl Future<Output = Result<(), ScanError>> + Send + 'static {
        let mut fixture = self.0.lock().unwrap();
        let index = fixture.batches.len();
        let outcome = fixture.next_outcome;
        fixture.batches.push(batch);
        fixture.outcomes.push(outcome);
        PublicationGate {
            fixture: Arc::clone(&self.0),
            index,
        }
    }
}

fn session(
    source: FakeSessionSource,
    publisher: FakeSemanticPublisher,
) -> ProjectIndexingSession<FakeSessionSource, TreeSitterProjectParser, FakeSemanticPublisher> {
    ProjectIndexingSession::new(
        source,
        TreeSitterProjectParser::default(),
        SemanticIndexProjection::new(Path::new("/workspace/project"), "mcp-test").unwrap(),
        publisher,
    )
}

fn ready<T>(future: impl Future<Output = T>) -> T {
    match pin!(future)
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("expected immediately completed fake publication"),
    }
}

#[test]
fn failed_publication_retries_complete_batch_before_acknowledging_cache() {
    let source = FakeSessionSource::default();
    source.put(1, "fn original() {}");
    let publisher = FakeSemanticPublisher::default();
    publisher.0.lock().unwrap().next_outcome = Some(false);
    let mut session = session(source, publisher.clone());
    assert!(ready(session.update(ScanScope::Full)).is_err());
    publisher.0.lock().unwrap().next_outcome = Some(true);
    let retry = ready(session.update(ScanScope::Full)).unwrap();
    assert!(retry.published);
    let fixture = publisher.0.lock().unwrap();
    assert_eq!(fixture.batches.len(), 2);
    assert_eq!(fixture.batches[0], fixture.batches[1]);
    drop(fixture);
    assert!(!ready(session.update(ScanScope::Full)).unwrap().published);
}

#[test]
fn cancelled_request_retains_write_and_prevents_newer_scan_overtaking_it() {
    let source = FakeSessionSource::default();
    source.put(1, "fn original() {}");
    let publisher = FakeSemanticPublisher::default();
    publisher.0.lock().unwrap().next_outcome = None;
    let mut session = session(source.clone(), publisher.clone());
    let mut first = Box::pin(session.update(ScanScope::Full));
    let mut context = Context::from_waker(Waker::noop());
    assert!(first.as_mut().poll(&mut context).is_pending());
    drop(first);
    source.put(2, "fn changed() {}");
    let mut second = Box::pin(session.update(ScanScope::Full));
    assert!(second.as_mut().poll(&mut context).is_pending());
    assert_eq!(source.0.lock().unwrap().inventories, 1);
    {
        let mut fixture = publisher.0.lock().unwrap();
        assert_eq!(fixture.batches.len(), 1);
        fixture.outcomes[0] = Some(true);
        fixture.next_outcome = Some(true);
    }
    assert!(ready(second).unwrap().published);
    let fixture = publisher.0.lock().unwrap();
    assert_eq!(fixture.batches.len(), 2);
    assert!(fixture.batches[0].replace_paths.is_empty());
    assert_eq!(fixture.batches[1].replace_paths, ["lib.rs"]);
    assert_eq!(
        fixture.batches[1].semantic_elements[1].semantic_element_name,
        "changed"
    );
    drop(fixture);
    assert!(!ready(session.update(ScanScope::Full)).unwrap().published);
}

#[test]
fn rejected_retained_publication_leaves_initial_snapshot_retryable() {
    let source = FakeSessionSource::default();
    source.put(1, "fn original() {}");
    let publisher = FakeSemanticPublisher::default();
    publisher.0.lock().unwrap().next_outcome = None;
    let mut session = session(source.clone(), publisher.clone());
    let mut request = Box::pin(session.update(ScanScope::Full));
    assert!(
        request
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(request);
    publisher.0.lock().unwrap().outcomes[0] = Some(false);
    assert!(ready(session.update(ScanScope::Full)).is_err());
    assert_eq!(source.0.lock().unwrap().inventories, 1);
    publisher.0.lock().unwrap().next_outcome = Some(true);
    assert!(ready(session.update(ScanScope::Full)).unwrap().published);
    let fixture = publisher.0.lock().unwrap();
    assert_eq!(fixture.batches.len(), 2);
    assert_eq!(fixture.batches[0], fixture.batches[1]);
}

#[test]
fn empty_baseline_publishes_once_and_metadata_only_changes_are_acknowledged() {
    let source = FakeSessionSource::default();
    let publisher = FakeSemanticPublisher::default();
    let mut session = session(source.clone(), publisher.clone());
    assert!(ready(session.update(ScanScope::Full)).unwrap().published);
    assert!(!ready(session.update(ScanScope::Full)).unwrap().published);
    source.put(1, "fn original() {}");
    assert!(ready(session.update(ScanScope::Full)).unwrap().published);
    source.put(2, "fn original() {}");
    let metadata = ready(session.update(ScanScope::Full)).unwrap();
    assert!(!metadata.published);
    assert_eq!(
        (metadata.scan.files_read, metadata.scan.files_parsed),
        (1, 0)
    );
    assert_eq!(
        ready(session.update(ScanScope::Full))
            .unwrap()
            .scan
            .files_read,
        0
    );
    assert_eq!(publisher.0.lock().unwrap().batches.len(), 2);
}
