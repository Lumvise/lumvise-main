//! Portable revision metadata is shared by native rows, archives and the gateway; bodies are stored once.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SemanticRevisionMetadata {
    Project(String),
    Record(SemanticRevisionRecordHeader),
    Connection(SemanticRevisionConnection),
    Selection(SemanticRevisionSelection),
    Membership(SemanticRevisionMembership),
    History(SemanticRevisionSelectionDelta),
    Link(SemanticRevisionLink),
    Association(SemanticRevisionAssociation),
    Operation(SemanticRevisionAcceptedOperation),
    Retention(SemanticRevisionAcceptedRetention),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SemanticRevisionMembership {
    pub selection_id: String,
    pub member: SemanticRevisionMember,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SemanticRevisionMember {
    Record(SemanticRecordRef),
    Connection(String),
}

impl SemanticRevisionMembership {
    /// Returns the exact record member. Example: `membership.record()`.
    pub fn record(&self) -> Option<&SemanticRecordRef> {
        match &self.member {
            SemanticRevisionMember::Record(record) => Some(record),
            _ => None,
        }
    }
    /// Returns the exact connection member. Example: `membership.connection_id()`.
    pub fn connection_id(&self) -> Option<&str> {
        match &self.member {
            SemanticRevisionMember::Connection(id) => Some(id),
            _ => None,
        }
    }
}

impl SemanticRevisionMetadata {
    /// Returns a membership's selection. Example: `row.membership_selection_id()`.
    pub fn membership_selection_id(&self) -> Option<&str> {
        match self {
            Self::Membership(member) => Some(&member.selection_id),
            _ => None,
        }
    }
    /// Names the portable row variant. Example: `row.kind()` returns `record` for a header.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Project(_) => "project",
            Self::Record(_) => "record",
            Self::Connection(_) => "connection",
            Self::Selection(_) => "selection",
            Self::Membership(_) => "membership",
            Self::History(_) => "history",
            Self::Link(_) => "link",
            Self::Association(_) => "association",
            Self::Operation(_) => "operation",
            Self::Retention(_) => "retention",
        }
    }
    /// Supplies a stable metadata identity. Example: `row.identity()?`.
    pub fn identity(&self) -> SemanticRevisionResult<String> {
        Ok(match self {
            Self::Project(_) => String::new(),
            Self::Record(header) => serde_json::to_string(&header.record)?,
            Self::Connection(connection) => connection.connection_id.clone(),
            Self::Selection(selection) => selection.selection_id.clone(),
            Self::Membership(member) => serde_json::to_string(member)?,
            Self::History(delta) => {
                serde_json::to_string(&(&delta.selection.selection_id, delta.selection.generation))?
            }
            Self::Link(link) => {
                serde_json::to_string(&(&link.source_context, &link.older, &link.newer))?
            }
            Self::Association(association) => association.association_id.clone(),
            Self::Operation(operation) => operation.receipt.operation_id.clone(),
            Self::Retention(operation) => operation.receipt.operation_id.clone(),
        })
    }
}

