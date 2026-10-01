//! Owns reusable syntax workers. All extraction remains in TreeSitterProjectParser;
//! the pool only schedules independent, already-read file bodies.
use crate::source::invalid;
use crate::{
    BlockSettings, ParsedFile, ProjectFileParser, ScanError, SourceParseInput, SyntaxParserMetrics,
    TreeSitterProjectParser,
};
use rayon::prelude::*;
use std::{num::NonZeroUsize, sync::Mutex};

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
                .map(|_| Mutex::new(TreeSitterProjectParser::default().with_block_settings(blocks)))
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
        if inputs.len() <= 1 {
            return inputs
                .iter()
                .map(|input| self.parse(input.path, input.bytes))
                .collect();
        }
        // Collect ordered per-file results before choosing an error; scheduling
        // must not change which source error a caller sees.
        let results: Vec<_> = self.pool.install(|| {
            inputs
                .par_iter()
                .map(|input| {
                    let index =
                        rayon::current_thread_index().expect("installed syntax pool worker");
                    self.parse_on_worker(index, input.path, input.bytes)
                })
                .collect()
        });
        results.into_iter().collect()
    }
}
