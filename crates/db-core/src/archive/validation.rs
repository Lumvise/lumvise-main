use super::{
    PZ_FORMAT_VERSION, PZ_REQUIRED_ENTRIES, PzByteRangeIntegrity, PzEntry, PzManifest,
    PzSnapshotResult, TABLE_SCHEMAS, column_data_type,
};
use crate::{DbError, Result};
use arrow_array::{Array, BooleanArray, Int64Array, StringArray};
use arrow_schema::DataType;
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::Type as PhysicalType;
use parquet::file::reader::{FileReader, SerializedFileReader};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use zip::CompressionMethod;
use zip::read::ZipArchive;

#[derive(Debug, Clone)]
struct PathLocator {
    path: String,
    semantic_element_id: String,
    target_entry: String,
    row_group: usize,
}

#[derive(Debug, Clone)]
struct AdjacencyLocator {
    endpoint_id: String,
    direction: String,
    source_element_id: String,
    target_element_id: String,
    target_entry: String,
    row_group: usize,
}

/// Opens, fully validates, and provides index-directed graph lookup over a PZ file.
///
/// Opening an archive validates ZIP ordering, manifest hashes, Parquet schemas,
/// physical range-integrity descriptors, and index locator membership before any
/// lookup is exposed.
///
/// # Example
///
/// ```no_run
/// use lumvise_db_core::PzArchive;
/// let archive = PzArchive::open("graph_db.pz")?;
/// let files = archive.lookup_path("src/lib.rs", false)?;
/// assert!(files.relationships.is_empty());
/// # Ok::<(), lumvise_db_core::DbError>(())
/// ```
pub struct PzArchive {
    manifest: PzManifest,
    #[cfg(test)]
    path_index: Vec<PathLocator>,
    #[cfg(test)]
    adjacency_index: Vec<AdjacencyLocator>,
    #[cfg(test)]
    elements_bytes: Vec<u8>,
    #[cfg(test)]
    relationships_bytes: Vec<u8>,
}

pub(super) fn integrity_for_table(
    bytes: &[u8],
) -> Result<(PzByteRangeIntegrity, Vec<PzByteRangeIntegrity>)> {
    let reader = SerializedFileReader::new(Bytes::copy_from_slice(bytes))
        .map_err(|e| DbError::invalid_value(e.to_string(), "valid parquet"))?;
    let metadata = reader.metadata();
    let footer_start = footer_start(bytes)?;
    let footer = PzByteRangeIntegrity {
        start: footer_start as u64,
        end_exclusive: bytes.len() as u64,
        sha256: sha256_hex(&bytes[footer_start..]),
    };
    let mut row_groups = Vec::with_capacity(metadata.row_groups().len());
    for group in metadata.row_groups() {
        let mut start = usize::MAX;
        let mut end = 0usize;
        for column in group.columns() {
            let column_start = column
                .dictionary_page_offset()
                .unwrap_or_else(|| column.data_page_offset());
            let compressed_size = column.compressed_size();
            if column_start < 0 || compressed_size < 0 {
                return Err(DbError::invalid_value(
                    "negative parquet physical range",
                    "non-negative row-group byte range",
                ));
            }
            let column_start = column_start as usize;
            let column_end = column_start
                .checked_add(compressed_size as usize)
                .ok_or_else(|| DbError::invalid_value("parquet physical range", "bounded range"))?;
            if column_end > bytes.len() {
                return Err(DbError::invalid_value(
                    column_end.to_string(),
                    "row-group range inside parquet entry",
                ));
            }
            start = start.min(column_start);
            end = end.max(column_end);
        }
        if start == usize::MAX || start >= end {
            return Err(DbError::invalid_value(
                "empty parquet row group",
                "physical row-group byte range",
            ));
        }
        row_groups.push(PzByteRangeIntegrity {
            start: start as u64,
            end_exclusive: end as u64,
            sha256: sha256_hex(&bytes[start..end]),
        });
    }
    Ok((footer, row_groups))
}

