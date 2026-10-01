use super::*;

pub(super) fn vector_row(scalars: &[Option<String>], vector: &[f32]) -> Result<Vec<TypedCell>> {
    let vector_index = match scalars.len() {
        9 => 7,
        8 => 6,
        _ => {
            return Err(DbError::invalid_value(
                "vector row",
                "known vector table schema",
            ));
        }
    };
    if !vector.iter().all(|value| value.is_finite()) {
        return Err(DbError::invalid_value("vector", "finite vector values"));
    }
    let dimension_index = vector_index - 2;
    let normalized_index = vector_index - 1;
    scalars
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if index == vector_index {
                return Ok(TypedCell::Vector(vector.to_vec()));
            }
            let Some(value) = value.as_ref() else {
                return Ok(TypedCell::Null);
            };
            if index == dimension_index {
                return value
                    .parse::<i64>()
                    .map(TypedCell::Int64)
                    .map_err(|_| DbError::invalid_value(value, "vector dimensions"));
            }
            if index == normalized_index {
                return value
                    .parse::<bool>()
                    .map(TypedCell::Bool)
                    .map_err(|_| DbError::invalid_value(value, "vector normalized flag"));
            }
            Ok(TypedCell::Text(value.clone()))
        })
        .collect()
}
/// Parses a `row_group` index cell to its numeric value for canonical ordering,
/// matching the numeric parse performed reader-side in `pz/archive.rs`. Sorting
/// this field as a raw string (`"10" < "2"`) diverges from the reader's
/// canonical numeric order once a snapshot has more than 9 row groups.
pub(super) fn row_group_of(cell: &Option<String>) -> usize {
    cell.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0)
}
pub(super) fn build_tables(
    snapshot: &SemanticProjectSnapshot,
    artifact_blobs: &[crate::ArtifactBlob],
    artifact_text_vectors: &[crate::StoredArtifactTextVector],
    element_name_vectors: &[crate::StoredSemanticElementNameVector],
) -> Result<Vec<TableBytes>> {
    let active: Vec<&SemanticElement> = snapshot
        .elements
        .iter()
        .filter(|e| e.lifecycle == "active")
        .collect();
    let active_ids: BTreeSet<&str> = active
        .iter()
        .map(|e| e.semantic_element_id.as_str())
        .collect();
    let mut elements = Vec::new();
    let all_ids: BTreeSet<&str> = snapshot
        .elements
        .iter()
        .map(|e| e.semantic_element_id.as_str())
        .collect();
    for e in &active {
        elements.push(vec![
            Some(e.semantic_element_id.clone()),
            Some(e.semantic_source_id.clone()),
            Some(root_relative_path(&e.path, &snapshot.project_root)?),
            Some(e.element_kind.clone()),
            Some(e.name.clone()),
            e.parent_element_id.clone(),
            e.content_fingerprint.clone(),
            e.start_line.map(|v| v.to_string()),
            e.end_line.map(|v| v.to_string()),
            Some(e.lifecycle.clone()),
            serde_json::to_string(&e.match_evidence).ok(),
            Some(canonical_json_value(&e.metadata)?),
        ]);
    }
    elements.sort_by(|a, b| a[0].cmp(&b[0]));
    let mut relationships = snapshot
        .relationships
        .iter()
        .filter(|r| {
            active_ids.contains(r.source_element_id.as_str())
                && active_ids.contains(r.target_element_id.as_str())
        })
        .map(|r| {
            vec![
                Some(r.source_element_id.clone()),
                Some(r.target_element_id.clone()),
                Some(r.relationship_kind.clone()),
                Some(r.label.clone()),
                Some(canonical_json_value(&r.metadata).unwrap_or_else(|_| "null".to_owned())),
            ]
        })
        .collect::<Vec<_>>();
    relationships.sort_by(|a, b| {
        a[0].cmp(&b[0])
            .then(a[1].cmp(&b[1]))
            .then(a[2].cmp(&b[2]))
            .then(a[3].cmp(&b[3]))
    });
    let mut path_rows = elements
        .iter()
        .enumerate()
        .map(|(row, r)| {
            vec![
                r[2].clone(),
                r[0].clone(),
                Some("elements.parquet".into()),
                Some((row / ROWS_PER_ROW_GROUP).to_string()),
            ]
        })
        .collect::<Vec<_>>();
    path_rows.sort_by(|a, b| {
        a[0].cmp(&b[0])
            .then(a[1].cmp(&b[1]))
            .then(a[2].cmp(&b[2]))
            .then(row_group_of(&a[3]).cmp(&row_group_of(&b[3])))
    });
    let mut adjacency = Vec::with_capacity(relationships.len() * 2);
    for (row, r) in relationships.iter().enumerate() {
        let source = r[0].clone().unwrap_or_default();
        let target = r[1].clone().unwrap_or_default();
        let row_group = (row / ROWS_PER_ROW_GROUP).to_string();
        adjacency.push(vec![
            Some(source.clone()),
            Some("outgoing".into()),
            Some(source.clone()),
            Some(target.clone()),
            Some("relationships.parquet".into()),
            Some(row_group.clone()),
        ]);
        adjacency.push(vec![
            Some(target.clone()),
            Some("incoming".into()),
            Some(source),
            Some(target),
            Some("relationships.parquet".into()),
            Some(row_group),
        ]);
    }
    adjacency.sort_by(|a, b| {
        a[0].cmp(&b[0])
            .then(a[1].cmp(&b[1]))
            .then(a[2].cmp(&b[2]))
            .then(a[3].cmp(&b[3]))
            .then(a[4].cmp(&b[4]))
            .then(row_group_of(&a[5]).cmp(&row_group_of(&b[5])))
    });
    let mut external_refs = snapshot
        .relationships
        .iter()
        .filter(|r| {
            active_ids.contains(r.source_element_id.as_str())
                && !all_ids.contains(r.target_element_id.as_str())
        })
        .filter_map(|r| {
            let project_id = external_project_id(&r.metadata)?;
            Some(vec![
                Some(r.source_element_id.clone()),
                Some(project_id),
                Some(r.target_element_id.clone()),
                Some(r.relationship_kind.clone()),
                Some(canonical_json_value(&r.metadata).unwrap_or_else(|_| "null".to_owned())),
            ])
        })
        .collect::<Vec<_>>();
    external_refs.sort_by(|a, b| a[0].cmp(&b[0]).then(a[1].cmp(&b[1])).then(a[2].cmp(&b[2])));
    let mut artifacts = snapshot
        .artifacts
        .iter()
        .filter(|artifact| active_ids.contains(artifact.semantic_element_id.as_str()))
        .map(|artifact| {
            Ok(vec![
                Some(artifact.artifact_id.clone()),
                Some(artifact.semantic_element_id.clone()),
                Some(artifact.artifact_kind.clone()),
                Some(artifact.title.clone()),
                artifact.content_ref.clone(),
                artifact.searchable_text.clone(),
                artifact.content_size_bytes.map(|size| size.to_string()),
                Some(canonical_json_value(&artifact.metadata)?),
            ])
        })
        .collect::<Result<Vec<_>>>()?;
    artifacts.sort_by(|a, b| a[1].cmp(&b[1]).then(a[0].cmp(&b[0])));
    let artifact_ids = snapshot
        .artifacts
        .iter()
        .filter(|artifact| active_ids.contains(artifact.semantic_element_id.as_str()))
        .map(|artifact| artifact.artifact_id.as_str())
        .collect::<BTreeSet<_>>();
    let blob_rows = artifact_blobs
        .iter()
        .filter(|blob| artifact_ids.contains(blob.artifact_id.as_str()))
        .map(|blob| {
            vec![
                TypedCell::Text(blob.content_ref.clone()),
                TypedCell::Text(blob.artifact_id.clone()),
                TypedCell::Text(blob.media_type.clone()),
                TypedCell::Binary(blob.content.clone()),
                TypedCell::Int64(blob.content.len() as i64),
                TypedCell::Text(sha256_hex(&blob.content)),
                TypedCell::Text(blob.updated_at.clone()),
            ]
        })
        .collect::<Vec<_>>();
    let artifact_vector_rows = artifact_text_vectors
        .iter()
        .filter(|vector| {
            active_ids.contains(vector.semantic_element_id.as_str())
                && artifact_ids.contains(vector.artifact_id.as_str())
        })
        .map(|stored| {
            vector_row(
                &[
                    Some(stored.artifact_id.clone()),
                    Some(stored.semantic_element_id.clone()),
                    Some(stored.source_text.clone()),
                    Some(stored.vector.engine_id.clone()),
                    stored.vector.model.clone(),
                    Some(stored.vector.dimensions.to_string()),
                    Some(stored.vector.normalized.to_string()),
                    None,
                    Some(canonical_json_value(&stored.vector.metadata)?),
                ],
                &stored.vector.vector,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let element_vector_rows = element_name_vectors
        .iter()
        .filter(|vector| active_ids.contains(vector.semantic_element_id.as_str()))
        .map(|stored| {
            vector_row(
                &[
                    Some(stored.semantic_element_id.clone()),
                    Some(stored.source_text.clone()),
                    Some(stored.vector.engine_id.clone()),
                    stored.vector.model.clone(),
                    Some(stored.vector.dimensions.to_string()),
                    Some(stored.vector.normalized.to_string()),
                    None,
                    Some(canonical_json_value(&stored.vector.metadata)?),
                ],
                &stored.vector.vector,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let mut tables = Vec::with_capacity(9);
    for (name, rows) in [
        ("elements.parquet", elements),
        ("relationships.parquet", relationships),
        ("external_references.parquet", external_refs),
        ("artifacts.parquet", artifacts),
        ("path_index.parquet", path_rows),
        ("adjacency_index.parquet", adjacency),
    ] {
        let (_, schema_id, columns) = TABLE_SCHEMAS
            .iter()
            .find(|(table_name, _, _)| *table_name == name)
            .ok_or_else(|| DbError::invalid_value(name, "known PZ table"))?;
        let bytes = encode_table(columns, rows.len(), &rows)?;
        tables.push(TableBytes {
            name,
            schema_id,
            bytes,
            rows: rows.len() as u64,
        });
    }
    let typed_tables = [
        ("artifact_blobs.parquet", blob_rows),
        ("artifact_text_vectors.parquet", artifact_vector_rows),
        ("element_name_vectors.parquet", element_vector_rows),
    ];
    for (name, rows) in typed_tables {
        let (_, schema_id, columns) = TABLE_SCHEMAS
            .iter()
            .find(|(table_name, _, _)| *table_name == name)
            .ok_or_else(|| DbError::invalid_value(name, "known PZ table"))?;
        let bytes = encode_typed_table(columns, rows.len(), &rows)?;
        tables.push(TableBytes {
            name,
            schema_id,
            bytes,
            rows: rows.len() as u64,
        });
    }
    tables.sort_by_key(|table| {
        PZ_REQUIRED_ENTRIES
            .iter()
            .position(|entry| *entry == table.name)
            .unwrap_or_default()
    });
    Ok(tables)
}
pub(super) fn column_data_type(name: &str) -> DataType {
    match name {
        "start_line" | "end_line" | "content_size_bytes" | "byte_size" | "dimensions"
        | "row_group" => DataType::Int64,
        "normalized" => DataType::Boolean,
        "content" => DataType::Binary,
        "vector" => DataType::List(Arc::new(Field::new("item", DataType::Float32, true))),
        _ => DataType::Utf8,
    }
}

pub(super) fn parse_optional_i64(
    rows: &[Vec<Option<String>>],
    column: usize,
) -> Result<Vec<Option<i64>>> {
    rows.iter()
        .map(|row| {
            row[column]
                .as_deref()
                .map(|value| {
                    value
                        .parse::<i64>()
                        .map_err(|_| DbError::invalid_value(value, "integer parquet column"))
                })
                .transpose()
        })
        .collect()
}

pub(super) fn parse_optional_bool(
    rows: &[Vec<Option<String>>],
    column: usize,
) -> Result<Vec<Option<bool>>> {
    rows.iter()
        .map(|row| {
            row[column]
                .as_deref()
                .map(|value| {
                    value
                        .parse::<bool>()
                        .map_err(|_| DbError::invalid_value(value, "boolean parquet column"))
                })
                .transpose()
        })
        .collect()
}

pub(super) fn encode_table(
    columns: &[&str],
    row_count: usize,
    rows: &[Vec<Option<String>>],
) -> Result<Vec<u8>> {
    if rows.iter().any(|row| row.len() != columns.len()) {
        return Err(DbError::invalid_value("table row", "schema column count"));
    }
    let schema = Arc::new(Schema::new(
        columns
            .iter()
            .map(|name| Field::new(*name, column_data_type(name), true))
            .collect::<Vec<_>>(),
    ));
    let mut arrays = Vec::<ArrayRef>::with_capacity(columns.len());
    for (col, name) in columns.iter().enumerate() {
        match *name {
            "start_line" | "end_line" | "content_size_bytes" | "byte_size" | "dimensions"
            | "row_group" => {
                arrays.push(Arc::new(Int64Array::from(parse_optional_i64(rows, col)?)) as ArrayRef);
            }
            "normalized" => {
                arrays.push(
                    Arc::new(BooleanArray::from(parse_optional_bool(rows, col)?)) as ArrayRef,
                );
            }
            _ => {
                arrays.push(Arc::new(StringArray::from(
                    rows.iter()
                        .map(|row| row[col].as_deref())
                        .collect::<Vec<_>>(),
                )) as ArrayRef);
            }
        }
    }
    let batch = RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|e| DbError::invalid_value(e.to_string(), "valid parquet record batch"))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .set_max_row_group_row_count(Some(1024))
        .build();
    let mut writer = ArrowWriter::try_new(Vec::new(), schema, Some(props))
        .map_err(|e| DbError::invalid_value(e.to_string(), "parquet writer"))?;
    if row_count > 0 {
        writer
            .write(&batch)
            .map_err(|e| DbError::invalid_value(e.to_string(), "parquet row group"))?;
    }
    writer
        .into_inner()
        .map_err(|e| DbError::invalid_value(e.to_string(), "parquet close"))
}
pub(super) fn encode_typed_table(
    columns: &[&str],
    row_count: usize,
    rows: &[Vec<TypedCell>],
) -> Result<Vec<u8>> {
    if rows.iter().any(|row| row.len() != columns.len()) {
        return Err(DbError::invalid_value("table row", "schema column count"));
    }
    let schema = Arc::new(Schema::new(
        columns
            .iter()
            .map(|name| Field::new(*name, column_data_type(name), true))
            .collect::<Vec<_>>(),
    ));
    let mut arrays = Vec::<ArrayRef>::with_capacity(columns.len());
    for (col, name) in columns.iter().enumerate() {
        match *name {
            "content" => {
                let values = rows
                    .iter()
                    .map(|row| match &row[col] {
                        TypedCell::Null => None,
                        TypedCell::Binary(value) => Some(value.as_slice()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                arrays.push(Arc::new(BinaryArray::from(values)) as ArrayRef);
            }
            "vector" => {
                let values = rows.iter().map(|row| match &row[col] {
                    TypedCell::Vector(value) => {
                        Some(value.iter().copied().map(Some).collect::<Vec<_>>())
                    }
                    TypedCell::Null => None,
                    _ => None,
                });
                arrays.push(
                    Arc::new(ListArray::from_iter_primitive::<Float32Type, _, _>(values))
                        as ArrayRef,
                );
            }
            "dimensions" | "byte_size" => {
                arrays.push(Arc::new(Int64Array::from(
                    rows.iter()
                        .map(|row| match &row[col] {
                            TypedCell::Int64(value) => Some(*value),
                            TypedCell::Null => None,
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                )) as ArrayRef);
            }
            "normalized" => {
                arrays.push(Arc::new(BooleanArray::from(
                    rows.iter()
                        .map(|row| match &row[col] {
                            TypedCell::Bool(value) => Some(*value),
                            TypedCell::Text(value) => value.parse().ok(),
                            TypedCell::Null => None,
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                )) as ArrayRef);
            }
            _ => {
                arrays.push(Arc::new(StringArray::from(
                    rows.iter()
                        .map(|row| match &row[col] {
                            TypedCell::Text(value) => Some(value.as_str()),
                            TypedCell::Null => None,
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                )) as ArrayRef);
            }
        }
    }
    let batch = RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|e| DbError::invalid_value(e.to_string(), "valid parquet record batch"))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .set_max_row_group_row_count(Some(1024))
        .build();
    let mut writer = ArrowWriter::try_new(Vec::new(), schema, Some(props))
        .map_err(|e| DbError::invalid_value(e.to_string(), "parquet writer"))?;
    if row_count > 0 {
        writer
            .write(&batch)
            .map_err(|e| DbError::invalid_value(e.to_string(), "parquet row group"))?;
    }
    writer
        .into_inner()
        .map_err(|e| DbError::invalid_value(e.to_string(), "parquet close"))
}
pub(super) fn root_relative_path(path: &str, root: &str) -> Result<String> {
    let path = path.replace('\\', "/");
    let root = root.replace('\\', "/");
    let root = if root == "/" {
        root
    } else {
        root.trim_end_matches('/').to_owned()
    };
    let relative = if root == "/" {
        path.strip_prefix('/').unwrap_or(path.as_str())
    } else {
        path.strip_prefix(&format!("{root}/"))
            .unwrap_or(path.as_str())
    };
    if relative.starts_with('/')
        || relative.split('/').any(|segment| segment == "..")
        || relative.is_empty()
    {
        return Err(DbError::invalid_value(path, "root-relative semantic path"));
    }
    Ok(relative.to_owned())
}
pub(super) fn external_project_id(metadata: &serde_json::Value) -> Option<String> {
    ["foreign_project_id", "external_project_id", "project_id"]
        .iter()
        .find_map(|key| metadata.get(*key).and_then(serde_json::Value::as_str))
        .filter(|value| {
            Uuid::parse_str(value)
                .map(|id| id.get_version_num() == 4)
                .unwrap_or(false)
        })
        .map(str::to_owned)
}
