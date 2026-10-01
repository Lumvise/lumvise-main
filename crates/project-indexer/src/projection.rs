//! Owns the existing semantic index DTO projection, IDs and reusable file fragments.
//! Callers use SemanticIndexProjection::project and publish its IndexBatchRequest
//! through Semantic. This module never accesses persistence or performs source I/O.
mod coverage;
mod elements;

use crate::fingerprints::{file_kind, stable_hash};
use crate::source::invalid;
use crate::{
    ParsedFile, PreparedProjectScan, ReferenceFileUpdate, ReferenceTarget, ScanError, ScannedFile,
    SourceKind,
};
use lumvise_contracts::{
    IndexBatchRequest, SemanticElementUpsert, SemanticRelationshipUpsert, SemanticSourceUpsert,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

/// Projects prepared source changes into the existing semantic ingestion contract.
/// Example: construct with `source.root()`, project, publish, then commit the scan.
pub struct SemanticIndexProjection {
    project_root: String,
    provider_id: String,
    source: SemanticSourceUpsert,
    files: BTreeMap<String, Arc<ProjectedFile>>,
    metrics: ProjectionMetrics,
}

/// Projection work counters. Example: a cached caller needs no new declaration
/// fingerprints when another file changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectionMetrics {
    /// Successfully built file fragments, including declaration fingerprints.
    pub files_projected: usize,
    /// Fragment lookups satisfied by prepared or acknowledged source reuse.
    pub cache_hits: usize,
}

struct ProjectedFile {
    digest: [u8; 32],
    kind: SourceKind,
    parsed: Arc<ParsedFile>,
    file_id: String,
    definition_ids: Vec<String>,
    elements: Vec<SemanticElementUpsert>,
}

impl SemanticIndexProjection {
    /// Uses the canonical absolute root already established by the source adapter.
    /// Example: `SemanticIndexProjection::new(source.root(), "mcp-client-1")`.
    pub fn new(root: &Path, provider_id: &str) -> Result<Self, ScanError> {
        let root_text = root
            .to_str()
            .ok_or_else(|| invalid(root.display().to_string(), "expected UTF-8 project root"))?;
        if !root.is_absolute()
            || root
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || provider_id.trim().is_empty()
        {
            return Err(invalid(
                format!("{root_text}:{provider_id}"),
                "expected canonical absolute root and non-blank provider id",
            ));
        }
        let source = SemanticSourceUpsert {
            semantic_source_id: format!("filesystem:{:016x}", stable_hash(root_text.as_bytes())),
            kind: "filesystem".into(),
            name: root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project")
                .into(),
            root_path: Some(root_text.into()),
            root_uri: format!("file://{root_text}"),
        };
        Ok(Self {
            project_root: root_text.into(),
            provider_id: provider_id.into(),
            source,
            files: BTreeMap::new(),
            metrics: ProjectionMetrics::default(),
        })
    }

    /// Returns cumulative work counts. Example: compare files_projected across an
    /// edit to verify unchanged callers reused their fragments.
    pub fn metrics(&self) -> &ProjectionMetrics {
        &self.metrics
    }

    /// Returns a full initial snapshot or complete replacements for affected paths.
    /// Memoized fragments are derived content only; publication is acknowledged by
    /// ProjectIndexer::commit, so a failed publish still produces a complete retry.
    pub fn project(&mut self, scan: &PreparedProjectScan) -> Result<IndexBatchRequest, ScanError> {
        if !scan.needs_publication() {
            return Err(invalid(
                "unchanged scan",
                "expected pending semantic publication",
            ));
        }
        let mut batch = self.empty_batch(scan);
        let selected: BTreeMap<_, _> = scan
            .changed_files()
            .map(|file| (&file.entry.path, file))
            .chain(
                scan.reference_updates()
                    .map(|update| (&update.file.entry.path, update.file.as_ref())),
            )
            .collect();
        for file in selected.values() {
            batch
                .semantic_elements
                .extend(self.file(file)?.elements.iter().cloned());
        }
        if !scan.is_full_snapshot() {
            batch.replace_paths = selected.keys().map(|path| (*path).clone()).collect();
        }
        for update in scan.reference_updates() {
            self.append_references(update, &mut batch.semantic_relationships)?;
        }
        coverage::project_resolutions(scan.reference_updates(), &mut batch.semantic_elements);
        deduplicate_relationships(&mut batch.semantic_relationships);
        for path in scan.removed_paths() {
            self.files.remove(path);
        }
        Ok(batch)
    }

