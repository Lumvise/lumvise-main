use crate::{ParseStatus, ReferenceFileUpdate, ReferenceTarget, ScannedFile};
use lumvise_contracts::SemanticElementUpsert;
use serde_json::{Value, json};

pub(super) fn extraction(file: &ScannedFile) -> Value {
    let (status, language, syntax_errors) = match &file.parsed.status {
        ParseStatus::Parsed {
            language,
            has_syntax_errors,
        } => ("parsed", Some(language), *has_syntax_errors),
        ParseStatus::PlainText => ("plain_text", None, false),
        ParseStatus::Binary => ("binary", None, false),
        ParseStatus::Unsupported => ("unsupported", None, false),
    };
    json!({"status":status, "language":language, "has_syntax_errors":syntax_errors,
        "definitions":file.parsed.definitions.len(), "references":file.parsed.references.len(),
        "conversion_error":file.parsed.conversion_error, "resolver":"syntax_scope_v1"})
}

pub(super) fn project_resolutions<'a>(
    updates: impl Iterator<Item = &'a ReferenceFileUpdate>,
    elements: &mut [SemanticElementUpsert],
) {
    let mut metadata: std::collections::BTreeMap<_, _> = elements
        .iter_mut()
        .filter_map(|element| {
            let metadata = element.metadata.as_mut()?;
            metadata.get("extraction")?;
            Some((element.path.as_str(), metadata))
        })
        .collect();
    for update in updates {
        let Some(metadata) = metadata.get_mut(update.file.entry.path.as_str()) else {
            continue;
        };
        metadata["extraction"]["resolution"] = resolution_counts(update);
    }
}

fn resolution_counts(update: &ReferenceFileUpdate) -> Value {
    let mut resolved = 0;
    let mut ambiguous = 0;
    for reference in &update.references {
        match reference.target {
            ReferenceTarget::Unique(_) => resolved += 1,
            ReferenceTarget::Ambiguous { .. } => ambiguous += 1,
            ReferenceTarget::Unresolved => {}
        }
    }
    json!({"resolved":resolved, "ambiguous":ambiguous,
        "unresolved":update.references.len() - resolved - ambiguous})
}
