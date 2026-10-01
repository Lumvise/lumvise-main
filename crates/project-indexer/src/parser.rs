//! Owns reusable syntax extraction. Query assets and Tree-sitter types stay private;
//! callers inject TreeSitterProjectParser through ProjectFileParser. Cross-file name
//! resolution and semantic identity assignment belong to subsequent scan stages.

mod captures;
mod languages;
mod qualifiers;

use crate::source::invalid;
use crate::{IndexedDefinition, ParseStatus, ParsedFile, ProjectFileParser, ScanError};
use languages::SyntaxLanguage;
use std::collections::BTreeMap;
use std::sync::Arc;
use tree_sitter::{Parser, Query, QueryCursor};

/// Bounds for the generic text fallback that indexes every unsupported text
/// file as blank-line-separated block elements.
/// Example: `TreeSitterProjectParser::default().with_block_settings(settings)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSettings {
    /// Stops block children after this count; the file element stays complete.
    /// Example: a settings file with 10 000 blocks indexes only the first 50.
    pub max_blocks_per_file: usize,
    /// Files longer than this line count keep only their file element, avoiding
    /// junk children on generated or minified text.
    pub max_file_lines: usize,
}

impl Default for BlockSettings {
    fn default() -> Self {
        Self {
            max_blocks_per_file: 50,
            max_file_lines: 5000,
        }
    }
}

/// Actual parser work, separate from filesystem reads. Example: unchanged scans
/// leave both counters unchanged when this adapter is injected into ProjectIndexer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyntaxParserMetrics {
    /// Language parser/query pairs compiled once for this adapter.
    pub languages_initialized: usize,
    /// Syntax trees built, exactly one per supported textual file invocation.
    pub trees_parsed: usize,
}

/// Cached language parsers and queries, owned by a scan worker rather than globals.
/// Example: `ProjectIndexer::new(source, TreeSitterProjectParser::default())`.
#[derive(Default)]
pub struct TreeSitterProjectParser {
    engines: BTreeMap<SyntaxLanguage, SyntaxEngine>,
    metrics: SyntaxParserMetrics,
    blocks: BlockSettings,
}

impl TreeSitterProjectParser {
    /// Overrides the generic text fallback bounds; other fields stay default.
    /// Example: `TreeSitterProjectParser::default().with_block_settings(settings)`.
    pub fn with_block_settings(mut self, blocks: BlockSettings) -> Self {
        self.blocks = blocks;
        self
    }

    /// Reports actual initialization/parse counts. Example: `parser.metrics().trees_parsed`.
    pub fn metrics(&self) -> &SyntaxParserMetrics {
        &self.metrics
    }

    fn parse_document(
        &mut self,
        path: &str,
        document: crate::ConvertedDocument,
    ) -> Result<ParsedFile, ScanError> {
        let mut parsed = self.engine(SyntaxLanguage::Markdown)?.extract(
            path,
            &document.markdown,
            SyntaxLanguage::Markdown,
        )?;
        let heading_starts: Vec<_> = parsed
            .definitions
            .iter()
            .map(|definition| definition.span.start)
            .collect();
        crate::documents::extend_sections(&document.markdown, &mut parsed.definitions);
        // Plain paragraphs and tables are meaningful even when a converter emits no headings.
        parsed.definitions.extend(
            text_blocks(&document.markdown, &self.blocks)
                .into_iter()
                .filter(|block| !heading_starts.contains(&block.span.start)),
        );
        for definition in &mut parsed.definitions {
            definition.kind = "markdown_section".into();
        }
        parsed.document = Some(document.provenance);
        self.metrics.trees_parsed += 1;
        Ok(parsed)
    }

    fn engine(&mut self, language: SyntaxLanguage) -> Result<&mut SyntaxEngine, ScanError> {
        match self.engines.entry(language) {
            std::collections::btree_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let engine = SyntaxEngine::new(language)?;
                self.metrics.languages_initialized += 1;
                Ok(entry.insert(engine))
            }
        }
    }
}

