//! Version-two archive pages and bounded policy steps. Adapters store opaque facts; Rust owns decisions.
use super::*;
mod canonical;
mod codec;
mod engine;
mod history;
mod merge;
mod protocol;
mod validate;
use protocol::*;
pub(in crate::policy_bytes) use protocol::{ImportContext, ImportFact};

pub(in crate::policy_bytes::storage) fn page(
    header_json: &str,
    page_json: &str,
) -> SemanticRevisionResult<StorageResult> {
    let header = TransferHeader::parse(header_json)?;
    let page: TransferPage = serde_json::from_str(page_json)?;
    page.validate(&header)?;
    let mut seen = BTreeSet::new();
    let rows = page
        .rows
        .into_iter()
        .map(|row| {
            if !seen.insert(row.key.clone()) {
                return Err(invalid(
                    &row.key.identity,
                    "one source row per archive identity",
                ));
            }
            canonical::source(row, &header.source_root)
        })
        .collect::<SemanticRevisionResult<Vec<_>>>()?;
    encoded("archive_import_page", &serde_json::json!({"rows":rows}))
}

pub(in crate::policy_bytes::storage) fn step(
    context: ImportContext,
    header_json: &str,
    checkpoint_json: Option<&str>,
    facts: Vec<ImportFact>,
) -> SemanticRevisionResult<StorageResult> {
    let header = TransferHeader::parse(header_json)?;
    context.validate(&header)?;
    let mut checkpoint: Checkpoint = checkpoint_json
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    let binding = crate::revision_index::digest(&(&context, &header))?;
    if !checkpoint.binding.is_empty() && checkpoint.binding != binding {
        return Err(invalid(
            &context.operation_id,
            "the same frozen archive, actor, base and timestamp for a validation checkpoint",
        ));
    }
    checkpoint.binding = binding;
    engine::run(context, header, checkpoint, facts)
}

pub(crate) fn encode(archive_json: &str) -> SemanticRevisionResult<Vec<String>> {
    codec::encode(archive_json)
}

fn invalid(value: impl Into<String>, expected: impl AsRef<str>) -> SemanticRevisionError {
    SemanticRevisionError::invalid_value(value, expected.as_ref())
}