impl SemanticRevisionState {
    /// Validates complete archived state without copying canonical bodies or treating omitted facts as absent.
    /// Example: `state.validate_archive_state()?` before encoding a portable archive.
    pub fn validate_archive_state(&self) -> SemanticRevisionResult<()> {
        crate::revision_index::require_project_identity(&self.project_id)?;
        require_archive_unique(
            self.records.iter().map(|row| &row.record),
            "unique exact archive records",
        )?;
        require_archive_unique(
            self.connections.iter().map(|row| &row.connection_id),
            "unique archive connection identities",
        )?;
        require_archive_unique(
            self.selections
                .iter()
                .map(|row| &row.selection.selection_id),
            "unique archive selections",
        )?;
        require_archive_unique(
            self.selection_history
                .iter()
                .map(|row| (&row.selection.selection_id, row.selection.generation)),
            "one delta per selection generation",
        )?;
        require_archive_unique(
            self.associations.iter().map(|row| &row.association_id),
            "unique archive associations",
        )?;
        require_archive_unique(
            self.accepted_operations
                .iter()
                .map(|row| &row.receipt.operation_id)
                .chain(
                    self.retention_operations
                        .iter()
                        .map(|row| &row.receipt.operation_id),
                ),
            "unique accepted operation identities",
        )?;
        for record in &self.records {
            validate_archive_record(&self.project_root, record)?;
        }
        for connection in &self.connections {
            connection.relationship.validate()?;
            crate::revision_index::ensure_root(
                &connection.relationship.project_root,
                &self.project_root,
            )?;
        }
        validate_restored_heads(self)?;
        validate_restored_requirements(self)
    }
    /// Removes routing aliases from portable archive metadata while preserving acknowledged request roots.
    /// Example: `state.archive_metadata_rows()` before encoding project-relative PZ records.
    pub fn archive_metadata_rows(&self) -> Vec<SemanticRevisionMetadata> {
        self.metadata_rows()
            .into_iter()
            .map(|mut row| {
                set_owned_root(&mut row, "");
                row
            })
            .collect()
    }
    /// Orders metadata for deterministic transport without changing selection or history meaning.
    /// Example: `state.normalize_order()` before persisting an owned archive.
    pub fn normalize_order(&mut self) {
        self.records.sort_by(|a, b| a.record.cmp(&b.record));
        self.connections
            .sort_by(|a, b| a.connection_id.cmp(&b.connection_id));
        self.selections
            .sort_by(|a, b| a.selection.selection_id.cmp(&b.selection.selection_id));
        for head in &mut self.selections {
            head.records.sort();
            head.connection_ids.sort();
        }
        self.selection_history.sort_by(|a, b| {
            (&a.selection.selection_id, a.selection.generation)
                .cmp(&(&b.selection.selection_id, b.selection.generation))
        });
        self.revision_links.sort_by(|a, b| {
            (&a.source_context, &a.older, &a.newer).cmp(&(&b.source_context, &b.older, &b.newer))
        });
        self.associations
            .sort_by(|a, b| a.association_id.cmp(&b.association_id));
        self.accepted_operations.sort_by(|a, b| {
            (a.receipt.commit_version, &a.receipt.operation_id)
                .cmp(&(b.receipt.commit_version, &b.receipt.operation_id))
        });
        self.retention_operations.sort_by(|a, b| {
            (a.receipt.commit_version, &a.receipt.operation_id)
                .cmp(&(b.receipt.commit_version, &b.receipt.operation_id))
        });
    }
    /// Preserves attachment ownership through retained artifact ancestry without retaining old bodies.
    /// Example: `state.retained_artifact_owner_ids()` when capturing raw archive blobs.
    pub fn retained_artifact_owner_ids(&self) -> BTreeSet<String> {
        let mut parents = BTreeMap::<&str, Vec<&str>>::new();
        for link in &self.revision_links {
            if link.older.kind == SemanticRecordKind::Artifact
                && link.newer.kind == SemanticRecordKind::Artifact
            {
                parents
                    .entry(&link.newer.record_id)
                    .or_default()
                    .push(&link.older.record_id);
            }
        }
        let mut pending = self
            .records
            .iter()
            .filter(|record| {
                record.record.kind == SemanticRecordKind::Artifact
                    && !matches!(record.body, SemanticRevisionBody::Pruned)
            })
            .map(|record| record.record.record_id.as_str())
            .collect::<Vec<_>>();
        let mut owners = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if owners.insert(id.to_string()) {
                pending.extend(parents.get(id).into_iter().flatten().copied());
            }
        }
        owners
    }
    /// Encodes metadata separately from bodies. Example: `state.metadata_rows()` for an archive.
    pub fn metadata_rows(&self) -> Vec<SemanticRevisionMetadata> {
        let mut rows = vec![SemanticRevisionMetadata::Project(self.project_id.clone())];
        rows.extend(
            self.records
                .iter()
                .map(|stored| SemanticRevisionMetadata::Record(stored.into())),
        );
        rows.extend(
            self.connections
                .iter()
                .cloned()
                .map(SemanticRevisionMetadata::Connection),
        );
        for head in &self.selections {
            rows.push(SemanticRevisionMetadata::Selection(head.selection.clone()));
            rows.extend(
                head.records
                    .iter()
                    .cloned()
                    .map(SemanticRevisionMember::Record)
                    .chain(
                        head.connection_ids
                            .iter()
                            .cloned()
                            .map(SemanticRevisionMember::Connection),
                    )
                    .map(|member| {
                        SemanticRevisionMetadata::Membership(SemanticRevisionMembership {
                            selection_id: head.selection.selection_id.clone(),
                            member,
                        })
                    }),
            );
        }
        rows.extend(
            self.selection_history
                .iter()
                .cloned()
                .map(SemanticRevisionMetadata::History),
        );
        rows.extend(
            self.revision_links
                .iter()
                .cloned()
                .map(SemanticRevisionMetadata::Link),
        );
        rows.extend(
            self.associations
                .iter()
                .cloned()
                .map(SemanticRevisionMetadata::Association),
        );
        rows.extend(
            self.accepted_operations
                .iter()
                .cloned()
                .map(SemanticRevisionMetadata::Operation),
        );
        rows.extend(
            self.retention_operations
                .iter()
                .cloned()
                .map(SemanticRevisionMetadata::Retention),
        );
        rows
    }
    /// Restores metadata and exact bodies while rebinding the caller's project route.
    /// Example: `SemanticRevisionState::from_metadata_rows("/restored", 17, rows, elements, artifacts)?`.
    pub fn from_metadata_rows(
        project_root: &str,
        commit_version: i64,
        rows: Vec<SemanticRevisionMetadata>,
        elements: Vec<SemanticElement>,
        artifacts: Vec<SemanticArtifact>,
    ) -> SemanticRevisionResult<Self> {
        let state =
            Self::from_storage_facts(project_root, commit_version, rows, elements, artifacts)?;
        state.validate_archive_state()?;
        Ok(state)
    }

    // Storage supplies scoped facts; only archive callers may assume complete historical membership.
    pub(crate) fn from_storage_facts(
        project_root: &str,
        commit_version: i64,
        rows: Vec<SemanticRevisionMetadata>,
        elements: Vec<SemanticElement>,
        artifacts: Vec<SemanticArtifact>,
    ) -> SemanticRevisionResult<Self> {
        crate::revision_index::ensure_root(project_root, project_root)?;
        let mut bodies = body_index(project_root, elements, artifacts)?;
        let mut state = Self {
            project_root: project_root.into(),
            commit_version,
            ..Self::default()
        };
        let mut memberships = BTreeMap::<String, Vec<SemanticRevisionMember>>::new();
        let mut identities = BTreeSet::new();
        for mut row in rows {
            let identity = (row.kind(), row.identity()?);
            if !identities.insert(identity.clone()) {
                return Err(SemanticRevisionError::invalid_value(
                    format!("{identity:?}"),
                    "one metadata row per identity",
                ));
            }
            set_owned_root(&mut row, project_root);
            append_metadata(&mut state, &mut memberships, &mut bodies, row)?;
        }
        if !bodies.is_empty() {
            return Err(SemanticRevisionError::invalid_value(
                format!("{:?}", bodies.keys()),
                "a revision header for every archived body",
            ));
        }
        if !identities.contains(&("project", String::new())) {
            return Err(SemanticRevisionError::invalid_value(
                project_root,
                "project revision metadata",
            ));
        }
        apply_memberships(&mut state, memberships)?;
        state.normalize_order();
        Ok(state)
    }
}

