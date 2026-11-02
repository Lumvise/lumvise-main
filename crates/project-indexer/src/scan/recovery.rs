//! Explicit recovery scanning stays on the ordinary source/parser/commit lifecycle.
use super::*;

impl<S: ProjectSource, P: ProjectFileParser> ProjectIndexer<S, P> {
    /// Rechecks acknowledged lost paths while retaining ordinary failed-path protection.
    /// Example: `indexer.prepare_resilient_invalidating(ScanScope::Full, &paths, |_| Ok(()))?`.
    pub fn prepare_resilient_invalidating(
        &mut self,
        scope: ScanScope,
        invalidated_paths: &BTreeSet<String>,
        progress: impl Fn(&ScanMetrics) -> Result<(), ScanError>,
    ) -> Result<PreparedProjectScan, ScanError> {
        self.prepare_scan_invalidating(scope, invalidated_paths, progress)
    }
}
