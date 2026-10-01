use lumvise_project_indexer::{
    FileRead, FilesystemProjectSource, ParallelTreeSitterProjectParser, ParsedFile,
    ProjectFileParser, ProjectSource, ScanError, ScanScope, SourceEntry, SourceParseInput,
    TreeSitterProjectParser,
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Default)]
struct MeasuredStages {
    inventory: Duration,
    reads: Duration,
    parses: BTreeMap<String, Duration>,
}

#[derive(Clone, Default)]
pub(super) struct StageCosts(Rc<RefCell<MeasuredStages>>);

impl StageCosts {
    pub(super) fn source(&self, source: FilesystemProjectSource) -> TimedSource {
        TimedSource {
            source,
            costs: self.clone(),
        }
    }

    pub(super) fn parser(&self, workers: usize) -> TimedParser {
        TimedParser {
            parser: if workers == 1 {
                Box::new(TreeSitterProjectParser::default())
            } else {
                Box::new(ParallelTreeSitterProjectParser::new(workers).unwrap())
            },
            costs: self.clone(),
        }
    }

    pub(super) fn report(&self, total_us: u128) -> serde_json::Value {
        let costs = self.0.borrow();
        let parse_time: Duration = costs.parses.values().copied().sum();
        let mut slowest: Vec<_> = costs.parses.iter().collect();
        slowest.sort_by_key(|(_, elapsed)| std::cmp::Reverse(**elapsed));
        let slowest: Vec<_> = slowest.into_iter().take(10)
            .map(|(path, elapsed)| serde_json::json!({"path": path, "parse_us": elapsed.as_micros()})).collect();
        serde_json::json!({"case": "real_repository_stages", "inventory_us": costs.inventory.as_micros(),
            "read_us": costs.reads.as_micros(), "parse_us": parse_time.as_micros(),
            "other_scan_us": total_us.saturating_sub((costs.inventory + costs.reads + parse_time).as_micros()),
            "slowest_parses": slowest})
    }
}

pub(super) struct TimedSource {
    source: FilesystemProjectSource,
    costs: StageCosts,
}

impl ProjectSource for TimedSource {
    fn inventory(&self, scope: &ScanScope) -> Result<Vec<SourceEntry>, ScanError> {
        let started = Instant::now();
        let result = self.source.inventory(scope);
        self.costs.0.borrow_mut().inventory += started.elapsed();
        result
    }

    fn read_file(&self, path: &str) -> Result<FileRead, ScanError> {
        let started = Instant::now();
        let result = self.source.read_file(path);
        self.costs.0.borrow_mut().reads += started.elapsed();
        result
    }
}

pub(super) struct TimedParser {
    parser: Box<dyn ProjectFileParser>,
    costs: StageCosts,
}

impl ProjectFileParser for TimedParser {
    fn batch_size(&self) -> std::num::NonZeroUsize {
        self.parser.batch_size()
    }

    fn parse_batch(
        &mut self,
        inputs: &[SourceParseInput<'_>],
    ) -> Result<Vec<ParsedFile>, ScanError> {
        if inputs.is_empty() {
            return Ok(vec![]);
        }
        if inputs.len() == 1 {
            return self
                .parse(inputs[0].path, inputs[0].bytes)
                .map(|parsed| vec![parsed]);
        }
        let started = Instant::now();
        let result = self.parser.parse_batch(inputs);
        *self
            .costs
            .0
            .borrow_mut()
            .parses
            .entry(format!(
                "{} files starting at {}",
                inputs.len(),
                inputs[0].path
            ))
            .or_default() += started.elapsed();
        result
    }

    fn parse(&mut self, path: &str, bytes: &[u8]) -> Result<ParsedFile, ScanError> {
        let started = Instant::now();
        let result = self.parser.parse(path, bytes);
        *self
            .costs
            .0
            .borrow_mut()
            .parses
            .entry(path.into())
            .or_default() += started.elapsed();
        result
    }
}