fn require_archive_unique<K: Ord + std::fmt::Debug>(
    keys: impl Iterator<Item = K>,
    expected: &str,
) -> SemanticRevisionResult<()> {
    let mut seen = BTreeSet::new();
    for key in keys {
        if seen.contains(&key) {
            return Err(SemanticRevisionError::invalid_value(
                format!("{key:?}"),
                expected,
            ));
        }
        seen.insert(key);
    }
    Ok(())
}

fn validate_archive_record(
    root: &str,
    stored: &SemanticStoredRevision,
) -> SemanticRevisionResult<()> {
    let exact = match &stored.body {
        SemanticRevisionBody::Element(element) => {
            element.validate()?;
            crate::revision_index::ensure_root(&element.project_root, root)?;
            if stored.element_scope.as_ref().is_none_or(|scope| {
                scope.path != element.path
                    || scope.provider_instance_id.as_deref()
                        != element.metadata["provider_instance_id"].as_str()
            }) {
                return Err(SemanticRevisionError::invalid_value(
                    &stored.record.record_id,
                    "the canonical element's path and provider ownership",
                ));
            }
            Some(SemanticRecordRef::element(&element.semantic_element_id))
        }
        SemanticRevisionBody::Artifact(artifact) => {
            artifact.validate()?;
            Some(SemanticRecordRef::artifact(&artifact.artifact_id))
        }
        SemanticRevisionBody::Pruned => None,
    };
    if exact
        .as_ref()
        .is_some_and(|reference| reference != &stored.record)
        || exact.is_none() != stored.pruned_at_ms.is_some()
        || stored.record.record_id.trim().is_empty()
    {
        return Err(SemanticRevisionError::invalid_value(
            &stored.record.record_id,
            "the exact immutable body or its explicit collected header",
        ));
    }
    Ok(())
}

