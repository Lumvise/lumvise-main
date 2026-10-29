//! Loads scoped family headers only; selection policy remains in the shared Rust owner.
use super::{
    revision_rows::{self, RevisionRow},
    semantic_storage::SemanticStorage,
};
use crate::local::sql::commits::latest_published_commit_version;
use crate::*;
use grafeo::{GrafeoDB, Value};
use std::collections::BTreeSet;

impl SemanticStorage<'_> {
    pub(crate) fn revision_families(
        &self,
        query: &SemanticRevisionFamilyQuery,
    ) -> Result<Vec<SemanticRevisionRemap>> {
        self.graph.stable_read(|graph| {
            let basis = if revision_rows::revision_project_managed(graph, &query.project_root)? {
                load_family_basis(
                    graph,
                    query,
                    latest_published_commit_version(&self.conn.read_conn())?,
                )?
            } else {
                let state = self.load_revisions(graph, &query.project_root)?.state;
                let head = state.selection(&query.selection_id);
                SemanticRevisionFamilyBasis {
                    project_root: query.project_root.clone(),
                    commit_version: state.commit_version,
                    selection: head.map(|head| head.selection.clone()),
                    selected_records: head.map_or_else(Vec::new, |head| head.records.clone()),
                    links: state.revision_links,
                }
            };
            Ok(basis.resolve(query)?)
        })
    }
}

fn load_family_basis(
    graph: &GrafeoDB,
    query: &SemanticRevisionFamilyQuery,
    commit_version: i64,
) -> Result<SemanticRevisionFamilyBasis> {
    let selection = selection_metadata(graph, query)?;
    let mut basis = SemanticRevisionFamilyBasis {
        project_root: query.project_root.clone(),
        commit_version,
        selection,
        selected_records: vec![],
        links: vec![],
    };
    let mut pending = query.records.clone();
    let mut visited = BTreeSet::new();
    while let Some(record) = pending.pop() {
        if visited.insert(record.clone()) {
            pending.extend(include_family_record(graph, query, &record, &mut basis)?);
        }
    }
    Ok(basis)
}

fn selection_metadata(
    graph: &GrafeoDB,
    query: &SemanticRevisionFamilyQuery,
) -> Result<Option<SemanticRevisionSelection>> {
    let nodes = revision_rows::revision_row_nodes(
        graph,
        &query.project_root,
        "selection",
        &query.selection_id,
    )?;
    let [node] = nodes.as_slice() else {
        return if nodes.is_empty() {
            Ok(None)
        } else {
            Err(DbError::invalid_value(
                &query.selection_id,
                "one selection metadata row",
            ))
        };
    };
    match revision_rows::revision_row_from_node(graph, *node)? {
        RevisionRow::Selection(selection) => Ok(Some(selection)),
        row => Err(DbError::invalid_value(row.kind(), "selection metadata")),
    }
}

fn include_family_record(
    graph: &GrafeoDB,
    query: &SemanticRevisionFamilyQuery,
    record: &SemanticRecordRef,
    basis: &mut SemanticRevisionFamilyBasis,
) -> Result<Vec<SemanticRecordRef>> {
    let Some(selection) = &basis.selection else {
        return Ok(vec![]);
    };
    if selected_record(graph, query, record)? {
        basis.selected_records.push(record.clone());
        return Ok(vec![]);
    }
    let links = successor_links(
        graph,
        &query.project_root,
        &selection.source_context,
        record,
    )?;
    let pending = links.iter().map(|link| link.newer.clone()).collect();
    basis.links.extend(links);
    Ok(pending)
}

fn selected_record(
    graph: &GrafeoDB,
    query: &SemanticRevisionFamilyQuery,
    record: &SemanticRecordRef,
) -> Result<bool> {
    let expected = RevisionRow::Membership(SemanticRevisionMembership {
        selection_id: query.selection_id.clone(),
        member: SemanticRevisionMember::Record(record.clone()),
    });
    let identity = expected.identity()?;
    let nodes =
        revision_rows::revision_row_nodes(graph, &query.project_root, "membership", &identity)?;
    match nodes.as_slice() {
        [] => Ok(false),
        [node]
            if matches!(revision_rows::revision_row_from_node(graph, *node)?,
            RevisionRow::Membership(stored) if stored.selection_id == query.selection_id && stored.record() == Some(record)) =>
        {
            Ok(true)
        }
        _ => Err(DbError::invalid_value(
            identity,
            "one exact selection membership row",
        )),
    }
}

fn successor_links(
    graph: &GrafeoDB,
    root: &str,
    context: &str,
    record: &SemanticRecordRef,
) -> Result<Vec<SemanticRevisionLink>> {
    let key = serde_json::to_string(&(root, context, record))?;
    graph
        .find_nodes_by_property("revision_link_older", &Value::from(key.as_str()))
        .into_iter()
        .map(
            |node| match revision_rows::revision_row_from_node(graph, node)? {
                RevisionRow::Link(link)
                    if link.project_root == root
                        && link.source_context == context
                        && link.older == *record =>
                {
                    Ok(link)
                }
                _ => Err(DbError::invalid_value(
                    &key,
                    "a successor link in this project and source context",
                )),
            },
        )
        .collect()
}