fn footer_start(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < 8 || &bytes[bytes.len() - 4..] != b"PAR1" {
        return Err(DbError::invalid_value("parquet footer", "PAR1 footer"));
    }
    let metadata_length = u32::from_le_bytes(
        bytes[bytes.len() - 8..bytes.len() - 4]
            .try_into()
            .expect("four-byte parquet footer length"),
    ) as usize;
    bytes
        .len()
        .checked_sub(metadata_length + 8)
        .filter(|start| *start < bytes.len() - 8)
        .ok_or_else(|| DbError::invalid_value("parquet footer", "bounded footer metadata"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

/// Reads every PZ table after the full archive validation `PzArchive::open` performs.
pub(super) fn read_validated_tables(path: &Path) -> Result<(PzManifest, HashMap<String, Vec<u8>>)> {
    let archive_path = path.to_path_buf();
    let archive_bytes = fs::read(&archive_path)?;
    let has_zip64_end = archive_bytes.windows(4).any(|bytes| bytes == b"PK\x06\x06");
    let has_zip64_locator = archive_bytes.windows(4).any(|bytes| bytes == b"PK\x06\x07");
    if !has_zip64_end || !has_zip64_locator {
        return Err(DbError::invalid_value(
            archive_path.display().to_string(),
            "ZIP64 PZ archive",
        ));
    }
    let file = File::open(&archive_path)?;
    let mut zip = ZipArchive::new(file).map_err(zip_error)?;
    let mut names = Vec::new();
    for i in 0..zip.len() {
        names.push(zip.by_index(i).map_err(zip_error)?.name().to_owned());
    }
    if names != PZ_REQUIRED_ENTRIES {
        return Err(DbError::invalid_value(
            names.join(","),
            "canonical PZ entry order",
        ));
    }
    let manifest = {
        let mut f = zip.by_name("manifest.json").map_err(zip_error)?;
        if f.compression() != CompressionMethod::Stored {
            return Err(DbError::invalid_value("manifest.json", "stored ZIP entry"));
        }
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        serde_json::from_slice::<PzManifest>(&bytes)?
    };
    validate_manifest(&manifest)?;
    let mut table_bytes = HashMap::new();
    let mut row_groups = HashMap::new();
    for entry in &manifest.entries {
        let mut f = zip.by_name(&entry.name).map_err(zip_error)?;
        if f.compression() != CompressionMethod::Stored {
            return Err(DbError::invalid_value(
                entry.name.clone(),
                "stored ZIP entry",
            ));
        }
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        row_groups.insert(entry.name.clone(), validate_table(entry, &bytes)?);
        table_bytes.insert(entry.name.clone(), bytes);
    }
    let path_index = decode_index(table_bytes.get("path_index.parquet").unwrap())?;
    let adjacency_index = decode_adjacency(table_bytes.get("adjacency_index.parquet").unwrap())?;
    validate_indexes(&table_bytes, &row_groups, &path_index, &adjacency_index)?;
    Ok((manifest, table_bytes))
}

impl PzArchive {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let (manifest, _table_bytes) = read_validated_tables(path.as_ref())?;
        #[cfg(test)]
        let mut table_bytes = _table_bytes;
        #[cfg(test)]
        let path_index = decode_index(table_bytes.get("path_index.parquet").unwrap())?;
        #[cfg(test)]
        let adjacency_index =
            decode_adjacency(table_bytes.get("adjacency_index.parquet").unwrap())?;
        #[cfg(test)]
        let elements_bytes = table_bytes.remove("elements.parquet").unwrap();
        #[cfg(test)]
        let relationships_bytes = table_bytes.remove("relationships.parquet").unwrap();
        Ok(Self {
            manifest,
            #[cfg(test)]
            path_index,
            #[cfg(test)]
            adjacency_index,
            #[cfg(test)]
            elements_bytes,
            #[cfg(test)]
            relationships_bytes,
        })
    }

    /// Reopens and validates every archive invariant, returning its manifest.
    #[cfg(test)]
    pub fn validate(path: impl AsRef<Path>) -> Result<PzManifest> {
        Ok(Self::open(path)?.manifest.clone())
    }

    /// Returns the validated PZ manifest.
    #[cfg(test)]
    pub fn manifest(&self) -> &PzManifest {
        &self.manifest
    }

    /// Looks up elements by exact path or recursive path prefix.
    #[cfg(test)]
    pub fn lookup_path(&self, path: &str, recursive: bool) -> Result<super::PzGraphLookup> {
        let key = path.replace('\\', "/");
        let locators = self
            .path_index
            .iter()
            .filter(|locator| {
                locator.path == key || (recursive && locator.path.starts_with(&(key.clone() + "/")))
            })
            .collect::<Vec<_>>();
        let ids = locators
            .iter()
            .map(|locator| locator.semantic_element_id.as_str())
            .collect::<BTreeSet<_>>();
        let row_groups = locators
            .iter()
            .map(|locator| locator.row_group)
            .collect::<BTreeSet<_>>();
        let elements = self
            .read_elements(&row_groups)?
            .into_iter()
            .filter(|element| ids.contains(element.semantic_element_id.as_str()))
            .collect();
        Ok(super::PzGraphLookup {
            elements,
            relationships: Vec::new(),
        })
    }

    /// Looks up the first indexed relationship neighbors of one element.
    #[cfg(test)]
    pub fn lookup_first_neighbors(&self, element_id: &str) -> Result<super::PzGraphLookup> {
        let relationship_groups = self
            .adjacency_index
            .iter()
            .filter(|locator| locator.endpoint_id == element_id)
            .map(|locator| locator.row_group)
            .collect::<BTreeSet<_>>();
        let relationships = self
            .read_relationships(&relationship_groups)?
            .into_iter()
            .filter(|relationship| first_neighbor_relationship(relationship, element_id))
            .collect::<Vec<_>>();
        let ids = relationships
            .iter()
            .flat_map(|relationship| {
                [
                    relationship.source_element_id.as_str(),
                    relationship.target_element_id.as_str(),
                ]
            })
            .collect::<BTreeSet<_>>();
        let element_groups = self.element_groups_for_ids(&ids);
        let elements = self
            .read_elements(&element_groups)?
            .into_iter()
            .filter(|element| ids.contains(element.semantic_element_id.as_str()))
            .collect();
        Ok(super::PzGraphLookup {
            elements,
            relationships,
        })
    }

    #[cfg(test)]
    fn element_groups_for_ids(&self, ids: &BTreeSet<&str>) -> BTreeSet<usize> {
        self.path_index
            .iter()
            .filter(|locator| ids.contains(locator.semantic_element_id.as_str()))
            .map(|locator| locator.row_group)
            .collect()
    }

    #[cfg(test)]
    fn read_elements(&self, row_groups: &BTreeSet<usize>) -> Result<Vec<super::PzElementRecord>> {
        decode_elements(&self.elements_bytes, row_groups)
    }

    #[cfg(test)]
    fn read_relationships(
        &self,
        row_groups: &BTreeSet<usize>,
    ) -> Result<Vec<super::PzRelationshipRecord>> {
        decode_relationships(&self.relationships_bytes, row_groups)
    }

    pub(super) fn result(&self, output_path: &Path) -> Result<PzSnapshotResult> {
        let bytes = fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
        Ok(PzSnapshotResult {
            project_id: self.manifest.project_id.clone(),
            snapshot_id: self.manifest.snapshot_id.clone(),
            commit_version: self.manifest.commit_version,
            published_at: self.manifest.published_at.clone(),
            output_path: output_path.to_owned(),
            output_bytes: bytes,
            row_counts: self
                .manifest
                .entries
                .iter()
                .map(|e| (e.name.clone(), e.row_count))
                .collect(),
        })
    }
}

#[cfg(test)]
fn first_neighbor_relationship(
    relationship: &super::PzRelationshipRecord,
    element_id: &str,
) -> bool {
    let incident = relationship.source_element_id == element_id
        || relationship.target_element_id == element_id;
    incident && relationship.relationship_kind != "contains" && relationship.label != "contains"
}

fn expected_physical_type(name: &str) -> PhysicalType {
    match column_data_type(name) {
        DataType::Int64 => PhysicalType::INT64,
        DataType::Boolean => PhysicalType::BOOLEAN,
        DataType::List(_) => PhysicalType::FLOAT,
        _ => PhysicalType::BYTE_ARRAY,
    }
}

fn column_type_matches(name: &str, actual: &DataType) -> bool {
    match (column_data_type(name), actual) {
        (DataType::List(_), DataType::List(field)) => field.data_type() == &DataType::Float32,
        (expected, actual) => &expected == actual,
    }
}

pub(super) fn validate_manifest(manifest: &PzManifest) -> Result<()> {
    if manifest.format_version != PZ_FORMAT_VERSION
        || uuid::Uuid::parse_str(&manifest.project_id).map(|u| u.get_version_num()) != Ok(4)
        || uuid::Uuid::parse_str(&manifest.snapshot_id).map(|u| u.get_version_num()) != Ok(7)
    {
        return Err(DbError::invalid_value(
            "manifest",
            "PZ v1 UUID/version fields",
        ));
    }
    if manifest.entries.len() != 9
        || manifest
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>()
            != PZ_REQUIRED_ENTRIES[1..]
    {
        return Err(DbError::invalid_value(
            "manifest entries",
            "canonical required table order",
        ));
    }
    for entry in &manifest.entries {
        let Some((_, schema, columns)) = TABLE_SCHEMAS
            .iter()
            .find(|(name, _, _)| *name == entry.name)
        else {
            return Err(DbError::invalid_value(&entry.name, "known table"));
        };
        if entry.schema != *schema
            || entry.sha256.len() != 64
            || entry.footer.sha256.len() != 64
            || entry.footer.start >= entry.footer.end_exclusive
            || columns.is_empty()
            || entry
                .row_groups
                .iter()
                .any(|group| group.sha256.len() != 64 || group.start >= group.end_exclusive)
        {
            return Err(DbError::invalid_value(
                &entry.name,
                "strict manifest integrity metadata",
            ));
        }
    }
    Ok(())
}

fn validate_table(entry: &PzEntry, bytes: &[u8]) -> Result<usize> {
    if bytes.len() as u64 != entry.uncompressed_bytes || sha256_hex(bytes) != entry.sha256 {
        return Err(DbError::invalid_value(
            &entry.name,
            "manifest size and SHA-256",
        ));
    }
    let reader = SerializedFileReader::new(Bytes::copy_from_slice(bytes))
        .map_err(|e| DbError::invalid_value(e.to_string(), "valid parquet"))?;
    let metadata = reader.metadata();
    if metadata.file_metadata().num_rows() as u64 != entry.row_count {
        return Err(DbError::invalid_value(&entry.name, "manifest row count"));
    }
    let (footer, row_groups) = integrity_for_table(bytes)?;
    if footer != entry.footer || row_groups != entry.row_groups {
        return Err(DbError::invalid_value(
            &entry.name,
            "manifest footer and row-group integrity metadata",
        ));
    }
    let Some((_, _, columns)) = TABLE_SCHEMAS.iter().find(|(n, _, _)| *n == entry.name) else {
        return Err(DbError::invalid_value(&entry.name, "known table"));
    };
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(bytes))
        .map_err(|e| DbError::invalid_value(e.to_string(), "readable parquet schema"))?;
    let schema = builder.schema();
    if schema.fields().len() != columns.len()
        || schema.fields().iter().zip(*columns).any(|(field, name)| {
            field.name() != *name || !column_type_matches(name, field.data_type())
        })
    {
        return Err(DbError::invalid_value(
            &entry.name,
            "declared typed parquet schema",
        ));
    }
    let descriptors = metadata.file_metadata().schema_descr().columns();
    for name in *columns {
        let valid_physical = if *name == "vector" {
            descriptors
                .iter()
                .any(|descriptor| descriptor.physical_type() == expected_physical_type(name))
        } else {
            descriptors
                .iter()
                .find(|descriptor| descriptor.name() == *name)
                .is_some_and(|descriptor| {
                    descriptor.physical_type() == expected_physical_type(name)
                })
        };
        if !valid_physical {
            return Err(DbError::invalid_value(
                &entry.name,
                "declared typed parquet schema",
            ));
        }
    }
    Ok(metadata.row_groups().len())
}

fn validate_indexes(
    table_bytes: &HashMap<String, Vec<u8>>,
    row_groups: &HashMap<String, usize>,
    paths: &[PathLocator],
    adjacency: &[AdjacencyLocator],
) -> Result<()> {
    let element_groups = *row_groups.get("elements.parquet").unwrap_or(&0);
    let relationship_groups = *row_groups.get("relationships.parquet").unwrap_or(&0);
    if paths.windows(2).any(|window| {
        (
            &window[0].path,
            &window[0].semantic_element_id,
            &window[0].target_entry,
            window[0].row_group,
        ) > (
            &window[1].path,
            &window[1].semantic_element_id,
            &window[1].target_entry,
            window[1].row_group,
        )
    }) {
        return Err(DbError::invalid_value(
            "path_index",
            "canonical path index order",
        ));
    }
    if adjacency.windows(2).any(|window| {
        (
            &window[0].endpoint_id,
            &window[0].direction,
            &window[0].source_element_id,
            &window[0].target_element_id,
            &window[0].target_entry,
            window[0].row_group,
        ) > (
            &window[1].endpoint_id,
            &window[1].direction,
            &window[1].source_element_id,
            &window[1].target_element_id,
            &window[1].target_entry,
            window[1].row_group,
        )
    }) {
        return Err(DbError::invalid_value(
            "adjacency_index",
            "canonical adjacency index order",
        ));
    }
    if paths.iter().any(|locator| {
        locator.semantic_element_id.is_empty()
            || locator.path.is_empty()
            || locator.target_entry != "elements.parquet"
            || locator.row_group >= element_groups
    }) {
        return Err(DbError::invalid_value(
            "path_index",
            "physical element row-group locators",
        ));
    }
    let path_ids = paths
        .iter()
        .map(|locator| locator.semantic_element_id.as_str())
        .collect::<BTreeSet<_>>();
    if path_ids.len() != paths.len() {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for locator in paths {
            *counts
                .entry(locator.semantic_element_id.as_str())
                .or_insert(0) += 1;
        }
        let duplicate_id = counts
            .into_iter()
            .find(|&(_, count)| count > 1)
            .map(|(id, _)| id)
            .unwrap_or("path_index");
        return Err(DbError::invalid_value(
            duplicate_id,
            "one locator per semantic element",
        ));
    }
    if adjacency.iter().any(|locator| {
        locator.endpoint_id.is_empty()
            || locator.source_element_id.is_empty()
            || locator.target_element_id.is_empty()
            || !path_ids.contains(locator.source_element_id.as_str())
            || !path_ids.contains(locator.target_element_id.as_str())
            || locator.target_entry != "relationships.parquet"
            || locator.row_group >= relationship_groups
            || (locator.direction != "outgoing" && locator.direction != "incoming")
            || (locator.direction == "outgoing" && locator.endpoint_id != locator.source_element_id)
            || (locator.direction == "incoming" && locator.endpoint_id != locator.target_element_id)
    }) {
        return Err(DbError::invalid_value(
            "adjacency_index",
            "physical relationship row-group locators",
        ));
    }
    let elements = table_bytes
        .get("elements.parquet")
        .ok_or_else(|| DbError::invalid_value("elements.parquet", "indexed PZ table"))?;
    let relationships = table_bytes
        .get("relationships.parquet")
        .ok_or_else(|| DbError::invalid_value("relationships.parquet", "indexed PZ table"))?;
    let path_groups = paths
        .iter()
        .map(|locator| locator.row_group)
        .collect::<BTreeSet<_>>();
    let mut element_rows = HashMap::new();
    for group in path_groups {
        let selected = BTreeSet::from([group]);
        for row in read_batches(elements, Some(&selected))? {
            element_rows.entry(group).or_insert_with(Vec::new).push(row);
        }
    }
    for locator in paths {
        let present = element_rows
            .get(&locator.row_group)
            .into_iter()
            .flatten()
            .any(|row| {
                row.len() > 2 && row[0] == locator.semantic_element_id && row[2] == locator.path
            });
        if !present {
            return Err(DbError::invalid_value(
                &locator.semantic_element_id,
                "path-index key in declared element row group",
            ));
        }
    }
    let adjacency_groups = adjacency
        .iter()
        .map(|locator| locator.row_group)
        .collect::<BTreeSet<_>>();
    let mut relationship_rows = HashMap::new();
    for group in adjacency_groups {
        let selected = BTreeSet::from([group]);
        for row in read_batches(relationships, Some(&selected))? {
            relationship_rows
                .entry(group)
                .or_insert_with(Vec::new)
                .push(row);
        }
    }
    for locator in adjacency {
        let present = relationship_rows
            .get(&locator.row_group)
            .into_iter()
            .flatten()
            .any(|row| {
                row.len() > 1
                    && row[0] == locator.source_element_id
                    && row[1] == locator.target_element_id
            });
        if !present {
            return Err(DbError::invalid_value(
                &locator.endpoint_id,
                "adjacency-index endpoint in declared relationship row group",
            ));
        }
    }
    Ok(())
}

fn read_batches(bytes: &[u8], row_groups: Option<&BTreeSet<usize>>) -> Result<Vec<Vec<String>>> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(bytes))
        .map_err(|e| DbError::invalid_value(e.to_string(), "readable parquet"))?;
    let builder = if let Some(groups) = row_groups {
        builder.with_row_groups(groups.iter().copied().collect())
    } else {
        builder
    };
    let reader = builder
        .with_batch_size(4096)
        .build()
        .map_err(|e| DbError::invalid_value(e.to_string(), "readable parquet batches"))?;
    let mut rows = Vec::new();
    for batch in reader {
        let batch =
            batch.map_err(|e| DbError::invalid_value(e.to_string(), "readable parquet batch"))?;
        for row in 0..batch.num_rows() {
            let mut values = Vec::new();
            for col in 0..batch.num_columns() {
                let array = batch.column(col);
                if array.is_null(row) {
                    values.push(String::new());
                } else if let Some(array) = array.as_any().downcast_ref::<StringArray>() {
                    values.push(array.value(row).to_owned());
                } else if let Some(array) = array.as_any().downcast_ref::<Int64Array>() {
                    values.push(array.value(row).to_string());
                } else if let Some(array) = array.as_any().downcast_ref::<BooleanArray>() {
                    values.push(array.value(row).to_string());
                } else {
                    return Err(DbError::invalid_value(
                        "column",
                        "supported typed parquet column",
                    ));
                }
            }
            rows.push(values);
        }
    }
    Ok(rows)
}

