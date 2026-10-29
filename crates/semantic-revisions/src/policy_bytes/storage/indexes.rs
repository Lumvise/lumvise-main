//! Lookup values are derived in Rust so JavaScript transports opaque strings, including NULs and i64s.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::policy_bytes) struct StorageIndex {
    pub(super) name: String,
    pub(super) value: String,
}

pub(super) fn entries(
    metadata: &SemanticRevisionMetadata,
    body: Option<&SemanticRevisionBody>,
) -> SemanticRevisionResult<Vec<StorageIndex>> {
    let mut entries = metadata_entries(metadata)?;
    if let Some(SemanticRevisionBody::Element(element)) = body {
        entries.push(entry("element_header", &super::headers::encode(element)?));
        entries.extend(super::names::indexes(element));
        if let Some((_, extension)) = element
            .path
            .rsplit('/')
            .next()
            .unwrap_or("")
            .rsplit_once('.')
        {
            entries.push(entry("element_extension", &extension.to_lowercase()));
        }
    }
    if let Some(SemanticRevisionBody::Artifact(artifact)) = body {
        if let Some(changes) = artifact
            .metadata
            .get("changes")
            .and_then(serde_json::Value::as_str)
        {
            entries.push(entry("artifact_changes", changes));
        }
        entries.push(entry("artifact_owner", &artifact.semantic_element_id));
        if let Some(author) = artifact
            .metadata
            .get("contributorSubjectHash")
            .and_then(serde_json::Value::as_str)
        {
            entries.push(entry("artifact_contributor", author));
        }
        for record in artifact.revision_dependencies() {
            entries.push(entry(
                "artifact_dependency",
                &serde_json::to_string(&record)?,
            ));
        }
        if let Some(reference) = &artifact.content_ref {
            entries.push(entry("content_ref", reference));
        }
    }
    Ok(entries)
}

fn metadata_entries(row: &SemanticRevisionMetadata) -> SemanticRevisionResult<Vec<StorageIndex>> {
    use SemanticRevisionMetadata::*;
    Ok(match row {
        Record(header) => header
            .element_scope
            .as_ref()
            .map(|scope| {
                let mut entries = vec![entry("record_path", &scope.path)];
                if let Some(provider) = &scope.provider_instance_id {
                    entries.push(entry("record_provider", provider));
                }
                entries
            })
            .unwrap_or_default(),
        Connection(connection) => {
            let relationship = &connection.relationship;
            let mut entries = vec![
                entry("connection_source", &relationship.source_element_id),
                entry("connection_target", &relationship.target_element_id),
            ];
            if relationship.relationship_kind == "contains" || relationship.label == "contains" {
                entries.push(entry("containment_target", &relationship.target_element_id));
            }
            entries
        }
        Selection(selection) => {
            let mut entries = vec![];
            if selection.mode == SemanticSelectionMode::FollowAccepted {
                entries.push(entry("follow_context", &selection.source_context));
            }
            if let Some(source) = super::contributions::views::source(&selection.source_context)? {
                entries.push(entry("challenge_source", &source));
            }
            if let Some(author) =
                super::contributions::views::contributor(&selection.source_context)?
            {
                entries.push(entry("challenge_author", &author));
            }
            entries
        }
        Membership(member) => vec![
            entry("selection_member", &member.selection_id),
            match &member.member {
                SemanticRevisionMember::Record(record) => {
                    entry("member_record", &serde_json::to_string(record)?)
                }
                SemanticRevisionMember::Connection(id) => entry("member_connection", id),
            },
        ],
        History(delta) => vec![entry("history_selection", &delta.selection.selection_id)],
        Link(link) => vec![
            entry("link_record", &serde_json::to_string(&link.older)?),
            entry(
                "link_older",
                &serde_json::to_string(&(&link.source_context, &link.older))?,
            ),
            entry(
                "link_newer",
                &serde_json::to_string(&(&link.source_context, &link.newer))?,
            ),
        ],
        Association(association) => vec![
            entry("association_element", &association.semantic_element_id),
            entry("association_artifact", &association.artifact_id),
            entry(
                "association_record",
                &serde_json::to_string(&SemanticRecordRef::artifact(&association.artifact_id))?,
            ),
            entry(
                "association_owner",
                &serde_json::to_string(&SemanticRecordRef::element(
                    &association.semantic_element_id,
                ))?,
            ),
        ],
        _ => vec![],
    })
}

fn entry(name: &str, value: &str) -> StorageIndex {
    StorageIndex {
        name: name.into(),
        value: value.into(),
    }
}
