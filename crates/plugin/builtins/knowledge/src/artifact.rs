use chrono::{DateTime, Utc};
use lumvise_contracts::{
    CreateKnowledgeArtifactRequestV2 as CreateRequest, KnowledgeArtifactLifecycleV2 as Lifecycle,
    KnowledgeArtifactStatusV2 as Status, UpdateKnowledgeArtifactRequestV2 as UpdateRequest,
};
use lumvise_plugin_sdk::PluginError;
use serde::Deserialize;

pub use lumvise_contracts::{
    KnowledgeArtifactKindV2 as KnowledgeKind, KnowledgeArtifactV2 as KnowledgeArtifact,
};

pub(crate) fn knowledge_type_label(kind: &KnowledgeKind) -> &'static str {
    match kind {
        KnowledgeKind::Specification => "specification",
        KnowledgeKind::Issue => "issue",
        KnowledgeKind::TaskAssignment => "task-assignment",
        KnowledgeKind::Definition => "definition",
        KnowledgeKind::Annotation => "annotation",
        KnowledgeKind::Report => "report",
        KnowledgeKind::Decision => "decision",
        KnowledgeKind::ManualNote => "manual-note",
        KnowledgeKind::DerivedSummary => "derived-summary",
        KnowledgeKind::Checkpoint => "checkpoint",
        KnowledgeKind::Question => "question",
    }
}

#[derive(Deserialize)]
pub(crate) struct SearchRequest {
    pub(crate) query: String,
    pub(crate) project_root: Option<String>,
    pub(crate) semantic_element_id: Option<String>,
    pub(crate) knowledge_type: Option<KnowledgeKind>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) include_inactive: Option<bool>,
}

pub(crate) fn create(request: CreateRequest) -> Result<KnowledgeArtifact, PluginError> {
    require(&request.artifact_id, "artifact_id")?;
    require(&request.semantic_element_id, "semantic_element_id")?;
    require(&request.title, "title")?;
    require(&request.content, "content")?;
    lumvise_contracts::validate_artifact_dependencies(&request.artifact_id, &request.dependencies)
        .map_err(|error| PluginError::new("invalid_knowledge_input", error, false))?;
    let artifact = KnowledgeArtifact {
        artifact_id: request.artifact_id,
        semantic_element_id: request.semantic_element_id,
        knowledge_type: request.knowledge_type,
        title: request.title,
        content: request.content,
        tags: request.tags,
        dependencies: request.dependencies,
        metadata: request.metadata,
        path: request.path,
        project_root: request.project_root,
        lifecycle: Lifecycle {
            expires_at: request
                .expires_at
                .as_deref()
                .map(normalize_expiry)
                .transpose()?,
            ..Default::default()
        },
    };
    artifact
        .lifecycle
        .validate(&artifact.artifact_id)
        .map_err(|error| PluginError::new("invalid_knowledge_input", error, false))?;
    Ok(artifact)
}