#[cfg(test)]
fn decode_elements(
    bytes: &[u8],
    row_groups: &BTreeSet<usize>,
) -> Result<Vec<super::PzElementRecord>> {
    Ok(read_batches(bytes, Some(row_groups))?
        .into_iter()
        .map(|r| super::PzElementRecord {
            semantic_element_id: r[0].clone(),
            path: r[2].clone(),
            element_kind: r[3].clone(),
            name: r[4].clone(),
        })
        .collect())
}

#[cfg(test)]
fn decode_relationships(
    bytes: &[u8],
    row_groups: &BTreeSet<usize>,
) -> Result<Vec<super::PzRelationshipRecord>> {
    Ok(read_batches(bytes, Some(row_groups))?
        .into_iter()
        .map(|r| super::PzRelationshipRecord {
            source_element_id: r[0].clone(),
            target_element_id: r[1].clone(),
            relationship_kind: r[2].clone(),
            label: r[3].clone(),
        })
        .collect())
}

fn decode_index(bytes: &[u8]) -> Result<Vec<PathLocator>> {
    read_batches(bytes, None)?
        .into_iter()
        .map(|r| {
            if r.len() != 4 || r[2] != "elements.parquet" {
                return Err(DbError::invalid_value(
                    "path_index",
                    "valid target entry locator",
                ));
            }
            Ok(PathLocator {
                path: r[0].clone(),
                semantic_element_id: r[1].clone(),
                target_entry: r[2].clone(),
                row_group: r[3].parse().map_err(|_| {
                    DbError::invalid_value("path_index", "integer row-group locator")
                })?,
            })
        })
        .collect()
}

fn decode_adjacency(bytes: &[u8]) -> Result<Vec<AdjacencyLocator>> {
    read_batches(bytes, None)?
        .into_iter()
        .map(|r| {
            if r.len() != 6
                || r[4] != "relationships.parquet"
                || (r[1] != "outgoing" && r[1] != "incoming")
            {
                return Err(DbError::invalid_value(
                    "adjacency_index",
                    "valid target entry locator",
                ));
            }
            Ok(AdjacencyLocator {
                endpoint_id: r[0].clone(),
                direction: r[1].clone(),
                source_element_id: r[2].clone(),
                target_element_id: r[3].clone(),
                target_entry: r[4].clone(),
                row_group: r[5].parse().map_err(|_| {
                    DbError::invalid_value("adjacency_index", "integer row-group locator")
                })?,
            })
        })
        .collect()
}

fn zip_error(error: zip::result::ZipError) -> DbError {
    DbError::invalid_value(error.to_string(), "valid ZIP64 PZ archive")
}
