//! A step retains one soft-target page and exact requested facts, never a project-sized state.
use super::*;
use std::collections::BTreeMap;

pub(super) struct ArchiveImportPlan {
    pub context: ImportContext,
    pub header: TransferHeader,
    pub checkpoint: Checkpoint,
    facts: BTreeMap<String, ImportFact>,
    answers: BTreeMap<String, Option<ImportFact>>,
    queries: Vec<Query>,
    changes: BTreeMap<(u8, StorageRowKey), Option<StorageRow>>,
    missing: bool,
}

pub(super) fn run(
    context: ImportContext,
    header: TransferHeader,
    checkpoint: Checkpoint,
    facts: Vec<ImportFact>,
) -> SemanticRevisionResult<StorageResult> {
    let original = checkpoint.clone();
    let mut plan = ArchiveImportPlan {
        context,
        header,
        checkpoint,
        facts: BTreeMap::new(),
        answers: BTreeMap::new(),
        queries: vec![],
        changes: BTreeMap::new(),
        missing: false,
    };
    for fact in facts {
        let id = fact.query_id.clone();
        if plan.facts.insert(id.clone(), fact).is_some() {
            return Err(invalid(
                id.to_string(),
                "one fact response per requested query",
            ));
        }
    }
    let acceptance = plan.execute()?;
    let state = if plan.missing {
        "need_facts"
    } else if acceptance.is_some() {
        "ready"
    } else {
        "advance"
    };
    let checkpoint = if plan.missing {
        &original
    } else {
        &plan.checkpoint
    };
    let result = Step {
        state,
        checkpoint_json: serde_json::to_string(checkpoint)?,
        queries: if plan.missing {
            plan.queries.clone()
        } else {
            vec![]
        },
        writes: if plan.missing { vec![] } else { plan.writes() },
        acceptance: if plan.missing { None } else { acceptance },
    };
    encoded("archive_import_step", &result)
}

impl ArchiveImportPlan {
    pub fn waiting(&self) -> bool {
        self.missing
    }
    pub fn commit_prefix(&mut self) {
        self.missing = false;
    }
    fn execute(&mut self) -> SemanticRevisionResult<Option<Acceptance>> {
        match self.checkpoint.phase {
            Phase::Replay => self.replay(),
            Phase::Finish => self.finish().map(Some),
            _ => {
                self.walk()?;
                Ok(None)
            }
        }
    }

    pub fn key(
        &mut self,
        catalog: Catalog,
        key: StorageRowKey,
    ) -> SemanticRevisionResult<Option<Option<StorageRow>>> {
        if let Some(value) = self.changes.get(&(catalog_id(catalog), key.clone())) {
            return Ok(Some(value.clone()));
        }
        let fact = self.query(catalog, Lookup::Key { key: key.clone() })?;
        let Some(fact) = fact else {
            return Ok(None);
        };
        if fact.next.is_some()
            || fact.rows.len() > 1
            || fact.rows.first().is_some_and(|row| row.key != key)
        {
            return Err(invalid(
                &key.identity,
                "one exact row or explicit absence for a key query",
            ));
        }
        Ok(Some(fact.rows.into_iter().next()))
    }

    pub fn index(
        &mut self,
        catalog: Catalog,
        name: &str,
        value: &str,
        after: Option<StorageRowKey>,
    ) -> SemanticRevisionResult<Option<ImportFact>> {
        self.query(
            catalog,
            Lookup::Index {
                name: name.into(),
                value: value.into(),
                after,
                target_bytes: PAGE_BYTES,
            },
        )
    }

    pub fn has_kind(
        &mut self,
        catalog: Catalog,
        kind: &str,
    ) -> SemanticRevisionResult<Option<bool>> {
        self.query(
            catalog,
            Lookup::Scan {
                row_kind: Some(kind.into()),
                after: None,
                target_bytes: 1,
            },
        )
        .map(|fact| fact.map(|fact| !fact.rows.is_empty()))
    }

    fn query(
        &mut self,
        catalog: Catalog,
        lookup: Lookup,
    ) -> SemanticRevisionResult<Option<ImportFact>> {
        let signature = serde_json::to_string(&(catalog, &lookup))?;
        if let Some(answer) = self.answers.get(&signature) {
            if answer.is_none() {
                self.missing = true;
            }
            return Ok(answer.clone());
        }
        if self.queries.len() >= 128 {
            self.missing = true;
            return Ok(None);
        }
        let query_id = signature.clone();
        self.queries.push(Query {
            query_id: query_id.clone(),
            catalog,
            lookup,
        });
        let fact = self.facts.remove(&query_id);
        if fact.is_none() {
            self.missing = true;
        }
        self.answers.insert(signature, fact.clone());
        Ok(fact)
    }

