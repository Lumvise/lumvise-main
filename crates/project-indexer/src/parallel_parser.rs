//! Owns reusable syntax workers. All extraction remains in TreeSitterProjectParser;
//! the pool schedules independent, already-read code bodies. Documents use
//! bounded scoped threads outside Rayon to prevent nested parser-lock reentry.
use crate::source::invalid;
use crate::{
    BlockSettings, ParsedFile, ProjectFileParser, ScanError, SourceParseInput, SyntaxParserMetrics,
    TreeSitterProjectParser, is_document_path,
};
use rayon::prelude::*;
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

/// Bounded, reusable syntax workers with deterministic output ordering.
/// Example: `ProjectIndexer::new(source, ParallelTreeSitterProjectParser::new(4)?)`.
pub struct ParallelTreeSitterProjectParser {
    pool: rayon::ThreadPool,
    workers: Vec<Mutex<TreeSitterProjectParser>>,
    batch_size: NonZeroUsize,
}

impl ParallelTreeSitterProjectParser {
    /// Starts exactly the requested number of workers; parsers and queries persist
    /// across batches. Example: `ParallelTreeSitterProjectParser::new(4)?`.
    pub fn new(workers: usize) -> Result<Self, ScanError> {
        Self::new_with_settings(workers, BlockSettings::default())
    }

    /// Starts workers with explicit generic text fallback bounds.
    /// Example: `ParallelTreeSitterProjectParser::new_with_settings(4, settings)?`.
    pub fn new_with_settings(workers: usize, blocks: BlockSettings) -> Result<Self, ScanError> {
        Self::new_with_document_converter(workers, blocks, Arc::default())
    }

    /// Starts workers sharing the same document configuration and model lifecycle.
    /// Example: `ParallelTreeSitterProjectParser::new_with_document_converter(4, blocks, converter)?`.
    pub fn new_with_document_converter(
        workers: usize,
        blocks: BlockSettings,
        documents: Arc<crate::DocumentConverter>,
    ) -> Result<Self, ScanError> {
        let batch_size = workers
            .checked_mul(8)
            .and_then(NonZeroUsize::new)
            .ok_or_else(|| {
                invalid(
                    workers.to_string(),
                    "expected positive worker count with representable batch size",
                )
            })?;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|index| format!("semantic-syntax-{index}"))
            .build()
            .map_err(|error| {
                invalid(
                    workers.to_string(),
                    format!("expected available syntax workers: {error}"),
                )
            })?;
        Ok(Self {
            pool,
            workers: (0..workers)
                .map(|_| {
                    Mutex::new(
                        TreeSitterProjectParser::default()
                            .with_block_settings(blocks)
                            .with_document_converter(Arc::clone(&documents)),
                    )
                })
                .collect(),
            batch_size,
        })
    }

    /// Totals actual work across workers. Example: unchanged scans add zero trees.
    pub fn metrics(&self) -> Result<SyntaxParserMetrics, ScanError> {
        let mut metrics = SyntaxParserMetrics::default();
        for (index, worker) in self.workers.iter().enumerate() {
            let parser = worker
                .lock()
                .map_err(|_| invalid(index.to_string(), "expected unpoisoned syntax worker"))?;
            metrics.languages_initialized += parser.metrics().languages_initialized;
            metrics.trees_parsed += parser.metrics().trees_parsed;
        }
        Ok(metrics)
    }

    fn parse_on_worker(
        &self,
        index: usize,
        path: &str,
        bytes: &[u8],
    ) -> Result<ParsedFile, ScanError> {
        self.workers[index]
            .lock()
            .map_err(|_| invalid(index.to_string(), "expected unpoisoned syntax worker"))?
            .parse(path, bytes)
    }

    fn parse_document_inputs(
        &self,
        inputs: &[SourceParseInput<'_>],
    ) -> Vec<Option<Result<ParsedFile, ScanError>>> {
        let documents: Vec<_> = inputs
            .iter()
            .enumerate()
            .filter(|(_, input)| is_document_path(input.path))
            .collect();
        let mut results = vec![None; inputs.len()];
        for (position, result) in self.parse_documents_concurrently(&documents) {
            results[position] = Some(result);
        }
        results
    }

    fn parse_documents_concurrently(
        &self,
        documents: &[(usize, &SourceParseInput<'_>)],
    ) -> Vec<(usize, Result<ParsedFile, ScanError>)> {
        let worker_count = documents.len().min(self.workers.len());
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..worker_count)
                .map(|worker| {
                    scope.spawn(move || self.parse_document_worker(documents, worker, worker_count))
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| {
                    worker
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        })
    }

    fn parse_document_worker(
        &self,
        documents: &[(usize, &SourceParseInput<'_>)],
        worker: usize,
        worker_count: usize,
    ) -> Vec<(usize, Result<ParsedFile, ScanError>)> {
        documents
            .iter()
            .skip(worker)
            .step_by(worker_count)
            .map(|(position, input)| {
                (
                    *position,
                    self.parse_on_worker(worker, input.path, input.bytes),
                )
            })
            .collect()
    }
}

impl ProjectFileParser for ParallelTreeSitterProjectParser {
    fn parse(&mut self, path: &str, bytes: &[u8]) -> Result<ParsedFile, ScanError> {
        self.parse_on_worker(0, path, bytes)
    }

    fn batch_size(&self) -> NonZeroUsize {
        self.batch_size
    }

    fn parse_batch(
        &mut self,
        inputs: &[SourceParseInput<'_>],
    ) -> Result<Vec<ParsedFile>, ScanError> {
        self.parse_batch_results(inputs).into_iter().collect()
    }

    fn parse_batch_results(
        &mut self,
        inputs: &[SourceParseInput<'_>],
    ) -> Vec<Result<ParsedFile, ScanError>> {
        if inputs.len() <= 1 {
            return inputs
                .iter()
                .map(|input| self.parse(input.path, input.bytes))
                .collect();
        }
        // Converters fork Rayon work which can steal a sibling's locked parser.
        // Fixed parser ownership on scoped OS threads prevents that reentry;
        // join all documents before admitting code to the syntax pool.
        let documents = self.parse_document_inputs(inputs);
        // Collect ordered per-file results before choosing an error; scheduling
        // must not change which source error a caller sees.
        let results: Vec<_> = self.pool.install(|| {
            inputs
                .par_iter()
                .zip(documents)
                .map(|(input, document)| {
                    document.unwrap_or_else(|| {
                        let index =
                            rayon::current_thread_index().expect("installed syntax pool worker");
                        self.parse_on_worker(index, input.path, input.bytes)
                    })
                })
                .collect()
        });
        results
    }
}
