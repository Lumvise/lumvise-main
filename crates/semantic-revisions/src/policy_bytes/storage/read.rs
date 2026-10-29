//! Describes indexed storage facts. Repeated rounds follow dependencies; acceptance stays in the policy.
use super::{
    StorageResult,
    rows::{StorageRow, StorageRowKey},
};
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
mod expansion;
mod intent;
mod selected;
pub(super) use intent::ReadIntent;
pub(super) use selected::SelectedQuery;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::policy_bytes) struct ReadContext {
    pub project_root: String,
    pub project_id: String,
    pub commit_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::policy_bytes) enum StorageQuery {
    Key {
        key: StorageRowKey,
    },
    Index {
        name: String,
        value: String,
        selection_id: Option<String>,
    },
    Path {
        path: String,
        provider: Option<String>,
        selection_id: String,
    },
    Prefix {
        prefix: String,
        selection_id: String,
    },
    All,
}

pub(super) struct ReadPlan {
    pub(super) rows: BTreeMap<StorageRowKey, StorageRow>,
    metadata: Vec<SemanticRevisionMetadata>,
    completed: BTreeSet<StorageQuery>,
    requested: BTreeSet<StorageQuery>,
}

pub(super) fn plan(
    context: ReadContext,
    intent_json: &str,
    rows: Vec<StorageRow>,
    completed: Vec<StorageQuery>,
) -> SemanticRevisionResult<StorageResult> {
    let intent: ReadIntent = serde_json::from_str(intent_json)?;
    let mut plan = ReadPlan::new(rows, completed)?;
    plan.key("project", "");
    intent.request(&mut plan)?;
    if let ReadIntent::Archive { selection_id } = &intent {
        if selection_id == "public" {
            plan.expand_selected(selection_id)?;
        }
        for metadata in plan.metadata.clone() {
            if let SemanticRevisionMetadata::Record(header) = metadata {
                let key = super::vectors::key(&header.record)?;
                plan.key(&key.kind, &key.identity);
                if header.record.kind == SemanticRecordKind::Artifact {
                    plan.index("blob_owner", &header.record.record_id, None);
                }
            }
        }
    } else if let ReadIntent::Selected {
        selection_id,
        query,
    } = &intent
    {
        plan.expand_selected(selection_id)?;
        if matches!(query, selected::SelectedQuery::Path { .. }) {
            plan.location_ancestors(selection_id)?;
        }
    } else if matches!(intent, ReadIntent::VectorWrite { .. }) {
        plan.expand_selected("working")?;
    } else if !matches!(
        intent,
        ReadIntent::Content { .. } | ReadIntent::Vectors { .. }
    ) && !intent.replay_available(&plan)?
    {
        plan.expand(!matches!(
            intent,
            ReadIntent::Connections { .. }
                | ReadIntent::SelectionAt { .. }
                | ReadIntent::SourceAnchor { .. }
        ))?;
    }
    let queries = plan
        .requested
        .difference(&plan.completed)
        .cloned()
        .collect::<Vec<_>>();
    if !queries.is_empty() {
        return Ok(StorageResult::Read {
            queries,
            state_json: None,
        });
    }
    let state = super::rows::decode(
        &context.project_root,
        &context.project_id,
        super::integer(&context.commit_version)?,
        plan.rows.into_values().collect(),
    )?;
    Ok(StorageResult::Read {
        queries,
        state_json: Some(serde_json::to_string(&state)?),
    })
}

