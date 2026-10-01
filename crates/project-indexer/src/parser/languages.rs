use std::path::Path;
use tree_sitter::Language;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SyntaxLanguage {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    C,
    Cpp,
    CSharp,
    Json,
    Markdown,
}

impl SyntaxLanguage {
    pub(super) fn for_path(path: &str) -> Option<Self> {
        let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "go" => Self::Go,
            "c" | "h" => Self::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Self::Cpp,
            "cs" => Self::CSharp,
            "json" => Self::Json,
            "md" | "markdown" => Self::Markdown,
            _ => return None,
        })
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Go => "go",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Json => "json",
            Self::Markdown => "markdown",
        }
    }

    pub(super) fn grammar(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            Self::Markdown => tree_sitter_md::LANGUAGE.into(),
        }
    }

    pub(super) fn query(self) -> &'static str {
        match self {
            Self::Rust => include_str!("queries/rust.scm"),
            Self::Python => include_str!("queries/python.scm"),
            Self::JavaScript => include_str!("queries/javascript.scm"),
            Self::TypeScript | Self::Tsx => include_str!("queries/typescript.scm"),
            Self::Go => include_str!("queries/go.scm"),
            Self::C => include_str!("queries/c.scm"),
            Self::Cpp => include_str!("queries/cpp.scm"),
            Self::CSharp => include_str!("queries/csharp.scm"),
            Self::Json => include_str!("queries/json.scm"),
            Self::Markdown => include_str!("queries/markdown.scm"),
        }
    }
}
