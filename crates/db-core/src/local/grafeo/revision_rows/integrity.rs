//! Header-only integrity operations reuse the ordinary revision row codec and atomic writer.
use super::*;
use std::collections::{BTreeSet, HashSet};

#[derive(Default)]
struct InspectedRevisionCatalog {
    headers: Vec<SemanticRevisionRecordHeader>,
    recovery_operations: Vec<SemanticRevisionAcceptedRecovery>,
}

pub(in super::super) fn inspect_revision_integrity(
    graph: &GrafeoDB,
    root: &str,
    project_id: &str,
    commit_version: i64,
) -> Result<SemanticRevisionIntegrityReport> {
    require_revision_project(graph, root, project_id)?;
    let mut missing_records = vec![];
    let mut acknowledged = vec![];
    let mut seen = BTreeSet::new();
    let catalog = project_integrity_catalog(graph, root)?;
    for header in catalog.headers {
        if !seen.insert(header.record.clone()) {
            return Err(DbError::invalid_value(
                &header.record.record_id,
                "one exact revision header per project record",
            ));
        }
        if let Some(absence) = header.payload_absence() {
            let stored = header.restore(absence)?;
            if matches!(stored.body, SemanticRevisionBody::Unavailable(_)) {
                acknowledged.push(stored);
            }
            continue;
        }
        match header.read_present_body(graph)? {
            Some(body) => {
                validate_present_body(&header, body, root)?;
            }
            None => missing_records.push(header),
        }
    }
    validate_existing_acknowledgements(
        root,
        project_id,
        commit_version,
        acknowledged,
        catalog.recovery_operations,
    )?;
    missing_records.sort_by(|left, right| left.record.cmp(&right.record));
    Ok(SemanticRevisionIntegrityReport {
        project_root: root.into(),
        project_id: project_id.into(),
        commit_version,
        missing_records,
        publication_inspection: None,
    })
}

fn validate_existing_acknowledgements(
    root: &str,
    project_id: &str,
    commit_version: i64,
    records: Vec<SemanticStoredRevision>,
    recovery_operations: Vec<SemanticRevisionAcceptedRecovery>,
) -> Result<()> {
    let catalog = SemanticRevisionState {
        project_root: root.into(),
        project_id: project_id.into(),
        commit_version,
        records,
        recovery_operations,
        ..Default::default()
    };
    Ok(catalog.validate_integrity_history()?)
}

fn validate_present_body(
    header: &SemanticRevisionRecordHeader,
    body: SemanticRevisionBody,
    root: &str,
) -> Result<()> {
    let restored = header.clone().restore(body)?;
    match restored.body {
        SemanticRevisionBody::Element(element) => {
            element.validate()?;
            let scoped = header.element_scope.as_ref().is_some_and(|scope| {
                scope.path == element.path
                    && scope.provider_instance_id.as_deref()
                        == element.metadata["provider_instance_id"].as_str()
            });
            if element.project_root != root || !scoped {
                return Err(DbError::invalid_value(
                    &header.record.record_id,
                    format!(
                        "a canonical body in project {root:?} matching retained path/provider ownership"
                    ),
                ));
            }
        }
        SemanticRevisionBody::Artifact(artifact) => artifact.validate()?,
        _ => {
            return Err(DbError::invalid_value(
                &header.record.record_id,
                "an available canonical body before absence inspection",
            ));
        }
    }
    Ok(())
}

fn require_revision_project(graph: &GrafeoDB, root: &str, project_id: &str) -> Result<()> {
    let nodes = revision_row_nodes(graph, root, "project", "")?;
    let [node] = nodes.as_slice() else {
        return Err(DbError::invalid_value(
            root,
            "one durable project revision identity before integrity inspection",
        ));
    };
    match revision_row_from_node(graph, *node)? {
        RevisionRow::Project(identity) if identity == project_id => Ok(()),
        other => Err(DbError::invalid_value(
            format!("{other:?}"),
            format!("revision identity matching durable project {project_id}"),
        )),
    }
}

