//! Indexed fact loading for small publications; the shared Rust policy owns every revision decision.
use super::revision_rows::{self, LoadedRevisionState, RevisionRow};
use crate::*;
use grafeo::{GrafeoDB, NodeId, Value};
use std::collections::{BTreeMap, BTreeSet};

struct RevisionBasisRows<'a> {
    graph: &'a GrafeoDB,
    root: &'a str,
    context: &'a str,
    nodes: BTreeSet<NodeId>,
    records: BTreeMap<SemanticRecordRef, SemanticStoredRevision>,
    families: BTreeSet<SemanticRecordRef>,
    connections: BTreeSet<String>,
}

pub(super) fn load_connection_revision_state(
    graph: &GrafeoDB,
    query: &SemanticRevisionConnectionQuery,
) -> Result<SemanticRevisionState> {
    let mut nodes = revision_rows::revision_row_nodes(graph, &query.project_root, "project", "")?;
    for id in query.connection_ids.iter().collect::<BTreeSet<_>>() {
        nodes.extend(revision_rows::revision_row_nodes(
            graph,
            &query.project_root,
            "connection",
            id,
        )?);
    }
    let loaded = revision_rows::load_revision_nodes(
        graph,
        &query.project_root,
        0,
        0,
        nodes,
        None,
        BTreeMap::new(),
    )?;
    Ok(loaded.state)
}

pub(super) fn load_publication_basis(
    graph: &GrafeoDB,
    store: &super::graph_store::GraphStore,
    request: &SemanticRevisionPublication,
    commit_version: i64,
    now_ms: i64,
) -> Result<Option<LoadedRevisionState>> {
    if request
        .structure
        .as_ref()
        .is_some_and(|structure| structure.partition.is_none())
        || !revision_rows::revision_project_managed(graph, &request.project_root)?
    {
        return Ok(None);
    }
    let mut rows = RevisionBasisRows::new(graph, &request.project_root, &request.source_context);
    rows.row("project", "")?;
    rows.row("selection", &request.selection_id)?;
    if let Some(base) = &request.producer_base {
        rows.row("selection", &base.selection_id)?;
    }
    rows.row("operation", &request.operation_id)?;
    rows.row("retention", &request.operation_id)?;
    let context_key = serde_json::to_string(&(&request.project_root, &request.source_context))?;
    rows.indexed("revision_follow_context", &context_key);
    if let Some(structure) = &request.structure {
        if let Some(partition) = &structure.partition {
            rows.structure(store, partition, &request.selection_id)?;
        }
        for element in &structure.elements {
            rows.family(
                &SemanticRecordRef::element(&element.semantic_element_id),
                &request.selection_id,
            )?;
            for required in element.required_revisions() {
                rows.record(&required)?;
            }
        }
        for connection in &structure.relationships {
            rows.record(&SemanticRecordRef::element(&connection.source_element_id))?;
            rows.record(&SemanticRecordRef::element(&connection.target_element_id))?;
        }
        rows.published_connection_definitions(&structure.relationships)?;
    }
    for id in lumvise_semantic_revisions::requested_artifact_ids(request) {
        rows.family(&SemanticRecordRef::artifact(id), &request.selection_id)?;
    }
    for write in &request.artifacts {
        rows.family(
            &SemanticRecordRef::element(&write.artifact.semantic_element_id),
            &request.selection_id,
        )?;
        for reference in write.artifact.revision_dependencies() {
            rows.record(&reference)?;
        }
    }
    rows.memberships()?;
    Ok(Some(revision_rows::load_revision_nodes(
        graph,
        &request.project_root,
        commit_version,
        now_ms,
        rows.nodes.into_iter().collect(),
        None,
        BTreeMap::new(),
    )?))
}

pub(super) fn load_selected_revision_state(
    graph: &GrafeoDB,
    root: &str,
    selection_id: &str,
    commit_version: i64,
) -> Result<SemanticRevisionState> {
    if !revision_rows::revision_project_managed(graph, root)? {
        return Err(DbError::invalid_value(
            root,
            "a managed revision project for indexed selection reads",
        ));
    }
    let mut rows = RevisionBasisRows::new(graph, root, "");
    rows.row("project", "")?;
    let key = serde_json::to_string(&(root, selection_id))?;
    let selected = rows.indexed("revision_selection_key", &key);
    for row in revision_rows::revision_rows_from_nodes(graph, &selected)? {
        match row {
            RevisionRow::Membership(member) => rows.member(&member)?,
            RevisionRow::Selection(_) => {}
            _ => {
                return Err(DbError::invalid_value(
                    &key,
                    "current selection metadata or membership",
                ));
            }
        }
    }
    Ok(revision_rows::load_revision_nodes(
        graph,
        root,
        commit_version,
        0,
        rows.nodes.into_iter().collect(),
        None,
        rows.records,
    )?
    .state)
}

