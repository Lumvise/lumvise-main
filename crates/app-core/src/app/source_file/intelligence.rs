//! App-owned source operations. Plugins receive locators and text, never filesystem handles.
mod git;
#[cfg(test)]
mod tests;

use super::{indexed_root, resolve_within_root};
use lumvise_db_core::SemanticPersistence;
use lumvise_resource_routing::InvocationControl;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum SourceRequest {
    Read {
        project_root: String,
        path: String,
        start_line: usize,
        end_line: usize,
    },
    Search {
        project_root: String,
        paths: Vec<String>,
        pattern: String,
        regex: bool,
        case_sensitive: bool,
    },
    GitChanges {
        project_root: String,
        base: String,
    },
}

/// Executes source access with the same indexed-root guard as the app file preview.
/// Example: invoke with `operation: "read"` and an inclusive one-based line range.
pub(crate) fn invoke_project_source(
    semantic: &dyn SemanticPersistence,
    input: Value,
    control: &InvocationControl,
) -> Result<Value, String> {
    let request: SourceRequest = serde_json::from_value(input.clone()).map_err(|error| {
        format!("source request {input}: expected read/search/git_changes: {error}")
    })?;
    let project = match &request {
        SourceRequest::Read { project_root, .. }
        | SourceRequest::Search { project_root, .. }
        | SourceRequest::GitChanges { project_root, .. } => project_root,
    };
    active(control)?;
    let root = indexed_root(semantic, project, control).map_err(|failure| failure.message)?;
    execute(&root, request, control)
}

fn execute(
    root: &Path,
    request: SourceRequest,
    control: &InvocationControl,
) -> Result<Value, String> {
    match request {
        SourceRequest::Read {
            path,
            start_line,
            end_line,
            ..
        } => read_range(root, &path, start_line, end_line),
        SourceRequest::Search {
            paths,
            pattern,
            regex,
            case_sensitive,
            ..
        } => search(root, paths, &pattern, regex, case_sensitive, control),
        SourceRequest::GitChanges { base, .. } => git::changes(root, &base, control),
    }
}

fn read_text(root: &Path, path: &str) -> Result<String, String> {
    let file = resolve_within_root(root, path).map_err(|failure| failure.message)?;
    std::fs::read_to_string(&file)
        .map_err(|error| format!("source {path}: expected readable UTF-8 file: {error}"))
}

fn read_range(root: &Path, path: &str, start: usize, end: usize) -> Result<Value, String> {
    if start == 0 || end < start {
        return Err(format!(
            "range {start}..{end}: expected 1 <= start_line <= end_line"
        ));
    }
    let text = read_text(root, path)?;
    let lines: Vec<_> = text.lines().collect();
    if start > lines.len() {
        return Err(format!(
            "line {start} in {path}: expected <= {}",
            lines.len()
        ));
    }
    Ok(
        json!({"path":path, "start_line":start, "end_line":end.min(lines.len()),
        "total_lines":lines.len(), "source_fingerprint":lumvise_project_indexer::SourceFingerprint::file(path,text.as_bytes()).encoded, "text":lines[start-1..end.min(lines.len())].join("\n")}),
    )
}

fn search(
    root: &Path,
    paths: Vec<String>,
    pattern: &str,
    regex: bool,
    case_sensitive: bool,
    control: &InvocationControl,
) -> Result<Value, String> {
    if pattern.is_empty() {
        return Err("pattern ``: expected non-empty search expression".into());
    }
    let expression = if regex {
        pattern.into()
    } else {
        regex::escape(pattern)
    };
    let matcher = regex::RegexBuilder::new(&expression)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| {
            format!("pattern {pattern}: expected valid regular expression: {error}")
        })?;
    let mut matches = Vec::new();
    let mut failures = Vec::new();
    for path in paths {
        active(control)?;
        match read_text(root, &path) {
            Ok(text) => matches.extend(
                text.lines()
                    .enumerate()
                    .filter(|(_, line)| matcher.is_match(line))
                    .map(|(line, text)| json!({"path":path,"line":line+1,"text":text})),
            ),
            Err(error) => failures.push(json!({"path":path,"error":error})),
        }
    }
    Ok(json!({"matches":matches,"failures":failures}))
}

fn active(control: &InvocationControl) -> Result<(), String> {
    if control.is_cancelled() || control.is_expired() {
        return Err(
            "project.source: invocation cancelled or deadline expired; expected active invocation"
                .into(),
        );
    }
    Ok(())
}
