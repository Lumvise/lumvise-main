//! Source and parser interfaces keep filesystem and language engines out of delta policy.

/// Failure includes the source location and the expected invariant.
#[derive(Debug, thiserror::Error)]
#[error("project scan `{path}` failed: {reason}")]
pub struct ScanError {
    /// Offending project-relative path or state identifier.
    pub path: String,
    /// The failure and expected shape or state.
    pub reason: String,
}

/// Selects inventory work. Example: `ScanScope::Paths(vec!["src/main.rs".into()])`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanScope {
    /// Inventory all accepted paths, including deletions since publication.
    Full,
    /// Inventory only these canonical relative paths/subtrees and their ancestors.
    Paths(Vec<String>),
}

/// Filesystem freshness evidence; adapters must change this when file contents can
/// have changed. `None` on SourceEntry disables the metadata shortcut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp(pub Vec<u8>);

/// Kind of source entry. Example: a directory has no bytes to parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// A directory supplies structural parent context.
    Directory,
    /// A regular file supplies content and optional language structure.
    File,
}

/// One accepted source entry. Example: path `src/main.rs`, kind `File`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEntry {
    /// Canonical project-relative path using `/` separators.
    pub path: String,
    /// Whether reading content is necessary.
    pub kind: SourceKind,
    /// Reliable freshness evidence, or None to require a content check.
    pub stamp: Option<FileStamp>,
}

/// One coherent file read. Example: a filesystem adapter validates metadata before
/// and after reading an open file, returning an error if it cannot obtain stability.
pub struct FileRead {
    /// Complete bytes from one stable read.
    pub bytes: Vec<u8>,
    /// Freshness evidence corresponding to these exact bytes.
    pub stamp: Option<FileStamp>,
}

/// Project-scoped filesystem boundary. Inventory must be complete for the requested
/// scope or fail; unreadable paths must never silently become deletions. Ignore-rule removals are explicit inventory policy.
pub trait ProjectSource {
    /// Returns accepted files/directories in scope plus required ancestors.
    /// Example: `source.inventory(&ScanScope::Full)` before an initial scan.
    fn inventory(&self, scope: &ScanScope) -> Result<Vec<SourceEntry>, ScanError>;
    /// Reads one stable file without a separate preflight content pass.
    /// Example: `source.read_file("src/main.rs")`.
    fn read_file(&self, path: &str) -> Result<FileRead, ScanError>;
}

/// Source-local definition; semantic identities are assigned during publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedDefinition {
    /// Parser-provided declaration kind, such as function or class.
    pub kind: String,
    /// Declaration name, preserving source case.
    pub name: String,
    /// Inclusive, one-based range.
    pub start_line: usize,
    /// Inclusive, one-based range end.
    pub end_line: usize,
    /// Declaration bytes in ParsedFile::source; no duplicate body allocation.
    pub span: SourceSpan,
    /// Logical container outside lexical nesting, such as a Rust impl target.
    pub implementation_type: Option<String>,
}

/// Unresolved source reference retained for incremental cross-file resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedReference {
    /// Referenced source name.
    pub name: String,
    /// One-based reference line used to resolve its owning declaration.
    pub line: usize,
    /// Relationship intent, such as calls or imports.
    pub kind: String,
    /// Name bytes distinguish references sharing a source line.
    pub span: SourceSpan,
    /// Syntactic scope or receiver; never discarded to guess a bare-name target.
    pub qualifier: ReferenceQualifier,
}

/// Evidence available without compiler type inference. Example: `Scope("Vec".into())`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ReferenceQualifier {
    /// A bare identifier may use lexical/project name candidates.
    #[default]
    Unqualified,
    /// Bare identifier inside a type; C# and C++ may call its members implicitly.
    UnqualifiedInType(String),
    /// Explicit type or module path.
    Scope(String),
    /// `self`, `Self`, or `this` with a known enclosing type.
    SelfScope(String),
    /// Receiver expression whose type is unknown; keep unresolved.
    Receiver(String),
}

/// Half-open UTF-8 byte range. Example: `source.get(span.start..span.end)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceSpan {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

/// Extraction coverage is explicit so unsupported files cannot masquerade as
/// successfully parsed empty code. Example: inspect before publishing declarations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ParseStatus {
    /// No language adapter handled this file.
    #[default]
    Unsupported,
    /// Generic text fallback split the content into block definitions.
    PlainText,
    /// Non-text bytes are retained only by their file fingerprint.
    Binary,
    /// A language grammar ran, possibly recovering from syntax errors.
    Parsed {
        /// Selected language grammar.
        language: String,
        /// True when the recovered tree contains missing/error nodes.
        has_syntax_errors: bool,
    },
}

/// Reusable source parsing result. Example: unsupported binary files have empty
/// definitions/references while their file record and fingerprint still update.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedFile {
    /// Mapping from derived Markdown to the original document.
    pub document: Option<crate::DocumentProvenance>,
    /// A failed conversion remains visible without aborting unrelated files.
    pub conversion_error: Option<String>,
    /// Source declarations.
    pub definitions: Vec<IndexedDefinition>,
    /// References remain unresolved until the project symbol set is available.
    pub references: Vec<IndexedReference>,
    /// One shared text body supports later content projection without rereading.
    pub source: Option<std::sync::Arc<str>>,
    /// Whether and how the source was parsed.
    pub status: ParseStatus,
}

/// Already-read input borrowed for one parser batch. Example: a worker consumes
/// `input.bytes` without reopening `input.path`.
pub struct SourceParseInput<'a> {
    /// Canonical project-relative source path used for language selection.
    pub path: &'a str,
    /// Bytes from the source adapter's stable read.
    pub bytes: &'a [u8],
}

/// Injected language extraction boundary; implementations reuse parser/query state.
pub trait ProjectFileParser {
    /// Parses bytes already read by the source adapter. Example: parse a changed
    /// `src/main.rs` without opening the file again.
    fn parse(&mut self, path: &str, bytes: &[u8]) -> Result<ParsedFile, ScanError>;

    /// Preferred number of inventory entries prepared at once, bounding temporary
    /// source buffers. Example: sequential parsers keep one entry in flight.
    fn batch_size(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::MIN
    }

    /// Returns exactly one result per input, in input order. Example: a parallel
    /// parser may complete work out of order but must preserve this result order.
    fn parse_batch(
        &mut self,
        inputs: &[SourceParseInput<'_>],
    ) -> Result<Vec<ParsedFile>, ScanError> {
        inputs
            .iter()
            .map(|input| self.parse(input.path, input.bytes))
            .collect()
    }
}

pub(crate) fn invalid(path: impl Into<String>, reason: impl Into<String>) -> ScanError {
    ScanError {
        path: path.into(),
        reason: reason.into(),
    }
}

pub(crate) fn validate_path(path: &str) -> Result<(), ScanError> {
    if path.is_empty()
        || path.contains('\\')
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(invalid(
            path,
            "expected canonical relative path without empty, . or .. components",
        ));
    }
    Ok(())
}