pub(crate) fn update(
    mut artifact: KnowledgeArtifact,
    request: UpdateRequest,
) -> Result<KnowledgeArtifact, PluginError> {
    // shortcut: read-check-write is not atomic across concurrent invocations, move the comparison into storage.semantic if agents need real claims
    if let Some(expected) = request
        .expected_revision
        .filter(|expected| *expected != artifact.lifecycle.revision)
    {
        return Err(PluginError::new(
            "knowledge_revision_conflict",
            format!(
                "Knowledge artifact `{}` is at revision {}; expected revision {expected}; re-read the artifact, merge your change and retry",
                artifact.artifact_id, artifact.lifecycle.revision
            ),
            false,
        ));
    }
    replace_nonempty(
        &mut artifact.semantic_element_id,
        request.semantic_element_id,
        "semantic_element_id",
    )?;
    if let Some(value) = request.knowledge_type {
        artifact.knowledge_type = value;
    }
    replace_nonempty(&mut artifact.title, request.title, "title")?;
    replace_nonempty(&mut artifact.content, request.content, "content")?;
    if let Some(dependencies) = request.dependencies {
        artifact.dependencies = dependencies;
    }
    lumvise_contracts::validate_artifact_dependencies(
        &artifact.artifact_id,
        &artifact.dependencies,
    )
    .map_err(|error| PluginError::new("invalid_knowledge_input", error, false))?;
    artifact.tags = request.tags.unwrap_or(artifact.tags);
    artifact.metadata = request.metadata.unwrap_or(artifact.metadata);
    artifact.path = request.path.or(artifact.path);
    artifact.project_root = request.project_root.or(artifact.project_root);
    if let Some(status) = request.status {
        artifact.lifecycle.status = status;
        artifact.lifecycle.expires_at = None;
        if status != Status::Superseded {
            artifact.lifecycle.superseded_by = None;
        }
    }
    if let Some(target) = request.superseded_by {
        artifact.lifecycle.superseded_by = Some(target);
    }
    if let Some(value) = request.expires_at.as_deref() {
        artifact.lifecycle.expires_at = Some(normalize_expiry(value)?);
    }
    artifact
        .lifecycle
        .validate(&artifact.artifact_id)
        .map_err(|error| PluginError::new("invalid_knowledge_input", error, false))?;
    Ok(artifact)
}

/// Sets the write metadata that storage owns: revision, creation, update and
/// status-change instants. A record without a previous write starts at revision 1.
pub(crate) fn stamp(
    artifact: &mut KnowledgeArtifact,
    previous: Option<&Lifecycle>,
    now: DateTime<Utc>,
) {
    let now = now.to_rfc3339();
    let lifecycle = &mut artifact.lifecycle;
    lifecycle.revision = previous.map_or(0, |previous| previous.revision) + 1;
    lifecycle.created_at = match previous {
        Some(previous) => previous.created_at.clone(),
        None => Some(now.clone()),
    };
    lifecycle.status_changed_at = match previous {
        Some(previous) if previous.status != lifecycle.status => Some(now.clone()),
        Some(previous) => previous.status_changed_at.clone(),
        None => lifecycle.status_changed_at.clone(),
    };
    lifecycle.updated_at = Some(now);
}

/// Record equality that ignores the write stamps storage advances on every write.
pub(crate) fn same_content(left: &KnowledgeArtifact, right: &KnowledgeArtifact) -> bool {
    let unstamped = |artifact: &KnowledgeArtifact| KnowledgeArtifact {
        lifecycle: Lifecycle {
            revision: 0,
            created_at: None,
            updated_at: None,
            status_changed_at: None,
            ..artifact.lifecycle.clone()
        },
        ..artifact.clone()
    };
    unstamped(left) == unstamped(right)
}

/// Active and not past its expiry. An expiry that does not parse never hides a record.
pub(crate) fn is_active(artifact: &KnowledgeArtifact, now: DateTime<Utc>) -> bool {
    let expired = artifact
        .lifecycle
        .expires_at
        .as_deref()
        .is_some_and(|expires_at| {
            DateTime::parse_from_rfc3339(expires_at).is_ok_and(|expires_at| expires_at <= now)
        });
    artifact.lifecycle.status == Status::Active && !expired
}

fn normalize_expiry(value: &str) -> Result<String, PluginError> {
    DateTime::parse_from_rfc3339(value)
        .map(|expires_at| expires_at.with_timezone(&Utc).to_rfc3339())
        .map_err(|_| {
            PluginError::new(
                "invalid_knowledge_input",
                format!("invalid `expires_at` value `{value}`; expected RFC 3339 timestamp"),
                false,
            )
        })
}

pub(crate) fn filter_matches(artifact: &KnowledgeArtifact, request: &SearchRequest) -> bool {
    matches_filter(
        artifact,
        request.project_root.as_ref(),
        request.semantic_element_id.as_ref(),
        request.knowledge_type.as_ref(),
    ) && is_discoverable(artifact, request.include_inactive)
}