    pub fn put(&mut self, catalog: Catalog, row: StorageRow) {
        self.changes
            .insert((catalog_id(catalog), row.key.clone()), Some(row));
    }
    pub fn remove(&mut self, catalog: Catalog, key: StorageRowKey) {
        self.changes.insert((catalog_id(catalog), key), None);
    }

    fn writes(&self) -> Vec<Write> {
        [Catalog::Candidate, Catalog::Scratch]
            .into_iter()
            .filter_map(|catalog| {
                let entries = self
                    .changes
                    .iter()
                    .filter(|((id, _), _)| *id == catalog_id(catalog));
                let mut write = Write {
                    catalog,
                    rows: vec![],
                    removed: vec![],
                };
                for ((_, key), row) in entries {
                    match row {
                        Some(row) => write.rows.push(row.clone()),
                        None => write.removed.push(key.clone()),
                    }
                }
                (!write.rows.is_empty() || !write.removed.is_empty()).then_some(write)
            })
            .collect()
    }

    fn walk(&mut self) -> SemanticRevisionResult<()> {
        let catalog = phase_catalog(self.checkpoint.phase);
        let fact = match self.checkpoint.owner.clone() {
            Some(key) => self.key(catalog, key)?.map(|row| ImportFact {
                query_id: String::new(),
                rows: row.into_iter().collect(),
                next: None,
            }),
            None => self.query(
                catalog,
                Lookup::Scan {
                    row_kind: None,
                    after: self.checkpoint.after.clone(),
                    target_bytes: 64 * 1024,
                },
            )?,
        };
        let Some(fact) = fact else {
            return Ok(());
        };
        let resumed = self.checkpoint.owner.is_some();
        let mut completed = 0;
        let mut waiting = false;
        for row in &fact.rows {
            let checkpoint = self.checkpoint.clone();
            let changes = self.changes.clone();
            self.missing = false;
            let done = self.visit(row)?;
            if self.missing || waiting {
                self.checkpoint = checkpoint;
                self.changes = changes;
                if completed > 0 {
                    self.missing = false;
                    return Ok(());
                }
                waiting = true;
                if self.queries.len() >= 128 {
                    break;
                }
                continue;
            }
            if !done {
                self.checkpoint.owner = Some(row.key.clone());
                return Ok(());
            }
            self.checkpoint.after = Some(row.key.clone());
            self.checkpoint.owner = None;
            self.checkpoint.offset = 0;
            self.checkpoint.section = 0;
            self.checkpoint.generation = 0;
            self.checkpoint.cursor = None;
            completed += 1;
        }
        if waiting {
            self.missing = true;
            return Ok(());
        }
        if fact.next.is_none() && !resumed {
            self.advance_phase();
        }
        Ok(())
    }

    fn visit(&mut self, row: &StorageRow) -> SemanticRevisionResult<bool> {
        match self.checkpoint.phase {
            Phase::ValidateIncoming => validate::row(self, row, Catalog::Incoming),
            Phase::ValidateCandidate => validate::row(self, row, Catalog::Candidate),
            Phase::CopyCurrent => merge::copy(self, row),
            Phase::MergeIncoming => merge::incoming(self, row),
            Phase::ReconcileRecords => merge::reconcile(self, row),
            Phase::RetainIncoming => validate::retain(self, row, Catalog::Incoming),
            Phase::RetainCandidate => validate::retain(self, row, Catalog::Candidate),
            Phase::CountRemoved => merge::count_removed(self, row),
            _ => unreachable!(),
        }
    }

    fn advance_phase(&mut self) {
        if matches!(
            self.checkpoint.phase,
            Phase::RetainIncoming | Phase::RetainCandidate
        ) && self.checkpoint.dirty
        {
            self.checkpoint.dirty = false;
            self.checkpoint.after = None;
            return;
        }
        self.checkpoint.phase = match self.checkpoint.phase {
            Phase::RetainIncoming => Phase::ValidateIncoming,
            Phase::ValidateIncoming => Phase::CopyCurrent,
            Phase::CopyCurrent => Phase::MergeIncoming,
            Phase::MergeIncoming => Phase::ReconcileRecords,
            Phase::ReconcileRecords => Phase::RetainCandidate,
            Phase::RetainCandidate => Phase::ValidateCandidate,
            Phase::ValidateCandidate => Phase::CountRemoved,
            Phase::CountRemoved => Phase::Finish,
            phase => phase,
        };
        self.checkpoint.after = None;
        self.checkpoint.owner = None;
    }