    fn empty_batch(&self, scan: &PreparedProjectScan) -> IndexBatchRequest {
        IndexBatchRequest {
            provider_instance_id: self.provider_id.clone(),
            project_root: self.project_root.clone(),
            replace_paths: Vec::new(),
            removed_paths: if scan.is_full_snapshot() {
                Vec::new()
            } else {
                scan.removed_paths().map(str::to_owned).collect()
            },
            semantic_sources: vec![self.source.clone()],
            semantic_elements: Vec::new(),
            semantic_relationships: Vec::new(),
        }
    }

    fn file(&mut self, file: &ScannedFile) -> Result<Arc<ProjectedFile>, ScanError> {
        if let Some(cached) = self.files.get(&file.entry.path).filter(|cached| {
            cached.digest == file.digest
                && cached.kind == file.entry.kind
                && Arc::ptr_eq(&cached.parsed, &file.parsed)
        }) {
            self.metrics.cache_hits += 1;
            return Ok(Arc::clone(cached));
        }
        let projected = Arc::new(elements::project_file(
            &self.source.semantic_source_id,
            file,
        )?);
        self.metrics.files_projected += 1;
        self.files
            .insert(file.entry.path.clone(), Arc::clone(&projected));
        Ok(projected)
    }

    fn append_references(
        &mut self,
        update: &ReferenceFileUpdate,
        output: &mut Vec<SemanticRelationshipUpsert>,
    ) -> Result<(), ScanError> {
        let source = self.file(&update.file)?;
        for reference in &update.references {
            let ReferenceTarget::Unique(site) = &reference.target else {
                continue;
            };
            let target = self.file(&site.file)?;
            let source_id = reference
                .owner_definition
                .map(|index| &source.definition_ids[index])
                .unwrap_or(&source.file_id);
            let target_id = &target.definition_ids[site.definition_index];
            let intent = &update.file.parsed.references[reference.reference_index].kind;
            if source_id == target_id && intent != "calls" {
                continue;
            }
            output.push(relationship(
                source_id,
                target_id,
                &site.file.parsed.definitions[site.definition_index].kind,
                intent,
            ));
        }
        Ok(())
    }
}

fn relationship(
    source: &str,
    target: &str,
    target_kind: &str,
    intent: &str,
) -> SemanticRelationshipUpsert {
    let label = match (intent, target_kind) {
        ("calls", "class" | "struct" | "record") => "instantiates",
        ("calls", _) => "calls",
        (_, "field" | "property" | "parameter" | "constant" | "variable" | "value" | "local") => {
            "uses_property"
        }
        _ => "uses_type",
    };
    SemanticRelationshipUpsert {
        source_element_id: source.into(),
        target_element_id: Some(target.into()),
        relationship_kind: "semantic".into(),
        relationship_label: label.into(),
        target_label: Some(target.into()),
        target_locator: Some(target.into()),
    }
}

fn deduplicate_relationships(relationships: &mut Vec<SemanticRelationshipUpsert>) {
    let mut seen = BTreeSet::new();
    relationships.retain(|item| {
        seen.insert((
            item.source_element_id.clone(),
            item.target_element_id.clone(),
            item.relationship_label.clone(),
        ))
    });
}

fn element_id(source: &str, path: &str, kind: &str, name: &str) -> String {
    format!("{source}:{path}:{kind}:{name}:")
}

fn published_kind<'a>(path: &str, kind: &'a str) -> &'a str {
    match kind {
        "alias" => "type",
        "namespace" => "module",
        "method"
            if matches!(
                Path::new(path).extension().and_then(|value| value.to_str()),
                Some("rs" | "py" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx")
            ) =>
        {
            "function"
        }
        _ => kind,
    }
}