pub(super) fn load_revision_membership_state(
    graph: &GrafeoDB,
    store: &super::graph_store::GraphStore,
    query: &SemanticRevisionSelectionQuery,
    commit_version: i64,
) -> Result<SemanticRevisionState> {
    if query.structure_coverage == SemanticRevisionStructureCoverage::Project
        || !revision_rows::revision_project_managed(graph, &query.project_root)?
    {
        return load_selected_revision_state(
            graph,
            &query.project_root,
            &query.selection_id,
            commit_version,
        );
    }
    let metadata = revision_rows::revision_row_nodes(
        graph,
        &query.project_root,
        "selection",
        &query.selection_id,
    )?;
    let Some(node) = metadata.first() else {
        return Ok(SemanticRevisionState {
            project_root: query.project_root.clone(),
            commit_version,
            ..Default::default()
        });
    };
    let RevisionRow::Selection(selection) = revision_rows::revision_row_from_node(graph, *node)?
    else {
        return Err(DbError::invalid_value(
            &query.selection_id,
            "stored selection metadata",
        ));
    };
    let mut rows = RevisionBasisRows::new(graph, &query.project_root, &selection.source_context);
    rows.nodes.extend(metadata);
    rows.row("project", "")?;
    if let SemanticRevisionStructureCoverage::Partition(partition) = &query.structure_coverage {
        rows.structure(store, partition, &query.selection_id)?;
    }
    for id in &query.artifact_ids {
        rows.family(&SemanticRecordRef::artifact(id), &query.selection_id)?;
    }
    rows.memberships()?;
    Ok(revision_rows::load_revision_nodes(
        graph,
        &query.project_root,
        commit_version,
        0,
        rows.nodes.into_iter().collect(),
        None,
        rows.records,
    )?
    .state)
}

pub(super) fn load_selection_history_state(
    graph: &GrafeoDB,
    root: &str,
    selection_id: &str,
    commit_version: i64,
) -> Result<SemanticRevisionState> {
    let key = serde_json::to_string(&(root, selection_id))?;
    let mut nodes =
        graph.find_nodes_by_property("revision_history_selection_key", &Value::from(key));
    nodes.extend(revision_rows::revision_row_nodes(
        graph, root, "project", "",
    )?);
    Ok(revision_rows::load_revision_nodes(
        graph,
        root,
        commit_version,
        0,
        nodes,
        None,
        BTreeMap::new(),
    )?
    .state)
}

impl<'a> RevisionBasisRows<'a> {
    fn new(graph: &'a GrafeoDB, root: &'a str, context: &'a str) -> Self {
        Self {
            graph,
            root,
            context,
            nodes: BTreeSet::new(),
            records: BTreeMap::new(),
            families: BTreeSet::new(),
            connections: BTreeSet::new(),
        }
    }

    fn row(&mut self, kind: &str, identity: &str) -> Result<()> {
        self.nodes.extend(revision_rows::revision_row_nodes(
            self.graph, self.root, kind, identity,
        )?);
        Ok(())
    }

    fn indexed(&mut self, property: &str, identity: &str) -> Vec<NodeId> {
        let nodes = self.index_candidates(property, identity);
        self.nodes.extend(nodes.iter().copied());
        nodes
    }

    fn index_candidates(&self, property: &str, identity: &str) -> Vec<NodeId> {
        self.graph
            .find_nodes_by_property(property, &Value::from(identity))
            .into_iter()
            .filter(|id| {
                self.graph
                    .get_node(*id)
                    .is_some_and(|node| node.has_label("SemanticRevisionRow"))
            })
            .collect()
    }

    fn record(&mut self, reference: &SemanticRecordRef) -> Result<()> {
        if self.records.contains_key(reference) {
            return Ok(());
        }
        let Some((root, record)) = revision_rows::stored_revision_by_id(self.graph, reference)?
        else {
            return Ok(());
        };
        if root != self.root {
            return Err(DbError::invalid_value(
                &reference.record_id,
                format!("an exact revision in project `{}`", self.root),
            ));
        }
        let dependencies = body_dependencies(&record.body);
        self.records.insert(reference.clone(), record);
        self.row("record", &serde_json::to_string(reference)?)?;
        if reference.kind == SemanticRecordKind::Artifact {
            self.artifact_associations(&reference.record_id)?;
        }
        for dependency in dependencies {
            self.record(&dependency)?;
        }
        Ok(())
    }