impl ProjectFileParser for TreeSitterProjectParser {
    fn parse(&mut self, path: &str, bytes: &[u8]) -> Result<ParsedFile, ScanError> {
        if SyntaxLanguage::for_path(path).is_none() {
            match crate::convert_document(path, bytes) {
                Ok(Some(document)) => return self.parse_document(path, document),
                Err(error) => {
                    return Ok(ParsedFile {
                        conversion_error: Some(error.to_string()),
                        status: ParseStatus::Unsupported,
                        ..ParsedFile::default()
                    });
                }
                Ok(None) => {}
            }
        }

        let text = match std::str::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => text,
            _ => {
                return Ok(ParsedFile {
                    status: ParseStatus::Binary,
                    ..ParsedFile::default()
                });
            }
        };
        let Some(language) = SyntaxLanguage::for_path(path) else {
            let definitions = text_blocks(text, &self.blocks);
            let status = if definitions.is_empty() {
                ParseStatus::Unsupported
            } else {
                ParseStatus::PlainText
            };
            return Ok(ParsedFile {
                source: Some(Arc::from(text)),
                definitions,
                status,
                ..ParsedFile::default()
            });
        };
        let parsed = self.engine(language)?.extract(path, text, language)?;
        self.metrics.trees_parsed += 1;
        Ok(parsed)
    }
}

/// Splits any text into blank-line-separated block definitions so every text
/// extension yields content-addressable children, bounded by `settings`.
fn text_blocks(text: &str, settings: &BlockSettings) -> Vec<IndexedDefinition> {
    if text.lines().count() > settings.max_file_lines {
        return Vec::new();
    }
    let mut blocks = Vec::new();
    let mut start: Option<(usize, usize, String)> = None; // (byte, line, name)
    let mut offset = 0usize;
    let mut content_end = 0usize;
    let mut line = 0usize;
    for raw in text.split_inclusive('\n') {
        let trimmed = raw.trim_end_matches('\n');
        if trimmed.trim().is_empty() {
            if let Some((begin, begin_line, name)) = start.take() {
                blocks.push(block_definition(
                    "block",
                    &name,
                    begin,
                    begin_line,
                    content_end,
                    line,
                ));
            }
        } else {
            if start.is_none() {
                let name: String = trimmed.trim().chars().take(80).collect();
                start = Some((offset, line, name));
            }
            content_end = offset + trimmed.len();
        }
        offset += raw.len();
        line += 1;
    }
    if let Some((begin, begin_line, name)) = start.take() {
        blocks.push(block_definition(
            "block",
            &name,
            begin,
            begin_line,
            content_end,
            line,
        ));
    }
    blocks.truncate(settings.max_blocks_per_file);
    blocks
        .into_iter()
        .filter(|block| !block.name.is_empty())
        .collect()
}

fn block_definition(
    kind: &str,
    name: &str,
    start: usize,
    start_line: usize,
    end: usize,
    end_line: usize,
) -> IndexedDefinition {
    IndexedDefinition {
        kind: kind.into(),
        name: name.into(),
        start_line: start_line + 1,
        end_line: end_line.max(start_line + 1),
        span: crate::SourceSpan { start, end },
        implementation_type: None,
    }
}

struct SyntaxEngine {
    parser: Parser,
    query: Query,
    cursor: QueryCursor,
}

impl SyntaxEngine {
    fn new(language: SyntaxLanguage) -> Result<Self, ScanError> {
        let grammar = language.grammar();
        let mut parser = Parser::new();
        parser.set_language(&grammar).map_err(|error| {
            invalid(
                language.name(),
                format!("expected compatible grammar: {error}"),
            )
        })?;
        let query = Query::new(&grammar, language.query()).map_err(|error| {
            invalid(
                language.name(),
                format!("expected valid embedded query: {error}"),
            )
        })?;
        Ok(Self {
            parser,
            query,
            cursor: QueryCursor::new(),
        })
    }

    fn extract(
        &mut self,
        path: &str,
        text: &str,
        language: SyntaxLanguage,
    ) -> Result<ParsedFile, ScanError> {
        let tree = self
            .parser
            .parse(text, None)
            .ok_or_else(|| invalid(path, "expected completed syntax tree"))?;
        let mut parsed = captures::extract(&self.query, &mut self.cursor, &tree, text);
        if self.cursor.did_exceed_match_limit() {
            return Err(invalid(
                path,
                "expected complete query extraction; match limit exceeded",
            ));
        }
        parsed.source = Some(Arc::from(text));
        parsed.status = ParseStatus::Parsed {
            language: language.name().into(),
            has_syntax_errors: tree.root_node().has_error(),
        };
        Ok(parsed)
    }
}
