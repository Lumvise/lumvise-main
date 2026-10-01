//! Deterministic graph-only PZ v1 snapshot writer, validator, and lookup reader.

use crate::{DbError, Result, SemanticElement, SemanticProjectSnapshot};
use arrow_array::types::Float32Type;
use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Int64Array, ListArray, RecordBatch, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

mod archive;
pub use archive::PzArchive;
mod format;
mod import;
mod snapshot;
mod tables;

pub(super) use crate::interface::PzSnapshotResult;
pub(crate) use format::PZ_REQUIRED_ENTRIES;
pub(super) use format::*;
pub(super) use import::*;
pub(super) use snapshot::*;
use tables::*;

#[cfg(test)]
mod tests {
    use super::row_group_of;

    /// Regression test for the row_group tie-break bug: `path_index` /
    /// `adjacency_index` rows were previously sorted by comparing the
    /// `row_group` cell as a raw string, so `"10" < "2"` lexicographically,
    /// diverging from the numeric canonical order the reader
    /// (`pz/archive.rs::validate_indexes`) requires once a snapshot has more
    /// than 9 row groups. This exercises the exact same comparator shape used
    /// by `build_tables`'s `path_rows.sort_by`/`adjacency.sort_by`, reusing
    /// the production `row_group_of` helper rather than reimplementing it.
    #[test]
    fn path_index_sort_orders_row_group_numerically() {
        let mut path_rows: Vec<Vec<Option<String>>> = vec![
            row(&["src/a.rs", "elem-1", "elements.parquet", "20"]),
            row(&["src/a.rs", "elem-1", "elements.parquet", "2"]),
            row(&["src/a.rs", "elem-1", "elements.parquet", "10"]),
            row(&["src/a.rs", "elem-1", "elements.parquet", "9"]),
        ];
        path_rows.sort_by(|a, b| {
            a[0].cmp(&b[0])
                .then(a[1].cmp(&b[1]))
                .then(a[2].cmp(&b[2]))
                .then(row_group_of(&a[3]).cmp(&row_group_of(&b[3])))
        });
        let row_groups: Vec<usize> = path_rows.iter().map(|r| row_group_of(&r[3])).collect();
        assert_eq!(row_groups, vec![2, 9, 10, 20]);
    }

    #[test]
    fn adjacency_index_sort_orders_row_group_numerically() {
        let mut adjacency: Vec<Vec<Option<String>>> = vec![
            row(&[
                "elem-1",
                "outgoing",
                "elem-1",
                "elem-2",
                "relationships.parquet",
                "20",
            ]),
            row(&[
                "elem-1",
                "outgoing",
                "elem-1",
                "elem-2",
                "relationships.parquet",
                "2",
            ]),
            row(&[
                "elem-1",
                "outgoing",
                "elem-1",
                "elem-2",
                "relationships.parquet",
                "10",
            ]),
            row(&[
                "elem-1",
                "outgoing",
                "elem-1",
                "elem-2",
                "relationships.parquet",
                "9",
            ]),
        ];
        adjacency.sort_by(|a, b| {
            a[0].cmp(&b[0])
                .then(a[1].cmp(&b[1]))
                .then(a[2].cmp(&b[2]))
                .then(a[3].cmp(&b[3]))
                .then(a[4].cmp(&b[4]))
                .then(row_group_of(&a[5]).cmp(&row_group_of(&b[5])))
        });
        let row_groups: Vec<usize> = adjacency.iter().map(|r| row_group_of(&r[5])).collect();
        assert_eq!(row_groups, vec![2, 9, 10, 20]);
    }

    fn row(cells: &[&str]) -> Vec<Option<String>> {
        cells.iter().map(|c| Some((*c).to_owned())).collect()
    }
}