/// Discovery default: inactive records are hidden unless the caller asks for them.
pub(crate) fn is_discoverable(
    artifact: &KnowledgeArtifact,
    include_inactive: Option<bool>,
) -> bool {
    include_inactive == Some(true) || is_active(artifact, Utc::now())
}

/// Shared search/list filter over the fields both listings carry; `None`
/// filters match everything. Project scope is exact equality on the record's
/// `project_root` — the semantics `search_knowledge` has always applied.
pub(crate) fn matches_filter(
    artifact: &KnowledgeArtifact,
    project_root: Option<&String>,
    semantic_element_id: Option<&String>,
    knowledge_type: Option<&KnowledgeKind>,
) -> bool {
    project_root.is_none_or(|value| artifact.project_root.as_ref() == Some(value))
        && semantic_element_id.is_none_or(|value| &artifact.semantic_element_id == value)
        && knowledge_type.is_none_or(|value| &artifact.knowledge_type == value)
}

/// Input of the `list_knowledge` export: every field optional.
#[derive(Deserialize)]
pub(crate) struct ListAllRequest {
    pub(crate) semantic_element_ids: Option<std::collections::HashSet<String>>,
    pub(crate) project_root: Option<String>,
    pub(crate) knowledge_type: Option<KnowledgeKind>,
    pub(crate) tags: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[serde(default)]
    pub(crate) include_inactive: Option<bool>,
}

/// Applies the `list_knowledge` filters in memory: project scope, knowledge
/// type, tags (any of the requested tags), then the result limit.
pub(crate) fn filter_artifacts(
    artifacts: Vec<KnowledgeArtifact>,
    request: &ListAllRequest,
) -> Vec<KnowledgeArtifact> {
    let mut filtered: Vec<KnowledgeArtifact> = artifacts
        .into_iter()
        .filter(|artifact| {
            matches_filter(
                artifact,
                request.project_root.as_ref(),
                None,
                request.knowledge_type.as_ref(),
            ) && request
                .semantic_element_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&artifact.semantic_element_id))
                && request.tags.as_ref().is_none_or(|tags| {
                    tags.iter()
                        .any(|tag| artifact.tags.iter().any(|own| own == tag))
                })
                && is_discoverable(artifact, request.include_inactive)
        })
        .collect();
    if let Some(limit) = request.limit {
        filtered.truncate(limit);
    }
    filtered
}

pub(crate) fn require(value: &str, field: &str) -> Result<(), PluginError> {
    if !value.trim().is_empty() {
        return Ok(());
    }
    Err(PluginError::new(
        "invalid_knowledge_input",
        format!("invalid `{field}` value `{value}`; expected non-empty string"),
        false,
    ))
}