    fn replay(&mut self) -> SemanticRevisionResult<Option<Acceptance>> {
        let receipt_identity = receipt_key(&self.context)?;
        let Some(receipt) = self.key(Catalog::Current, receipt_identity)? else {
            return Ok(None);
        };
        if let Some(row) = receipt {
            let receipt: imports::ImportReceipt = serde_json::from_str(&row.metadata_json)?;
            if receipt.request_digest != self.context.archive_digest {
                return Err(invalid(
                    &self.context.operation_id,
                    "identical bytes for an accepted import",
                ));
            }
            return Ok(Some(Acceptance {
                expected_commit: self.context.expected_commit.clone(),
                commit_version: self.context.expected_commit.clone(),
                outcome_json: receipt.outcome_json,
                receipt_row: row,
            }));
        }
        let Some(project) = self.key(Catalog::Incoming, key("project", ""))? else {
            return Ok(None);
        };
        let project =
            project.ok_or_else(|| invalid(&self.context.project_id, "archive project metadata"))?;
        if !matches!(canonical::metadata(&project)?, SemanticRevisionMetadata::Project(id) if id == self.header.project_id)
        {
            return Err(invalid(
                &self.header.project_id,
                "matching source project metadata",
            ));
        }
        let Some(working) = self.key(Catalog::Incoming, key("selection", "working"))? else {
            return Ok(None);
        };
        if working.is_none() {
            return Err(invalid("working", "an existing archive selection"));
        }
        self.checkpoint.phase = Phase::RetainIncoming;
        Ok(None)
    }

    fn finish(&self) -> SemanticRevisionResult<Acceptance> {
        let next = integer(&self.context.expected_commit)?
            .max(integer(&self.context.archive_commit)?)
            .checked_add(1)
            .ok_or_else(|| {
                invalid(
                    &self.context.archive_commit,
                    "an incrementable archive commit",
                )
            })?;
        let counts = &self.checkpoint.counts;
        let outcome = serde_json::json!({"projectId":self.header.project_id,"snapshotId":self.header.snapshot_id,"canonicalRemote":self.header.canonical_remote,
            "artifactsImported":counts.added_artifacts,"artifactsKept":counts.kept_artifacts,
            "structure":{"elements_upserted":counts.added_elements,"relationships_upserted":counts.connections,"identities_reused":counts.reused,"elements_marked_inactive":counts.inactive}}).to_string();
        let identity = receipt_key(&self.context)?.identity;
        let metadata = imports::ImportReceipt {
            identity: identity.clone(),
            request_digest: self.context.archive_digest.clone(),
            outcome_json: outcome.clone(),
        };
        Ok(Acceptance {
            expected_commit: self.context.expected_commit.clone(),
            commit_version: next.to_string(),
            outcome_json: outcome,
            receipt_row: StorageRow {
                key: key("archive_receipt", &identity),
                metadata_json: serde_json::to_string(&metadata)?,
                body_json: None,
                indexes: vec![],
            },
        })
    }
}

fn phase_catalog(phase: Phase) -> Catalog {
    match phase {
        Phase::RetainIncoming | Phase::ValidateIncoming | Phase::MergeIncoming => Catalog::Incoming,
        Phase::CopyCurrent | Phase::CountRemoved => Catalog::Current,
        _ => Catalog::Candidate,
    }
}

fn catalog_id(catalog: Catalog) -> u8 {
    match catalog {
        Catalog::Incoming => 0,
        Catalog::Current => 1,
        Catalog::Candidate => 2,
        Catalog::Scratch => 3,
    }
}
pub(super) fn key(kind: &str, identity: &str) -> StorageRowKey {
    StorageRowKey {
        kind: kind.into(),
        identity: identity.into(),
    }
}
pub(super) fn record_key(record: &SemanticRecordRef) -> SemanticRevisionResult<StorageRowKey> {
    Ok(key("record", &serde_json::to_string(record)?))
}
fn receipt_key(context: &ImportContext) -> SemanticRevisionResult<StorageRowKey> {
    Ok(key(
        "archive_receipt",
        &crate::revision_index::digest(&(&context.authenticated_actor, &context.operation_id))?,
    ))
}
