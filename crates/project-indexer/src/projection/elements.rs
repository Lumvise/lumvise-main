use super::{ProjectedFile, element_id, file_kind, published_kind};
use crate::source::invalid;
use crate::{ScanError, ScannedFile, SourceFingerprint, SourceKind};
use lumvise_contracts::SemanticElementUpsert;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) fn project_file(source: &str, file: &ScannedFile) -> Result<ProjectedFile, ScanError> {
    let root = file_element(source, file)?;
    let file_id = root.semantic_element_id.clone();
    let definition_ids = definition_ids(source, file);
    let parents = definition_parents(file);
    let mut elements = vec![root];
    for (index, definition) in file.parsed.definitions.iter().enumerate() {
        let parent = parents[index]
            .map(|parent| definition_ids[parent].clone())
            .unwrap_or_else(|| file_id.clone());
        elements.push(definition_element(
            source,
            file,
            &definition_ids[index],
            parent,
            definition,
        )?);
    }
    Ok(ProjectedFile {
        digest: file.digest,
        kind: file.entry.kind,
        parsed: Arc::clone(&file.parsed),
        file_id,
        definition_ids,
        elements,
    })
}

fn file_element(source: &str, file: &ScannedFile) -> Result<SemanticElementUpsert, ScanError> {
    let path = &file.entry.path;
    let folder = file.entry.kind == SourceKind::Directory;
    let name = path.rsplit('/').next().unwrap_or(path);
    let fingerprint = file.fingerprint.as_ref();
    Ok(SemanticElementUpsert {
        semantic_source_id: source.into(),
        semantic_element_id: element_id(
            source,
            path,
            if folder { "folder" } else { "file" },
            if folder { path } else { name },
        ),
        path: path.clone(),
        semantic_element_type: if folder { "folder" } else { file_kind(path) }.into(),
        semantic_element_name: name.into(),
        parent_element_id: path
            .rsplit_once('/')
            .map(|(parent, _)| element_id(source, parent, "folder", parent)),
        content_fingerprint: fingerprint.map(|value| value.encoded.clone()),
        start_line: (!folder).then_some(1),
        end_line: file_end_line(file)?,
        metadata: Some(
            serde_json::json!({ "kind": if folder { "directory" } else { file_kind(path) }, "bytes": file.byte_len, "extraction": super::coverage::extraction(file), "fingerprint_algorithm": fingerprint.map(|value| value.algorithm), "markdown_alias": file.parsed.document.as_ref().map(|doc| { let mut metadata = doc.metadata(); metadata["content"] = serde_json::json!(file.parsed.source.as_deref()); metadata }).or_else(|| file.parsed.conversion_error.as_ref().map(|reason| serde_json::json!({"status":"unavailable", "reason":reason}))) }),
        ),
    })
}

fn file_end_line(file: &ScannedFile) -> Result<Option<i64>, ScanError> {
    if file.entry.kind == SourceKind::Directory {
        return Ok(None);
    }
    if file_kind(&file.entry.path) != "file" {
        return Ok(Some(1));
    }
    file.parsed
        .source
        .as_ref()
        .map(|text| line_number(&file.entry.path, text.lines().count().max(1)))
        .transpose()
}

fn definition_ids(source: &str, file: &ScannedFile) -> Vec<String> {
    let mut occurrences = BTreeMap::new();
    file.parsed
        .definitions
        .iter()
        .map(|definition| {
            let kind = published_kind(&file.entry.path, &definition.kind);
            let count = occurrences.entry((kind, &definition.name)).or_insert(0);
            *count += 1;
            let name = if *count == 1 {
                definition.name.clone()
            } else {
                format!("{}#{count}", definition.name)
            };
            element_id(source, &file.entry.path, kind, &name)
        })
        .collect()
}

fn definition_element(
    source: &str,
    file: &ScannedFile,
    id: &str,
    parent: String,
    definition: &crate::IndexedDefinition,
) -> Result<SemanticElementUpsert, ScanError> {
    let body = file
        .parsed
        .source
        .as_ref()
        .and_then(|text| text.get(definition.span.start..definition.span.end))
        .ok_or_else(|| {
            invalid(
                format!(
                    "{}:{}..{}",
                    file.entry.path, definition.span.start, definition.span.end
                ),
                "expected definition span within shared UTF-8 source",
            )
        })?;
    let fingerprint = SourceFingerprint::text(body.trim());
    Ok(SemanticElementUpsert {
        semantic_source_id: source.into(),
        semantic_element_id: id.into(),
        path: file.entry.path.clone(),
        semantic_element_type: published_kind(&file.entry.path, &definition.kind).into(),
        semantic_element_name: definition.name.clone(),
        parent_element_id: Some(parent),
        content_fingerprint: Some(fingerprint.encoded),
        start_line: Some(line_number(&file.entry.path, definition.start_line)?),
        end_line: Some(line_number(&file.entry.path, definition.end_line)?),
        metadata: Some(
            serde_json::json!({"extractor": if file.parsed.document.is_some() { "document_markdown" } else { "tree_sitter" }, "fingerprint_algorithm": fingerprint.algorithm,
                "markdown_alias": file.parsed.document.as_ref().map(|doc| doc.metadata()),
                "anchor_selector": file.parsed.document.as_ref().and_then(|doc| doc.selector(file.parsed.source.as_deref().unwrap_or(""), definition.span)),
                "stable_name": id.strip_prefix(&format!("{source}:{}:{}:", file.entry.path, published_kind(&file.entry.path, &definition.kind))).and_then(|name| name.strip_suffix(':')).unwrap_or(&definition.name) }),
        ),
    })
}

fn line_number(path: &str, line: usize) -> Result<i64, ScanError> {
    i64::try_from(line)
        .ok()
        .filter(|line| *line > 0)
        .ok_or_else(|| {
            invalid(
                format!("{path}:{line}"),
                "expected positive 64-bit source line",
            )
        })
}

fn definition_parents(file: &ScannedFile) -> Vec<Option<usize>> {
    let definitions = &file.parsed.definitions;
    let mut ordered: Vec<_> = (0..definitions.len()).collect();
    ordered.sort_unstable_by_key(|index| {
        (
            definitions[*index].span.start,
            Reverse(definitions[*index].span.end),
            *index,
        )
    });
    let mut parents = vec![None; definitions.len()];
    let mut active: Vec<usize> = Vec::new();
    for index in ordered {
        let span = definitions[index].span;
        while active.last().is_some_and(|parent| {
            definitions[*parent].span.end < span.end || definitions[*parent].span == span
        }) {
            active.pop();
        }
        parents[index] = active.last().copied();
        active.push(index);
    }
    parents
}