fn replace_nonempty(
    current: &mut String,
    replacement: Option<String>,
    field: &str,
) -> Result<(), PluginError> {
    if let Some(value) = replacement {
        require(&value, field)?;
        *current = value;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn artifact(value: Value) -> KnowledgeArtifact {
        serde_json::from_value(value).expect("decode KnowledgeArtifactV2")
    }

    #[test]
    fn list_all_filter_applies_project_scope_type_tag_and_limit() {
        let artifacts = vec![
            artifact(json!({
                "artifact_id": "c4", "semantic_element_id": "file:src/a.rs",
                "knowledge_type": "report", "title": "T", "content": "",
                "tags": ["scoped-c4", "nucleus"], "dependencies": [],
                "metadata": {}, "project_root": "/p1"
            })),
            artifact(json!({
                "artifact_id": "canvas", "semantic_element_id": "folder:src",
                "knowledge_type": "definition", "title": "T", "content": "",
                "tags": ["canvas"], "dependencies": [],
                "metadata": {}, "project_root": "/p1"
            })),
            artifact(json!({
                "artifact_id": "other", "semantic_element_id": "file:lib.rs",
                "knowledge_type": "report", "title": "T", "content": "",
                "tags": ["scoped-c4"], "dependencies": [],
                "metadata": {}, "project_root": "/p2"
            })),
            artifact(json!({
                "artifact_id": "unscoped", "semantic_element_id": "file:lib.rs",
                "knowledge_type": "report", "title": "T", "content": "",
                "tags": ["scoped-c4"], "dependencies": [],
                "metadata": {}, "project_root": null
            })),
        ];
        let no_filter = ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: None,
            tags: None,
            limit: None,
            include_inactive: None,
        };
        assert_eq!(filter_artifacts(artifacts.clone(), &no_filter).len(), 4);

        // Project scope: records whose `project_root` equals the argument.
        // Unscoped records carry no project scope and do not match a root
        // filter — the same semantics `search_knowledge` applies.
        let scoped = ListAllRequest {
            semantic_element_ids: None,
            project_root: Some("/p1".into()),
            knowledge_type: None,
            tags: None,
            limit: None,
            include_inactive: None,
        };
        assert_eq!(
            filter_artifacts(artifacts.clone(), &scoped)
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c4", "canvas"]
        );

        let typed = ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: Some(serde_json::from_value(json!("definition")).expect("valid kind")),
            tags: None,
            limit: None,
            include_inactive: None,
        };
        assert_eq!(
            filter_artifacts(artifacts.clone(), &typed)
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>(),
            vec!["canvas"]
        );

        // Tags match when the record carries any requested tag.
        let tagged = ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: None,
            tags: Some(vec!["canvas".into(), "absent".into()]),
            limit: None,
            include_inactive: None,
        };
        assert_eq!(
            filter_artifacts(artifacts.clone(), &tagged)
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>(),
            vec!["canvas"]
        );

        let limited = ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: None,
            tags: None,
            limit: Some(2),
            include_inactive: None,
        };
        assert_eq!(filter_artifacts(artifacts, &limited).len(), 2);
    }

    fn record(id: &str, lifecycle: Value) -> KnowledgeArtifact {
        let mut value = json!({
            "artifact_id": id, "semantic_element_id": "file:src/a.rs",
            "knowledge_type": "decision", "title": "T", "content": "C",
            "tags": [], "dependencies": [], "metadata": {}, "project_root": "/p1"
        });
        value
            .as_object_mut()
            .expect("record object")
            .extend(lifecycle.as_object().expect("lifecycle fields").clone());
        artifact(value)
    }

    fn apply_update(
        current: KnowledgeArtifact,
        fields: Value,
    ) -> Result<KnowledgeArtifact, PluginError> {
        let mut request = json!({"artifact_id": current.artifact_id});
        request
            .as_object_mut()
            .expect("request object")
            .extend(fields.as_object().expect("update fields").clone());
        update(
            current,
            serde_json::from_value(request).expect("update request"),
        )
    }

    #[test]
    fn status_change_clears_expiry_and_superseded_status_needs_target() {
        let timed = create(
            serde_json::from_value(json!({
                "artifact_id": "a", "semantic_element_id": "file:src/a.rs",
                "knowledge_type": "decision", "title": "T", "content": "C",
                "expires_at": "2027-01-01T02:00:00+02:00"
            }))
            .expect("create request"),
        )
        .expect("create");
        assert_eq!(
            timed.lifecycle.expires_at.as_deref(),
            Some("2027-01-01T00:00:00+00:00")
        );
        let resolved = apply_update(timed, json!({"status": "resolved"})).expect("resolve");
        assert_eq!(resolved.lifecycle.status, Status::Resolved);
        assert_eq!(resolved.lifecycle.expires_at, None);

        let error = apply_update(resolved.clone(), json!({"status": "superseded"}))
            .expect_err("superseded requires a target");
        assert_eq!(error.code, "invalid_knowledge_input");
        let superseded = apply_update(
            resolved,
            json!({"status": "superseded", "superseded_by": "b"}),
        )
        .expect("supersede");
        assert_eq!(superseded.lifecycle.superseded_by.as_deref(), Some("b"));

        let unclaimed = apply_update(superseded, json!({"status": "archived"})).expect("archive");
        assert_eq!(unclaimed.lifecycle.superseded_by, None);
    }

    #[test]
    fn stale_expected_revision_is_rejected_before_any_field_changes() {
        let current = record("a", json!({"revision": 2}));
        let error = apply_update(
            current.clone(),
            json!({"title": "New", "expected_revision": 1}),
        )
        .expect_err("stale read");
        assert_eq!(error.code, "knowledge_revision_conflict");
        assert!(error.message.contains("`a`") && error.message.contains("revision 2"));
        assert_eq!(current.title, "T");

        let updated = apply_update(current, json!({"title": "New", "expected_revision": 2}))
            .expect("fresh read");
        assert_eq!(updated.title, "New");
        assert_eq!(updated.lifecycle.revision, 2);
    }

    #[test]
    fn discovery_filters_hide_inactive_records_until_requested() {
        let artifacts = vec![
            record("active", json!({})),
            record("resolved", json!({"status": "resolved"})),
            record(
                "expired",
                json!({"expires_at": "2000-01-01T00:00:00+00:00"}),
            ),
        ];
        let list = |include_inactive| ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: None,
            tags: None,
            limit: None,
            include_inactive,
        };
        let ids = |artifacts: Vec<KnowledgeArtifact>| {
            artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(filter_artifacts(artifacts.clone(), &list(None))),
            ["active"]
        );
        assert_eq!(
            ids(filter_artifacts(artifacts.clone(), &list(Some(true)))),
            ["active", "resolved", "expired"]
        );

        let search = |include_inactive: Option<bool>| {
            serde_json::from_value::<SearchRequest>(json!({
                "query": "t", "include_inactive": include_inactive
            }))
            .expect("search request")
        };
        assert!(filter_matches(&artifacts[0], &search(None)));
        assert!(!filter_matches(&artifacts[1], &search(None)));
        assert!(filter_matches(&artifacts[1], &search(Some(true))));
    }

    #[test]
    fn stamp_advances_revision_and_marks_only_real_status_changes() {
        let now = DateTime::parse_from_rfc3339("2026-10-13T08:00:00Z")
            .expect("instant")
            .with_timezone(&Utc);
        let later = now + chrono::Duration::hours(1);

        let mut first = record("a", json!({}));
        stamp(&mut first, None, now);
        assert_eq!(first.lifecycle.revision, 1);
        assert_eq!(first.lifecycle.created_at, Some(now.to_rfc3339()));
        assert_eq!(first.lifecycle.status_changed_at, None);

        let mut same = record("a", json!({}));
        stamp(&mut same, Some(&first.lifecycle), later);
        assert_eq!(same.lifecycle.revision, 2);
        assert_eq!(same.lifecycle.created_at, first.lifecycle.created_at);
        assert_eq!(same.lifecycle.updated_at, Some(later.to_rfc3339()));
        assert_eq!(same.lifecycle.status_changed_at, None);

        let mut resolved = record("a", json!({"status": "resolved"}));
        stamp(&mut resolved, Some(&same.lifecycle), later);
        assert_eq!(resolved.lifecycle.revision, 3);
        assert_eq!(
            resolved.lifecycle.status_changed_at,
            Some(later.to_rfc3339())
        );

        let mut legacy = record("a", json!({}));
        stamp(&mut legacy, Some(&Lifecycle::default()), later);
        assert_eq!(legacy.lifecycle.revision, 1);
        assert_eq!(legacy.lifecycle.created_at, None);
    }
}