    fn family(&mut self, reference: &SemanticRecordRef, selection_id: &str) -> Result<()> {
        self.record(reference)?;
        let mut pending = vec![reference.clone()];
        while let Some(record) = pending.pop() {
            if !self.families.insert(record.clone()) {
                continue;
            }
            if self.selected(selection_id, SemanticRevisionMember::Record(record.clone()))? {
                self.record(&record)?;
                continue;
            }
            // Policy follows the same ancestry links; intermediate bodies are not publication input.
            pending.extend(self.successors(&record)?);
        }
        Ok(())
    }

    fn successors(&mut self, reference: &SemanticRecordRef) -> Result<Vec<SemanticRecordRef>> {
        let key = serde_json::to_string(&(self.root, self.context, reference))?;
        let mut successors = Vec::new();
        for node in self.indexed("revision_link_older", &key) {
            let RevisionRow::Link(link) = revision_rows::revision_row_from_node(self.graph, node)?
            else {
                return Err(DbError::invalid_value(key, "a scoped successor link"));
            };
            successors.push(link.newer);
        }
        Ok(successors)
    }

    fn memberships(&mut self) -> Result<()> {
        for reference in self.records.keys().cloned().collect::<Vec<_>>() {
            let identity = serde_json::to_string(&reference)?;
            for node in self.indexed("revision_membership_identity", &identity) {
                let row = revision_rows::revision_row_from_node(self.graph, node)?;
                let selection = row
                    .membership_selection_id()
                    .ok_or_else(|| DbError::invalid_value(&identity, "a current membership row"))?;
                self.row("selection", selection)?;
            }
        }
        for id in self.connections.iter().cloned().collect::<Vec<_>>() {
            for node in self.indexed("revision_membership_connection", &id) {
                let row = revision_rows::revision_row_from_node(self.graph, node)?;
                let selection = row.membership_selection_id().ok_or_else(|| {
                    DbError::invalid_value(&id, "a selected connection membership")
                })?;
                self.row("selection", selection)?;
            }
        }
        Ok(())
    }

    fn structure(
        &mut self,
        store: &super::graph_store::GraphStore,
        partition: &SemanticPartition,
        selection_id: &str,
    ) -> Result<()> {
        for node in self.partition_headers(store, partition)? {
            self.structure_header(node, selection_id)?;
        }
        Ok(())
    }

    fn partition_headers(
        &self,
        store: &super::graph_store::GraphStore,
        partition: &SemanticPartition,
    ) -> Result<BTreeSet<NodeId>> {
        let mut headers = BTreeSet::new();
        for path in &partition.replace_paths {
            headers.extend(store.path_prefix_revision_headers(self.graph, self.root, path));
        }
        if partition.replace_paths.is_empty() {
            if let Some(provider) = &partition.provider_instance_id {
                let key = serde_json::to_string(&(self.root, provider))?;
                headers.extend(self.index_candidates("revision_record_provider", &key));
            }
        }
        Ok(headers)
    }

    fn structure_header(&mut self, node: NodeId, selection_id: &str) -> Result<()> {
        let RevisionRow::Record(header) = revision_rows::revision_row_from_node(self.graph, node)?
        else {
            return Err(DbError::invalid_value(
                node.to_string(),
                "a revision header in the path/provider index",
            ));
        };
        // A selected connection may still need this older owner as exact evidence.
        self.owned_connections(&header.record, selection_id)?;
        // Path headers retain navigation history; current membership seeds ordinary body reads.
        if !self.selected(
            selection_id,
            SemanticRevisionMember::Record(header.record.clone()),
        )? {
            return Ok(());
        }
        self.record(&header.record)?;
        self.associations(&header.record.record_id)
    }

    fn published_connection_definitions(
        &mut self,
        relationships: &[SemanticRelationship],
    ) -> Result<()> {
        let owners = relationships
            .iter()
            .map(|relationship| &relationship.source_element_id)
            .collect::<BTreeSet<_>>();
        for owner in owners {
            let nodes = self.connection_candidates(&SemanticRecordRef::element(owner))?;
            // Deselected definitions still reserve exact IDs; their bodies need not be hydrated.
            self.nodes.extend(nodes);
        }
        Ok(())
    }

