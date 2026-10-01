//! Owns scan/publication ordering. Transport adapters implement only the crate-root
//! publisher contract; pending publication and acknowledgment remain internal.
use crate::{
    PreparedProjectScan, ProjectFileParser, ProjectIndexer, ProjectSource, ScanError, ScanMetrics,
    ScanScope, SemanticIndexProjection,
};
use lumvise_contracts::IndexBatchRequest;
use std::{future::Future, pin::Pin};

/// Publishes through the semantic ingestion boundary without exposing persistence.
/// Example: an MCP adapter returns an owned future using a cloned transport client.
pub trait SemanticIndexPublisher {
    /// Completes only when ingestion acknowledges success or failure. The owned
    /// future lets a session retain the operation after its caller cancels.
    /// Example: `async move { client.ingest(batch).await }`.
    fn publish(
        &mut self,
        batch: IndexBatchRequest,
    ) -> impl Future<Output = Result<(), ScanError>> + Send + 'static;
}

/// Work for the requested scan; an older pending publication is completed first.
/// Example: `report.published == false` means this scan needed no graph write.
#[derive(Debug, PartialEq, Eq)]
pub struct ProjectUpdateReport {
    /// Whether the requested scan published semantic changes.
    pub published: bool,
    /// Source work performed for the requested scope.
    pub scan: ScanMetrics,
}

struct PendingPublication {
    scan: PreparedProjectScan,
    completion: Pin<Box<dyn Future<Output = Result<(), ScanError>> + Send>>,
}

/// One project's serialized scan and publication lifetime. Keep this session alive
/// across requests; dropping a request future retains its pending publication.
/// Example: `session.update(ScanScope::Full).await?` establishes the baseline.
pub struct ProjectIndexingSession<S, P, W> {
    indexer: ProjectIndexer<S, P>,
    projection: SemanticIndexProjection,
    publisher: W,
    pending: Option<PendingPublication>,
}

impl<S: ProjectSource, P: ProjectFileParser, W: SemanticIndexPublisher>
    ProjectIndexingSession<S, P, W>
{
    /// Injects a source, parser, projection for the same project, and publisher.
    /// Example: `ProjectIndexingSession::new(source, parser, projection, publisher)`.
    pub fn new(source: S, parser: P, projection: SemanticIndexProjection, publisher: W) -> Self {
        Self {
            indexer: ProjectIndexer::new(source, parser),
            projection,
            publisher,
            pending: None,
        }
    }

    /// Finishes any previous publication before scanning the requested scope.
    /// Source work is synchronous: async runtimes should run sessions on a worker.
    /// Example: `session.update(ScanScope::Paths(vec!["src/lib.rs".into()])).await?`.
    pub async fn update(&mut self, scope: ScanScope) -> Result<ProjectUpdateReport, ScanError> {
        self.finish_publication().await?;
        let scan = self.indexer.prepare(scope)?;
        let report = ProjectUpdateReport {
            published: scan.needs_publication(),
            scan: scan.metrics().clone(),
        };
        if !report.published {
            self.indexer.commit(scan)?;
            return Ok(report);
        }
        let batch = self.projection.project(&scan)?;
        self.pending = Some(PendingPublication {
            scan,
            completion: Box::pin(self.publisher.publish(batch)),
        });
        self.finish_publication().await?;
        Ok(report)
    }

    async fn finish_publication(&mut self) -> Result<(), ScanError> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(());
        };
        // Await in place: cancellation cannot discard an unacknowledged write and
        // let a newer batch overtake it on the next request.
        let result = pending.completion.as_mut().await;
        let finished = self
            .pending
            .take()
            .expect("pending publication was retained");
        result?;
        self.indexer.commit(finished.scan)
    }
}