fn validate_restored_requirements(state: &SemanticRevisionState) -> SemanticRevisionResult<()> {
    let index = crate::required_revisions::RevisionDependencyIndex::new(state);
    for stored in state
        .records
        .iter()
        .filter(|row| !matches!(row.body, SemanticRevisionBody::Pruned))
    {
        for reference in index.requirements(stored, &BTreeSet::new()) {
            index.require(&reference)?;
        }
    }
    let versions = state
        .selection_history
        .iter()
        .map(|row| row.commit_version)
        .chain(state.revision_links.iter().map(|row| row.commit_version))
        .chain(
            state
                .accepted_operations
                .iter()
                .map(|row| row.receipt.commit_version),
        )
        .chain(
            state
                .retention_operations
                .iter()
                .map(|row| row.receipt.commit_version),
        );
    if state.commit_version < 0
        || versions
            .into_iter()
            .any(|version| version < 0 || version > state.commit_version)
    {
        return Err(SemanticRevisionError::invalid_value(
            state.commit_version.to_string(),
            "a nonnegative archive commit no earlier than its accepted history",
        ));
    }
    Ok(())
}

fn body_index(
    root: &str,
    elements: Vec<SemanticElement>,
    artifacts: Vec<SemanticArtifact>,
) -> SemanticRevisionResult<BTreeMap<SemanticRecordRef, SemanticRevisionBody>> {
    let bodies = elements
        .into_iter()
        .map(|mut element| {
            element.project_root = root.into();
            (
                SemanticRecordRef::element(&element.semantic_element_id),
                SemanticRevisionBody::Element(element),
            )
        })
        .chain(artifacts.into_iter().map(|artifact| {
            (
                SemanticRecordRef::artifact(&artifact.artifact_id),
                SemanticRevisionBody::Artifact(artifact),
            )
        }));
    let mut indexed = BTreeMap::new();
    for (record, body) in bodies {
        if indexed.insert(record.clone(), body).is_some() {
            return Err(SemanticRevisionError::invalid_value(
                record.record_id,
                "one body per exact revision",
            ));
        }
    }
    Ok(indexed)
}

fn validate_restored_heads(state: &SemanticRevisionState) -> SemanticRevisionResult<()> {
    for head in &state.selections {
        crate::selection::validate_selected_records(state, &head.records, &head.connection_ids)?;
        crate::selection::require_context_selection(
            state,
            &head.selection.selection_id,
            &head.selection.source_context,
            head.selection.mode,
        )?;
        let historical = state
            .selection_at_generation(&head.selection.selection_id, head.selection.generation)?;
        if historical.as_ref() != Some(head) {
            return Err(SemanticRevisionError::invalid_value(
                &head.selection.selection_id,
                "current membership matching its complete generation history",
            ));
        }
    }
    Ok(())
}