fn project_integrity_catalog(graph: &GrafeoDB, root: &str) -> Result<InspectedRevisionCatalog> {
    let mut catalog = InspectedRevisionCatalog::default();
    let mut operation_ids = BTreeSet::new();
    // GRAFEO06-REVIEW(26): integrity inspection reads one committed snapshot; 0.5 filtered by epoch visibility.
    let snapshot = graph.committed_snapshot()?;
    let view = snapshot.view();
    let labeled: HashSet<_> = snapshot
        .nodes_by_label(ROW_LABEL, view)
        .db()?
        .into_iter()
        .collect();
    for node_id in graph
        .find_nodes_by_property(ROW_PROJECT, &Value::from(root))
        .db()?
    {
        if !labeled.contains(&node_id) || !snapshot.is_node_visible(node_id, view).db()? {
            continue;
        }
        // Ignored history/connection rows may carry large payloads; inspect their key before loading one.
        let key = snapshot
            .node_property(node_id, ROW_KEY, view)
            .db()?
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| {
                DbError::invalid_value(node_id.to_string(), "a canonical revision metadata key")
            })?;
        let (owner, kind, identity): (String, String, String) = serde_json::from_str(&key)?;
        if owner != root {
            return Err(DbError::invalid_value(
                key,
                format!("metadata owned by project {root:?}"),
            ));
        }
        require_unique_integrity_operation(&kind, &identity, &mut operation_ids)?;
        if !matches!(kind.as_str(), "record" | "recovery") {
            continue;
        }
        let row = revision_row_from_node(graph, node_id)?;
        if matches!(&row, RevisionRow::Record(header) if header.record.record_id.trim().is_empty())
        {
            return Err(DbError::invalid_value(
                "empty revision ID",
                "a nonempty exact revision header identity before integrity inspection",
            ));
        }
        if row.key(root)? != key {
            return Err(DbError::invalid_value(
                key,
                "canonical metadata identity matching its retained payload",
            ));
        }
        match row {
            RevisionRow::Record(header) if kind == "record" => catalog.headers.push(header),
            RevisionRow::Recovery(operation) if kind == "recovery" => {
                catalog.recovery_operations.push(operation)
            }
            _ => {
                return Err(DbError::invalid_value(
                    key,
                    "a header or recovery payload matching its canonical metadata kind",
                ));
            }
        }
    }
    Ok(catalog)
}

fn require_unique_integrity_operation(
    kind: &str,
    identity: &str,
    operation_ids: &mut BTreeSet<String>,
) -> Result<()> {
    if !matches!(kind, "operation" | "retention" | "recovery") {
        return Ok(());
    }
    if !operation_ids.insert(identity.into()) {
        return Err(DbError::invalid_value(
            identity,
            "one accepted publication, retention or recovery operation identity per project",
        ));
    }
    Ok(())
}

pub(in super::super) fn unavailable_selection_headers(
    graph: &GrafeoDB,
    root: &str,
    selection_id: &str,
) -> Result<Vec<SemanticRevisionRecordHeader>> {
    let mut headers = BTreeMap::new();
    for node in graph
        .find_nodes_by_property("revision_unavailable_project", &Value::from(root))
        .db()?
    {
        let RevisionRow::Record(header) = revision_row_from_node(graph, node)? else {
            return Err(DbError::invalid_value(
                node.to_string(),
                "an indexed unavailable revision header",
            ));
        };
        let Some(absence) = header.payload_absence() else {
            return Err(DbError::invalid_value(
                header.record.record_id,
                "declared unavailable revision metadata",
            ));
        };
        header.clone().restore(absence)?;
        if header.unavailable.is_none() {
            return Err(DbError::invalid_value(
                header.record.record_id,
                "unavailable state distinct from pruning",
            ));
        }
        let member = RevisionMembership {
            selection_id: selection_id.into(),
            member: RevisionMember::Record(header.record.clone()),
        };
        let identity = serde_json::to_string(&member)?;
        if !revision_row_nodes(graph, root, "membership", &identity)?.is_empty() {
            headers.insert(header.record.clone(), header);
        }
    }
    Ok(headers.into_values().collect())
}

pub(in super::super) fn prepare_recovery_rows(
    graph: &GrafeoDB,
    root: &str,
    plan: &SemanticRevisionRecoveryPlan,
) -> Result<PreparedRevisionRows> {
    let mut rows = plan
        .headers
        .iter()
        .cloned()
        .map(RevisionRow::Record)
        .collect::<Vec<_>>();
    rows.push(RevisionRow::Recovery(plan.accepted.clone()));
    let mut row_ids = BTreeMap::new();
    for row in &rows {
        let key = row.key(root)?;
        let nodes = revision_row_nodes(graph, root, row.kind(), &row.identity()?)?;
        if nodes.len() > 1 {
            return Err(DbError::invalid_value(
                key,
                "one existing metadata row per recovery identity",
            ));
        }
        if let Some(node) = nodes.first() {
            row_ids.insert(key, vec![*node]);
        }
    }
    let loaded = LoadedRevisionState {
        state: SemanticRevisionState {
            project_root: root.into(),
            ..Default::default()
        },
        adopted: false,
        row_ids,
    };
    loaded.prepare_rows(rows, vec![])
}