impl ReadPlan {
    fn new(rows: Vec<StorageRow>, completed: Vec<StorageQuery>) -> SemanticRevisionResult<Self> {
        let mut indexed = BTreeMap::new();
        let mut metadata = Vec::new();
        for row in rows {
            if super::contributions::is_receipt(&row) {
                super::contributions::validate_receipt(&row)?;
                indexed.insert(row.key.clone(), row);
                continue;
            }
            if super::content::attachments::is_attachment(&row) {
                super::content::attachments::decode(&row)?;
                indexed.insert(row.key.clone(), row);
                continue;
            }
            if super::content::staged::is_staged(&row) {
                super::content::staged::validate(&row)?;
                indexed.insert(row.key.clone(), row);
                continue;
            }
            if super::archives::is_metadata(&row) {
                super::archives::validate_metadata(&row)?;
                indexed.insert(row.key.clone(), row);
                continue;
            }
            if super::vectors::is_vector(&row) {
                super::vectors::owner(&row)?;
                if indexed.insert(row.key.clone(), row).is_some() {
                    return Err(SemanticRevisionError::invalid_value(
                        "duplicate vector",
                        "unique stored vector facts",
                    ));
                }
                continue;
            }
            let decoded: SemanticRevisionMetadata = serde_json::from_str(&row.metadata_json)?;
            if decoded.kind() != row.key.kind
                || decoded.identity()? != row.key.identity
                || indexed.insert(row.key.clone(), row).is_some()
            {
                return Err(SemanticRevisionError::invalid_value(
                    decoded.identity()?,
                    "unique storage facts matching their exact indexed identities",
                ));
            }
            metadata.push(decoded);
        }
        Ok(Self {
            rows: indexed,
            metadata,
            completed: completed.into_iter().collect(),
            requested: BTreeSet::new(),
        })
    }
    pub fn key(&mut self, kind: &str, identity: &str) {
        self.requested.insert(StorageQuery::Key {
            key: StorageRowKey {
                kind: kind.into(),
                identity: identity.into(),
            },
        });
    }
    pub fn index(&mut self, name: &str, value: &str, selection: Option<&str>) {
        self.requested.insert(StorageQuery::Index {
            name: name.into(),
            value: value.into(),
            selection_id: selection.map(str::to_string),
        });
    }
    pub fn records(
        &mut self,
        records: impl IntoIterator<Item = SemanticRecordRef>,
    ) -> SemanticRevisionResult<()> {
        for record in records {
            self.key("record", &serde_json::to_string(&record)?);
        }
        Ok(())
    }
    pub fn selection(&mut self, id: &str, members: bool) {
        self.key("selection", id);
        if members {
            self.index("selection_member", id, None);
        }
    }
    pub fn membership(
        &mut self,
        selection: &str,
        member: SemanticRevisionMember,
    ) -> SemanticRevisionResult<bool> {
        let row = SemanticRevisionMetadata::Membership(SemanticRevisionMembership {
            selection_id: selection.into(),
            member,
        });
        let identity = row.identity()?;
        self.key("membership", &identity);
        Ok(self.rows.contains_key(&StorageRowKey {
            kind: "membership".into(),
            identity,
        }))
    }
    pub fn coverage(&mut self, selection: &str, coverage: &SemanticRevisionStructureCoverage) {
        match coverage {
            SemanticRevisionStructureCoverage::Project => self.selection(selection, true),
            SemanticRevisionStructureCoverage::Partition(partition) => {
                for path in &partition.replace_paths {
                    self.requested.insert(StorageQuery::Path {
                        path: path.clone(),
                        provider: partition.provider_instance_id.clone(),
                        selection_id: selection.into(),
                    });
                }
                if partition.replace_paths.is_empty() {
                    if let Some(provider) = &partition.provider_instance_id {
                        self.index("record_provider", provider, Some(selection));
                    }
                }
            }
            SemanticRevisionStructureCoverage::None => {}
        }
    }
    pub fn families(
        &mut self,
        context: &str,
        selection: &str,
        roots: Vec<SemanticRecordRef>,
    ) -> SemanticRevisionResult<()> {
        let mut pending = roots;
        let mut seen = BTreeSet::new();
        while let Some(record) = pending.pop() {
            if !seen.insert(record.clone()) {
                continue;
            }
            self.records([record.clone()])?;
            if self.membership(selection, SemanticRevisionMember::Record(record.clone()))? {
                continue;
            }
            self.index(
                "link_older",
                &serde_json::to_string(&(context, &record))?,
                None,
            );
            pending.extend(self.metadata.iter().filter_map(|row| match row {
                SemanticRevisionMetadata::Link(link)
                    if link.source_context == context && link.older == record =>
                {
                    Some(link.newer.clone())
                }
                _ => None,
            }));
        }
        Ok(())
    }
}