fn append_metadata(
    state: &mut SemanticRevisionState,
    memberships: &mut BTreeMap<String, Vec<SemanticRevisionMember>>,
    bodies: &mut BTreeMap<SemanticRecordRef, SemanticRevisionBody>,
    row: SemanticRevisionMetadata,
) -> SemanticRevisionResult<()> {
    match row {
        SemanticRevisionMetadata::Project(project_id) => state.project_id = project_id,
        SemanticRevisionMetadata::Record(header) => {
            let body = bodies
                .remove(&header.record)
                .unwrap_or(SemanticRevisionBody::Pruned);
            if let SemanticRevisionBody::Element(element) = &body {
                if header.canonical_lifecycle.as_deref() != Some(&element.lifecycle)
                    || header.element_scope.as_ref().is_none_or(|scope| {
                        scope.path != element.path
                            || scope.provider_instance_id.as_deref()
                                != element.metadata["provider_instance_id"].as_str()
                    })
                {
                    return Err(SemanticRevisionError::invalid_value(
                        &header.record.record_id,
                        "canonical element fields matching their revision header",
                    ));
                }
            }
            state.records.push(header.restore(body)?);
        }
        SemanticRevisionMetadata::Connection(value) => state.connections.push(value),
        SemanticRevisionMetadata::Selection(selection) => {
            state.selections.push(SemanticRevisionSelectionState {
                selection,
                records: vec![],
                connection_ids: vec![],
            });
        }
        SemanticRevisionMetadata::Membership(member) => memberships
            .entry(member.selection_id)
            .or_default()
            .push(member.member),
        SemanticRevisionMetadata::History(value) => state.selection_history.push(value),
        SemanticRevisionMetadata::Link(value) => state.revision_links.push(value),
        SemanticRevisionMetadata::Association(value) => state.associations.push(value),
        SemanticRevisionMetadata::Operation(value) => state.accepted_operations.push(value),
        SemanticRevisionMetadata::Retention(value) => state.retention_operations.push(value),
    }
    Ok(())
}

fn apply_memberships(
    state: &mut SemanticRevisionState,
    memberships: BTreeMap<String, Vec<SemanticRevisionMember>>,
) -> SemanticRevisionResult<()> {
    for (selection_id, members) in memberships {
        let head = state
            .selections
            .iter_mut()
            .find(|head| head.selection.selection_id == selection_id)
            .ok_or_else(|| {
                SemanticRevisionError::invalid_value(
                    &selection_id,
                    "selection metadata for every membership",
                )
            })?;
        for member in members {
            match member {
                SemanticRevisionMember::Record(record) => head.records.push(record),
                SemanticRevisionMember::Connection(id) => head.connection_ids.push(id),
            }
        }
        head.records.sort();
        head.connection_ids.sort();
    }
    Ok(())
}

fn rebind_receipt(receipt: &mut SemanticRevisionPublicationReceipt, root: &str) {
    receipt.selection.project_root = root.into();
    if let Some(base) = &mut receipt.producer_base {
        base.project_root = root.into();
    }
    for link in &mut receipt.revision_links {
        link.project_root = root.into();
    }
}

fn set_owned_root(row: &mut SemanticRevisionMetadata, root: &str) {
    match row {
        SemanticRevisionMetadata::Connection(value) => {
            value.relationship.project_root = root.into()
        }
        SemanticRevisionMetadata::Selection(value) => value.project_root = root.into(),
        SemanticRevisionMetadata::History(value) => value.selection.project_root = root.into(),
        SemanticRevisionMetadata::Link(value) => value.project_root = root.into(),
        SemanticRevisionMetadata::Operation(value) => rebind_receipt(&mut value.receipt, root),
        _ => {}
    }
}
