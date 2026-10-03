//! Owns source scanning and reusable parsed-file state for the MCP project provider.
//! Callers use `ProjectIndexer::prepare` and acknowledge successful publication with
//! `ProjectIndexer::commit`. Source/parser adapters implement the crate-root traits;
//! cached state, delta planning and invalidation remain private. No database backend
//! or authoritative semantic state belongs here.
//! Production adapters use `ProjectIndexingSession::update` to serialize publication
//! with cache acknowledgment, including across caller cancellation.

mod documents;
mod filesystem;
mod fingerprints;
mod parallel_parser;
mod parser;
mod projection;
mod references;
mod scan;
mod session;
mod source;

pub use documents::{
    ConvertedDocument, DocumentConversionOptions, DocumentConverter, DocumentFigure, DocumentImage,
    DocumentPage, DocumentProvenance, convert_document, is_document_path,
};
pub use filesystem::FilesystemProjectSource;
pub use fingerprints::SourceFingerprint;
pub use parallel_parser::ParallelTreeSitterProjectParser;
pub use parser::{BlockSettings, SyntaxParserMetrics, TreeSitterProjectParser};
pub use projection::{ProjectionMetrics, SemanticIndexProjection};
pub use references::{DefinitionSite, ReferenceFileUpdate, ReferenceTarget, ResolvedReference};

pub use scan::{PreparedProjectScan, ProjectIndexer, ScanMetrics, ScannedFile};
pub use session::{ProjectIndexingSession, ProjectUpdateReport, SemanticIndexPublisher};
pub use source::{
    FileRead, FileStamp, IndexedDefinition, IndexedReference, ParseStatus, ParsedFile,
    ProjectFileParser, ProjectSource, ReferenceQualifier, ScanError, ScanScope, SourceEntry,
    SourceKind, SourceParseInput, SourceSpan,
};
