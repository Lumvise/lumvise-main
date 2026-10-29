//! Commit-scoped artifact bytes travel with the immutable record locator, including working edits.
use super::*;
use std::collections::BTreeMap;
pub(super) mod attachments;
pub(super) mod staged;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredBody {
    pub record: SemanticRevisionBody,
    pub content: Option<StoredContent>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredContent {
    pub content_ref: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub updated_at: Option<String>,
}

pub(super) fn encode(record: &SemanticRevisionBody) -> SemanticRevisionResult<String> {
    Ok(serde_json::to_string(&StoredBody {
        record: record.clone(),
        content: None,
    })?)
}

pub(super) fn decode(body: &str) -> SemanticRevisionResult<StoredBody> {
    let body: StoredBody = serde_json::from_str(body)?;
    if let Some(content) = &body.content {
        let SemanticRevisionBody::Artifact(artifact) = &body.record else {
            return Err(SemanticRevisionError::invalid_value(
                "element content",
                "content attached to an artifact body",
            ));
        };
        if artifact.content_ref.as_ref() != Some(&content.content_ref)
            || artifact.content_size_bytes != Some(content.bytes.len())
        {
            return Err(SemanticRevisionError::invalid_value(
                &content.content_ref,
                "content matching the artifact reference and byte length",
            ));
        }
    }
    Ok(body)
}

pub(super) fn attach(
    delta_json: &str,
    request_json: Option<&str>,
    rows: &[StorageRow],
) -> SemanticRevisionResult<StorageResult> {
    let mut delta: StorageResult = serde_json::from_str(delta_json)?;
    let StorageResult::Delta { rows: changes, .. } = &mut delta else {
        return Err(SemanticRevisionError::invalid_value(
            delta_json,
            "a storage delta",
        ));
    };
    let mut contents = available(rows)?;
    for (reference, body) in available(changes)? {
        insert(contents.entry(reference), body)?;
    }
    if let Some(request) = request_json {
        incoming(request, &mut contents)?;
    }
    for row in changes.iter_mut().filter(|row| row.key.kind == "record") {
        let Some(encoded) = &row.body_json else {
            continue;
        };
        let mut body = decode(encoded)?;
        if let SemanticRevisionBody::Artifact(artifact) = &body.record {
            body.content = artifact
                .content_ref
                .as_ref()
                .map(|reference| {
                    contents.get(reference).cloned().ok_or_else(|| {
                        SemanticRevisionError::invalid_value(
                            reference,
                            "available exact artifact content before publication",
                        )
                    })
                })
                .transpose()?;
        }
        let encoded = serde_json::to_string(&body)?;
        decode(&encoded)?;
        row.body_json = Some(encoded);
    }
    let claimed = staged::claimed(&delta, rows, request_json)?;
    attachments::finish(&mut delta, rows, request_json, claimed)?;
    Ok(delta)
}

pub(super) fn available(
    rows: &[StorageRow],
) -> SemanticRevisionResult<BTreeMap<String, StoredContent>> {
    let mut contents = BTreeMap::new();
    for row in rows.iter().filter(|row| row.key.kind == "record") {
        if let Some(body) = &row.body_json {
            if let Some(content) = decode(body)?.content {
                insert(contents.entry(content.content_ref.clone()), content)?;
            }
        }
    }
    staged::append(rows, &mut contents)?;
    attachments::append(rows, &mut contents)?;
    Ok(contents)
}

pub(super) fn insert(
    entry: std::collections::btree_map::Entry<String, StoredContent>,
    content: StoredContent,
) -> SemanticRevisionResult<()> {
    if matches!(&entry, std::collections::btree_map::Entry::Occupied(previous) if previous.get().bytes != content.bytes || previous.get().media_type != content.media_type)
    {
        return Err(SemanticRevisionError::invalid_value(
            &content.content_ref,
            "immutable content bytes and media type",
        ));
    }
    entry.or_insert(content);
    Ok(())
}

fn incoming(
    request_json: &str,
    contents: &mut BTreeMap<String, StoredContent>,
) -> SemanticRevisionResult<()> {
    let request: serde_json::Value = serde_json::from_str(request_json)?;
    if request["operation"] == "import_archive" {
        return super::archives::contents(request_json, contents);
    }
    if request["operation"] != "publication" {
        return Ok(());
    }
    let publication: SemanticRevisionPublication =
        serde_json::from_value(request["request"].clone())?;
    for write in publication.artifacts {
        let Some(text) = &write.artifact.content else {
            continue;
        };
        let content_ref = write
            .stored_artifact(&publication.project_id)?
            .content_ref
            .expect("inline content reference");
        insert(
            contents.entry(content_ref.clone()),
            StoredContent {
                content_ref,
                media_type: write.media_type,
                bytes: text.as_bytes().to_vec(),
                updated_at: None,
            },
        )?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BlobRequest {
    #[serde(rename = "operation")]
    pub _operation: String,
    pub selection_id: Option<String>,
    pub content_ref: String,
}

pub(super) fn read_blob(
    state: &SemanticRevisionState,
    input: &str,
    rows: &[StorageRow],
    published_at: &str,
    actor: Option<&str>,
) -> SemanticRevisionResult<StorageResult> {
    let request: BlobRequest = serde_json::from_str(input)?;
    let snapshot = request
        .selection_id
        .as_ref()
        .map(|id| state.snapshot(id, published_at))
        .transpose()?
        .flatten();
    let artifact = snapshot
        .as_ref()
        .and_then(|snapshot| {
            snapshot.artifacts.iter().find(|artifact| {
                artifact.content_ref.as_ref() == Some(&request.content_ref)
                    || owns_attachment(rows, &artifact.artifact_id, &request.content_ref)
            })
        })
        .or_else(|| {
            request
                .selection_id
                .is_none()
                .then(|| {
                    state.records.iter().find_map(|record| match &record.body {
                        SemanticRevisionBody::Artifact(artifact)
                            if artifact.content_ref.as_ref() == Some(&request.content_ref)
                                || owns_attachment(
                                    rows,
                                    &artifact.artifact_id,
                                    &request.content_ref,
                                ) =>
                        {
                            Some(artifact)
                        }
                        _ => None,
                    })
                })
                .flatten()
        });
    let Some(artifact) = artifact else {
        if request.selection_id.is_none() {
            if let Some(blob) = attachments::retained_blob(state, rows, &request.content_ref)? {
                return encoded("blob", &Some(blob));
            }
            return encoded("blob", &staged::read(rows, &request.content_ref, actor)?);
        }
        return encoded("blob", &Option::<serde_json::Value>::None);
    };
    let contents = available(rows)?;
    let content = contents.get(&request.content_ref).ok_or_else(|| {
        SemanticRevisionError::invalid_value(
            &request.content_ref,
            "available committed artifact bytes",
        )
    })?;
    encoded(
        "blob",
        &serde_json::json!({"content_ref":content.content_ref,"artifact_id":artifact.artifact_id,"media_type":content.media_type,"content":content.bytes,"updated_at":content.updated_at.as_deref().unwrap_or(published_at)}),
    )
}

fn owns_attachment(rows: &[StorageRow], owner: &str, reference: &str) -> bool {
    rows.iter()
        .filter(|row| attachments::is_attachment(row) && row.key.identity == reference)
        .any(|row| attachments::decode(row).is_ok_and(|blob| blob.artifact_id == owner))
}