    fn owned_connections(
        &mut self,
        reference: &SemanticRecordRef,
        selection_id: &str,
    ) -> Result<()> {
        for node in self.connection_candidates(reference)? {
            let connection = self.connection_row(node)?;
            // Retained definitions reserve IDs even when their membership is not selected.
            self.nodes.insert(node);
            let member = SemanticRevisionMember::Connection(connection.connection_id.clone());
            if !self.selected(selection_id, member)? {
                continue;
            }
            self.include_connection(node, connection)?;
        }
        Ok(())
    }

    fn connection_candidates(&self, reference: &SemanticRecordRef) -> Result<Vec<NodeId>> {
        let key = serde_json::to_string(&(self.root, &reference.record_id))?;
        Ok(
            ["revision_connection_source", "revision_containment_target"]
                .into_iter()
                .flat_map(|property| self.index_candidates(property, &key))
                .collect(),
        )
    }

    fn connection_row(&self, node: NodeId) -> Result<SemanticRevisionConnection> {
        match revision_rows::revision_row_from_node(self.graph, node)? {
            RevisionRow::Connection(connection) => Ok(connection),
            _ => Err(DbError::invalid_value(
                node.to_string(),
                "a stored exact connection definition",
            )),
        }
    }

    fn include_connection(
        &mut self,
        node: NodeId,
        connection: SemanticRevisionConnection,
    ) -> Result<()> {
        self.nodes.insert(node);
        if !self.connections.insert(connection.connection_id) {
            return Ok(());
        }
        self.record(&SemanticRecordRef::element(
            &connection.relationship.source_element_id,
        ))?;
        self.record(&SemanticRecordRef::element(
            &connection.relationship.target_element_id,
        ))
    }

    fn selected(&self, selection_id: &str, member: SemanticRevisionMember) -> Result<bool> {
        let membership = RevisionRow::Membership(revision_rows::RevisionMembership {
            selection_id: selection_id.into(),
            member,
        });
        let identity = membership.identity()?;
        let Some(node) = self.membership_node(&identity)? else {
            return Ok(false);
        };
        let stored = revision_rows::revision_row_from_node(self.graph, node)?;
        if stored.kind() != "membership" || stored.identity()? != identity {
            return Err(DbError::invalid_value(
                identity,
                "the indexed selection membership",
            ));
        }
        Ok(true)
    }

    fn membership_node(&self, identity: &str) -> Result<Option<NodeId>> {
        let nodes =
            revision_rows::revision_row_nodes(self.graph, self.root, "membership", identity)?;
        match nodes.as_slice() {
            [] => Ok(None),
            [node] => Ok(Some(*node)),
            _ => Err(DbError::invalid_value(
                identity,
                "one exact selection membership",
            )),
        }
    }

    fn associations(&mut self, element: &str) -> Result<()> {
        let key = serde_json::to_string(&(self.root, element))?;
        for node in self.indexed("revision_association_element", &key) {
            let RevisionRow::Association(association) =
                revision_rows::revision_row_from_node(self.graph, node)?
            else {
                return Err(DbError::invalid_value(
                    element,
                    "a contextual artifact association",
                ));
            };
            self.record(&SemanticRecordRef::artifact(&association.artifact_id))?;
        }
        Ok(())
    }

    fn artifact_associations(&mut self, artifact: &str) -> Result<()> {
        let key = serde_json::to_string(&(self.root, artifact))?;
        for node in self.indexed("revision_association_artifact", &key) {
            let RevisionRow::Association(association) =
                revision_rows::revision_row_from_node(self.graph, node)?
            else {
                return Err(DbError::invalid_value(
                    artifact,
                    "a contextual artifact association",
                ));
            };
            self.record(&SemanticRecordRef::element(
                &association.semantic_element_id,
            ))?;
        }
        Ok(())
    }

    fn member(&mut self, member: &revision_rows::RevisionMembership) -> Result<()> {
        match member.record() {
            Some(record) => self.record(record),
            None => {
                let id = member.connection_id().ok_or_else(|| {
                    DbError::invalid_value("membership", "an exact record or connection ID")
                })?;
                let nodes =
                    revision_rows::revision_row_nodes(self.graph, self.root, "connection", id)?;
                for node in nodes {
                    let connection = self.connection_row(node)?;
                    self.include_connection(node, connection)?;
                }
                Ok(())
            }
        }
    }
}

fn body_dependencies(body: &SemanticRevisionBody) -> Vec<SemanticRecordRef> {
    let mut references = body.intrinsic_dependencies();
    if let SemanticRevisionBody::Artifact(artifact) = body {
        references.push(SemanticRecordRef::element(&artifact.semantic_element_id));
    }
    references
}
