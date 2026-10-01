//! Portable graph selection; local traversal and transport remain implementation details.

use serde::{Deserialize, Serialize};

use crate::{ProjectSnapshotScope, SemanticElement, SemanticRelationship};

/// Selects graph records for a bounded structural view without loading artifacts.
/// Example: `ScopedSemanticRead::Structure { scope: ProjectSnapshotScope::SemanticElement("root".into()), max_depth: 0, include_inactive: false }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopedSemanticRead {
    Structure {
        scope: ProjectSnapshotScope,
        max_depth: usize,
        include_inactive: bool,
    },
    /// Selects records at a canonical relative path and line. A missing path
    /// means the caller resolved a location outside this project: return no records.
    Location {
        project_root: String,
        path: Option<String>,
        line: i64,
        include_inactive: bool,
    },
    /// Selects records needed to render the dependency tree. Direction applies
    /// to the root; nested branches traverse both directions.
    Dependency {
        semantic_element_id: String,
        direction: SemanticDependencyDirection,
        max_depth: usize,
        include_descendants: bool,
        include_inactive: bool,
    },
}

/// Root dependency traversal direction. Example: `SemanticDependencyDirection::Both`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticDependencyDirection {
    Dependencies,
    Dependents,
    Both,
}

/// Records selected under one published revision, including structural parent edges.
/// Example: inspect `elements` after `SemanticOperation::ScopedRead(request)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticScopedGraph {
    pub commit_version: i64,
    pub published_at: String,
    pub project_root: String,
    pub elements: Vec<SemanticElement>,
    pub relationships: Vec<SemanticRelationship>,
    /// Dependency views preserve the complete eligible scope count while
    /// returning only reachable records. Other selectors leave this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_element_count: Option<usize>,
}
