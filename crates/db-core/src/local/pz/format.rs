use super::*;

pub(super) const ROWS_PER_ROW_GROUP: usize = 1024;
pub const PZ_FORMAT_VERSION: u32 = 1;
pub const PZ_REQUIRED_ENTRIES: [&str; 10] = [
    "manifest.json",
    "elements.parquet",
    "relationships.parquet",
    "external_references.parquet",
    "artifacts.parquet",
    "artifact_blobs.parquet",
    "artifact_text_vectors.parquet",
    "element_name_vectors.parquet",
    "path_index.parquet",
    "adjacency_index.parquet",
];

pub(super) const TABLE_SCHEMAS: [(&str, &str, &[&str]); 9] = [
    (
        "elements.parquet",
        "lumvise.pz.elements.v1",
        &[
            "semantic_element_id",
            "semantic_source_id",
            "path",
            "element_kind",
            "name",
            "parent_element_id",
            "content_fingerprint",
            "start_line",
            "end_line",
            "lifecycle",
            "match_evidence_json",
            "metadata_json",
        ],
    ),
    (
        "relationships.parquet",
        "lumvise.pz.relationships.v1",
        &[
            "source_element_id",
            "target_element_id",
            "relationship_kind",
            "label",
            "metadata_json",
        ],
    ),
    (
        "external_references.parquet",
        "lumvise.pz.external_references.v1",
        &[
            "source_element_id",
            "foreign_project_id",
            "foreign_element_id",
            "reference_kind",
            "metadata_json",
        ],
    ),
    (
        "artifacts.parquet",
        "lumvise.pz.artifacts.v1",
        &[
            "artifact_id",
            "semantic_element_id",
            "artifact_kind",
            "title",
            "content_ref",
            "searchable_text",
            "content_size_bytes",
            "metadata_json",
        ],
    ),
    (
        "artifact_blobs.parquet",
        "lumvise.pz.artifact_blobs.v1",
        &[
            "content_ref",
            "artifact_id",
            "media_type",
            "content",
            "byte_size",
            "sha256",
            "updated_at",
        ],
    ),
    (
        "artifact_text_vectors.parquet",
        "lumvise.pz.artifact_text_vectors.v1",
        &[
            "artifact_id",
            "semantic_element_id",
            "source_text",
            "engine_id",
            "model",
            "dimensions",
            "normalized",
            "vector",
            "metadata_json",
        ],
    ),
    (
        "element_name_vectors.parquet",
        "lumvise.pz.element_name_vectors.v1",
        &[
            "semantic_element_id",
            "source_text",
            "engine_id",
            "model",
            "dimensions",
            "normalized",
            "vector",
            "metadata_json",
        ],
    ),
    (
        "path_index.parquet",
        "lumvise.pz.path_index.v1",
        &["path", "semantic_element_id", "target_entry", "row_group"],
    ),
    (
        "adjacency_index.parquet",
        "lumvise.pz.adjacency_index.v1",
        &[
            "endpoint_id",
            "direction",
            "source_element_id",
            "target_element_id",
            "target_entry",
            "row_group",
        ],
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PzManifest {
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    #[serde(rename = "projectId")]
    pub project_id: String,
    #[serde(rename = "snapshotId")]
    pub snapshot_id: String,
    #[serde(rename = "commitVersion")]
    pub commit_version: i64,
    #[serde(rename = "publishedAt")]
    pub published_at: String,
    pub project: PzProjectInfo,
    pub entries: Vec<PzEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PzProjectInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PzEntry {
    pub name: String,
    pub schema: String,
    pub sha256: String,
    #[serde(rename = "uncompressedBytes")]
    pub uncompressed_bytes: u64,
    #[serde(rename = "rowCount")]
    pub row_count: u64,
    /// Physical byte range and digest for the Parquet footer.
    pub footer: PzByteRangeIntegrity,
    /// Physical byte ranges and digests for each Parquet row group.
    #[serde(rename = "rowGroups")]
    pub row_groups: Vec<PzByteRangeIntegrity>,
}

/// A deterministic physical byte range protected by SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PzByteRangeIntegrity {
    pub start: u64,
    #[serde(rename = "endExclusive")]
    pub end_exclusive: u64,
    pub sha256: String,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PzGraphLookup {
    pub elements: Vec<PzElementRecord>,
    pub relationships: Vec<PzRelationshipRecord>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PzElementRecord {
    pub semantic_element_id: String,
    pub path: String,
    pub name: String,
    pub element_kind: String,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PzRelationshipRecord {
    pub source_element_id: String,
    pub target_element_id: String,
    pub relationship_kind: String,
    pub label: String,
}

pub(super) struct TableBytes {
    pub(super) name: &'static str,
    pub(super) schema_id: &'static str,
    pub(super) bytes: Vec<u8>,
    pub(super) rows: u64,
}
#[derive(Debug, Clone)]
pub(super) enum TypedCell {
    Null,
    Text(String),
    Int64(i64),
    Bool(bool),
    Binary(Vec<u8>),
    Vector(Vec<f32>),
}
