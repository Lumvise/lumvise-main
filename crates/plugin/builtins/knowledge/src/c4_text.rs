use std::path::Path;

use crate::{components::ComponentGroup, semantic_context::SemanticElement};

pub(crate) fn display_label(group: &ComponentGroup) -> String {
    match group.label.as_str() {
        "crates/app-core" => "App Core".into(),
        "crates/frontend-core" => "Frontend Core".into(),
        "crates/neural-core" => "Neural Core".into(),
        "crates/db-core" => "Database Core".into(),
        "crates/mcp-core" => "MCP Core".into(),
        "tools/codeprysm-graph-wrapper" => "CodePrysm Graph Wrapper".into(),
        "project-docs" => "Project Documentation".into(),
        "documentation" => "Documentation Corpus".into(),
        value if value.starts_with("tools/") => titleize_tool_label(value),
        value => value.into(),
    }
}

pub(crate) fn lazy_c4_link(element: &SemanticElement, label: &str) -> String {
    let report_path = c4_report_path(element);
    let destination = format!(
        "Lumvise Knowledge/{}/Nuclei/{}",
        project_slug(&element.project_root),
        report_path.trim_end_matches(".md")
    );
    internal_link(&destination, label)
}

pub(crate) fn c4_report_path(element: &SemanticElement) -> String {
    let base = element.path.trim_matches('/');
    if base.is_empty() || base == "." {
        "C4 Architecture.md".into()
    } else {
        format!("{base}/C4 Architecture.md")
    }
}

pub(crate) fn c4_target_path(element: &SemanticElement) -> String {
    let base = element.path.trim_matches('/');
    if base.is_empty() || base == "." {
        return String::new();
    }
    let path_name = Path::new(base)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if path_name == element.name || element.name.trim().is_empty() {
        base.into()
    } else {
        format!("{base}/{}", safe_path_segment(&element.name))
    }
}

/// Prose cross-reference: renders as a real clickable link in the app's own
/// Markdown Preview (plain CommonMark). Unlike `internal_link` (used only
/// inside mermaid diagram node labels, which mermaid.js itself interprets as
/// HTML), the app's Milkdown editor has no raw-HTML passthrough for prose
/// text, so an `internal_link` here rendered as literal `<a ...>` text.
pub(crate) fn project_note_link(element: &SemanticElement, label: &str) -> String {
    let destination = format!(
        "Lumvise Knowledge/{}/Project/{}",
        project_slug(&element.project_root),
        project_note_target_path(element)
    );
    markdown_link(&destination, label)
}

pub(crate) fn c4_text(value: &str) -> String {
    let normalized: String = value
        .replace('"', "'")
        .replace('`', "")
        .replace(',', ";")
        .replace('|', "/")
        .replace(['\r', '\n'], " ")
        .chars()
        .take(110)
        .collect();
    html_body_text(&normalized)
}

fn project_note_target_path(element: &SemanticElement) -> String {
    let base = element.path.trim_matches('/');
    if base.is_empty() || base == "." || is_project_note_element(element, base) {
        return base.into();
    }
    let anchor = safe_path_segment(&element.name);
    if anchor.is_empty() {
        base.into()
    } else {
        format!("{base}#{anchor}")
    }
}

fn is_project_note_element(element: &SemanticElement, base: &str) -> bool {
    let path_name = Path::new(base)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    matches!(
        element.element_kind.as_str(),
        "project" | "workspace" | "folder" | "directory" | "package" | "dataset" | "file"
    ) || path_name == element.name
        || element.name.trim().is_empty()
}

fn safe_path_segment(value: &str) -> String {
    value
        .replace(
            [
                '\\', '/', ':', '*', '?', '"', '<', '>', '|', '#', '^', '[', ']',
            ],
            "-",
        )
        .trim_matches([' ', '.', '-'])
        .into()
}

fn internal_link(destination: &str, label: &str) -> String {
    format!(
        "<a class='internal-link is-unresolved' href='{}'>{}</a>",
        html_attr_text(destination),
        html_body_text(label)
    )
}

/// Plain CommonMark link: `[label](<destination>)`. Angle-bracketed so a
/// destination containing spaces (every vault path here does) stays one
/// link target instead of breaking the syntax.
pub(crate) fn markdown_link(destination: &str, label: &str) -> String {
    format!(
        "[{}](<{}>)",
        escape_markdown_label(label),
        escape_markdown_destination(destination)
    )
}

fn escape_markdown_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn escape_markdown_destination(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('<', "\\<")
        .replace('>', "\\>")
}

fn project_slug(project_root: &str) -> String {
    Path::new(project_root)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(project_root)
        .replace(
            [
                '\\', '/', ':', '*', '?', '"', '<', '>', '|', '#', '^', '[', ']',
            ],
            "-",
        )
        .trim()
        .trim_matches('.')
        .chars()
        .take(120)
        .collect()
}

fn html_attr_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('\'', "&#39;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html_body_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn titleize_tool_label(value: &str) -> String {
    value
        .trim_start_matches("tools/")
        .trim_end_matches(".py")
        .trim_end_matches(".rs")
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(title_case_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_case_word(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c4_text_matches_canonical_html_escaping() {
        assert_eq!(
            c4_text("Returns Result<Vec<Value>> & status."),
            "Returns Result&lt;Vec&lt;Value&gt;&gt; &amp; status."
        );
    }
}
