//! Shared ingestion paging for desktop and compiled producers; revision policy stays in Semantic.
//! Page zero carries the full observation and replacement scope. Repeating that observation or
//! serializing the entire batch per page makes large source publications quadratic in size.
use lumvise_contracts::{
    IndexBatchRequest, SemanticElementUpsert, SemanticIndexObservation, SemanticRelationshipUpsert,
    SemanticSourceUpsert,
};
use serde::Serialize;
use std::collections::BTreeSet;
use std::ops::Range;

// These staged increments keep ordinary ingestion invocations within the plugin deadline.
const ELEMENTS_PER_PAGE: usize = 1000;
const RELATIONSHIPS_PER_PAGE: usize = 5000;

/// Lazily borrows one producer batch instead of cloning all pages before publication.
/// Example: serialize each value from SemanticIndexPages::new(&batch, "operation", false).
pub struct SemanticIndexPages<'a> {
    batch: &'a IndexBatchRequest,
    operation_id: &'a str,
    full_snapshot: bool,
    next: usize,
    count: usize,
}

/// One serializable staged input; its slices borrow the frozen producer batch.
/// Example: serde_json::to_value(page)? before invoking Semantic's ingestion export.
#[derive(Serialize)]
pub struct SemanticIndexPage<'a> {
    provider_instance_id: &'a str,
    project_root: &'a str,
    revision_observation: Option<&'a SemanticIndexObservation>,
    semantic_sources: &'a [SemanticSourceUpsert],
    semantic_elements: &'a [SemanticElementUpsert],
    semantic_relationships: &'a [SemanticRelationshipUpsert],
    replace_paths: Vec<&'a str>,
    removed_paths: &'a [String],
    ingestion_job_id: &'a str,
    ingestion_page_index: usize,
    ingestion_page_count: usize,
}

impl<'a> SemanticIndexPages<'a> {
    /// Pages a complete or path-scoped frozen batch without serializing it eagerly.
    /// Example: SemanticIndexPages::new(&batch, "source-index-1", false).len().
    pub fn new(batch: &'a IndexBatchRequest, operation_id: &'a str, full_snapshot: bool) -> Self {
        Self {
            batch,
            operation_id,
            full_snapshot,
            next: 0,
            count: ingestion_page_count(batch),
        }
    }

    fn build_index_page(&self, index: usize) -> SemanticIndexPage<'a> {
        let batch = self.batch;
        let elements = page_slice(&batch.semantic_elements, index, ELEMENTS_PER_PAGE);
        SemanticIndexPage {
            provider_instance_id: &batch.provider_instance_id,
            project_root: &batch.project_root,
            revision_observation: if index == 0 {
                batch.revision_observation.as_ref()
            } else {
                None
            },
            semantic_sources: if index == 0 {
                &batch.semantic_sources
            } else {
                &[]
            },
            semantic_elements: elements,
            semantic_relationships: page_slice(
                &batch.semantic_relationships,
                index,
                RELATIONSHIPS_PER_PAGE,
            ),
            replace_paths: page_paths(batch, elements, index, self.full_snapshot),
            removed_paths: if index == 0 {
                &batch.removed_paths
            } else {
                &[]
            },
            ingestion_job_id: self.operation_id,
            ingestion_page_index: index,
            ingestion_page_count: self.count,
        }
    }
}

impl<'a> Iterator for SemanticIndexPages<'a> {
    type Item = SemanticIndexPage<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.count {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(self.build_index_page(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.count - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for SemanticIndexPages<'_> {}

fn page_paths<'a>(
    batch: &'a IndexBatchRequest,
    elements: &'a [SemanticElementUpsert],
    index: usize,
    full: bool,
) -> Vec<&'a str> {
    if full {
        return vec![];
    }
    if index == 0 {
        return batch.replace_paths.iter().map(String::as_str).collect();
    }
    elements
        .iter()
        .map(|element| element.path.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn window(index: usize, stride: usize, count: usize) -> Range<usize> {
    let start = index.saturating_mul(stride).min(count);
    start..start.saturating_add(stride).min(count)
}

fn page_slice<T>(records: &[T], index: usize, stride: usize) -> &[T] {
    &records[window(index, stride, records.len())]
}

fn ingestion_page_count(batch: &IndexBatchRequest) -> usize {
    batch
        .semantic_elements
        .len()
        .div_ceil(ELEMENTS_PER_PAGE)
        .max(
            batch
                .semantic_relationships
                .len()
                .div_ceil(RELATIONSHIPS_PER_PAGE),
        )
        .max(1)
}
