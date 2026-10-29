//! The common producer lifecycle: resolve a source, observe before parsing, freeze and acknowledge.
use super::*;
use lumvise_contracts::{IndexBatchRequest, SemanticSourceUpsert};
use lumvise_db_core::{
    RelationalPersistence, SemanticPartition, SemanticRevisionPublicationReceipt,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(crate) struct IndexSourceCheckout {
    journal: IndexPublicationJournal,
    source: SemanticSourceUpsert,
}

pub(crate) struct IndexSourceBase {
    generation: Option<i64>,
    inventory: Vec<IndexPathDigest>,
    pub(super) token: String,
    pub(crate) requires_full_scan: bool,
}

impl IndexSourceCheckout {
    /// Resolves a logical source before parsing; checkout paths stay client-local identities.
    /// Example: open two physical roots against one project route with separate client IDs.
    pub(crate) fn open(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        client: &str,
        root: &Path,
        project_root: &str,
        provider: &str,
        control: &InvocationControl,
    ) -> Result<Self> {
        let binding = super::source_binding::resolve(
            semantic.as_ref(),
            relational.clone(),
            client,
            root,
            project_root,
            provider,
            control,
        )?;
        let source = SemanticSourceUpsert {
            semantic_source_id: binding.semantic_source_id.clone(),
            kind: "filesystem".into(),
            name: "Project source".into(),
            root_path: None,
            root_uri: format!(
                "lumvise://projects/{}/sources/filesystem",
                binding.project_id
            ),
        };
        Ok(Self {
            journal: IndexPublicationJournal::new(
                semantic,
                relational,
                binding,
                project_root.into(),
            )?,
            source,
        })
    }

    /// The shared identity to inject into ProjectIndexer before producing IDs.
    /// Example: pass this descriptor to SemanticIndexProjection::with_source.
    pub(crate) fn source(&self) -> &SemanticSourceUpsert {
        &self.source
    }

    /// Captures the durable base before filesystem parsing; an unknown base needs verified attachment.
    /// Example: retain this value across prepare/project and pass it to freeze_batch.
    pub(crate) fn before_scan(&self, control: &InvocationControl) -> Result<IndexSourceBase> {
        self.journal.rows.ensure(control)?;
        let header = self
            .journal
            .rows
            .get::<CheckpointHeader>("header", control)?;
        let inventory = if header.is_some() {
            self.journal.inventory(control)?
        } else {
            vec![]
        };
        if self
            .journal
            .rows
            .get::<CheckpointHeader>("header", control)?
            != header
        {
            return Err(invalid_checkpoint(
                &self.journal.binding.checkout_id,
                "a stable producer base before scanning",
            ));
        }
        Ok(IndexSourceBase {
            token: self.scan_token(header.as_ref())?,
            requires_full_scan: header
                .as_ref()
                .is_none_or(|header| header.producer_generation == 0),
            generation: header.map(|header| header.generation),
            inventory,
        })
    }

    /// Returns original pending producer input before any new filesystem scan.
    /// Example: replay this batch through Semantic with the same operation ID after restart.
    pub(crate) fn pending(
        &self,
        control: &InvocationControl,
    ) -> Result<Option<FrozenIndexPublication>> {
        self.journal.rows.ensure(control)?;
        let Some(header) = self
            .journal
            .rows
            .get::<CheckpointHeader>("header", control)?
        else {
            return Ok(None);
        };
        if header
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.ready)
        {
            self.journal.discard_unpublished_draft(control)?;
            return Ok(None);
        }
        self.journal.pending(control)
    }

    /// Filters unchanged stale files and freezes exact changed input for the Semantic boundary.
    /// Example: a full scan after restart publishes only changes against this checkout's old base.
    pub(crate) fn freeze_batch(
        &self,
        mut batch: IndexBatchRequest,
        full: bool,
        before: IndexSourceBase,
        control: &InvocationControl,
    ) -> Result<Option<FrozenIndexPublication>> {
        let inventory = source_projection::projected_inventory(&batch)?;
        let current = self
            .journal
            .rows
            .get::<CheckpointHeader>("header", control)?;
        if self.scan_token(current.as_ref())? != before.token {
            return Err(invalid_checkpoint(
                &before.token,
                "the unchanged checkout checkpoint captured before parsing",
            ));
        }
        self.establish_base(&inventory, full, before.generation, control)?;
        let initial = self.journal.header(control)?.producer_generation == 0;
        if initial && !full {
            return Err(invalid_checkpoint(
                &self.journal.binding.checkout_id,
                "a complete first acceptance to protect the verified source base",
            ));
        }
        let mut changed = if initial {
            inventory.iter().map(|entry| entry.path.clone()).collect()
        } else {
            changed_paths(&inventory, &before.inventory)
        };
        changed.extend(source_projection::affected_callers(&batch, &changed));
        let removed = removed_paths(&batch, full, &inventory, &before.inventory);
        if changed.is_empty()
            && removed.is_empty()
            && self
                .journal
                .header(control)?
                .last_acknowledged_operation
                .is_some()
        {
            return Ok(None);
        }
        let replace_all = full && changed.len() == inventory.len();
        retain_changed(&mut batch, &changed, &removed);
        if replace_all {
            // A complete source replacement needs one provider scope, not thousands of path filters.
            batch.replace_paths.clear();
            batch.removed_paths.clear();
        }
        self.rewrite_references(&mut batch, control)?;
        let partition = source_partition(&batch);
        batch.revision_observation = Some(self.journal.observation(&partition, control)?);
        let frozen = FrozenIndexPublication {
            operation_id: format!("source-index-{}", uuid::Uuid::new_v4()),
            batch,
            inventory: inventory
                .into_iter()
                .filter(|entry| changed.contains(&entry.path))
                .collect(),
        };
        self.journal.freeze(&frozen, control)?;
        Ok(Some(frozen))
    }

    /// Advances only the scope that Semantic actually accepted.
    /// Example: call after the final ingestion page, including its source-descriptor write, succeeds.
    pub(crate) fn acknowledge(
        &self,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        self.journal.acknowledge(receipt, control)
    }

    fn scan_token(&self, header: Option<&CheckpointHeader>) -> Result<String> {
        let bytes = serde_json::to_vec(&(&self.journal.binding, header))
            .map_err(lumvise_db_core::DbError::from)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    fn establish_base(
        &self,
        inventory: &[IndexPathDigest],
        full: bool,
        generation: Option<i64>,
        control: &InvocationControl,
    ) -> Result<()> {
        if generation.is_none() && !full {
            return Err(invalid_checkpoint(
                &self.journal.binding.checkout_id,
                "a complete first scan for source verification",
            ));
        }
        self.journal.attach(inventory, control)?;
        if let Some(generation) = generation {
            if self.journal.header(control)?.generation != generation {
                return Err(invalid_checkpoint(
                    &self.journal.binding.checkout_id,
                    "the checkout base observed before parsing",
                ));
            }
        }
        Ok(())
    }

    fn rewrite_references(
        &self,
        batch: &mut IndexBatchRequest,
        control: &InvocationControl,
    ) -> Result<()> {
        let incoming = batch
            .semantic_elements
            .iter()
            .map(|element| element.semantic_element_id.clone())
            .collect::<BTreeSet<_>>();
        let mut remaps = BTreeMap::<String, String>::new();
        for parent in batch
            .semantic_elements
            .iter_mut()
            .filter_map(|element| element.parent_element_id.as_mut())
        {
            self.rewrite_reference(parent, &incoming, &mut remaps, control)?;
        }
        for edge in &mut batch.semantic_relationships {
            self.rewrite_reference(&mut edge.source_element_id, &incoming, &mut remaps, control)?;
            let Some(target) = edge.target_element_id.as_mut() else {
                continue;
            };
            let original = target.clone();
            self.rewrite_reference(target, &incoming, &mut remaps, control)?;
            for locator in [&mut edge.target_label, &mut edge.target_locator]
                .into_iter()
                .flatten()
            {
                if *locator == original {
                    *locator = target.clone();
                }
            }
        }
        Ok(())
    }

    fn rewrite_reference(
        &self,
        reference: &mut String,
        incoming: &BTreeSet<String>,
        remaps: &mut BTreeMap<String, String>,
        control: &InvocationControl,
    ) -> Result<()> {
        if incoming.contains(reference) {
            return Ok(());
        }
        let accepted = match remaps.get(reference) {
            Some(id) => Some(id.clone()),
            None => self.journal.accepted_element(reference, control)?,
        };
        if let Some(accepted) = accepted {
            remaps.insert(reference.clone(), accepted.clone());
            *reference = accepted;
        }
        Ok(())
    }
}

fn changed_paths(inventory: &[IndexPathDigest], previous: &[IndexPathDigest]) -> BTreeSet<String> {
    let previous = previous
        .iter()
        .map(|entry| (&entry.path, entry))
        .collect::<BTreeMap<_, _>>();
    inventory
        .iter()
        .filter(|entry| previous.get(&entry.path).is_none_or(|old| *old != *entry))
        .map(|entry| entry.path.clone())
        .collect()
}

fn removed_paths(
    batch: &IndexBatchRequest,
    full: bool,
    inventory: &[IndexPathDigest],
    previous: &[IndexPathDigest],
) -> BTreeSet<String> {
    let current = inventory
        .iter()
        .map(|entry| &entry.path)
        .collect::<BTreeSet<_>>();
    previous
        .iter()
        .filter(|entry| full && !current.contains(&entry.path))
        .map(|entry| entry.path.clone())
        .chain(batch.removed_paths.iter().cloned())
        .collect()
}

fn retain_changed(
    batch: &mut IndexBatchRequest,
    changed: &BTreeSet<String>,
    removed: &BTreeSet<String>,
) {
    batch
        .semantic_elements
        .retain(|element| changed.contains(&element.path));
    let ids = batch
        .semantic_elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<BTreeSet<_>>();
    batch
        .semantic_relationships
        .retain(|edge| ids.contains(edge.source_element_id.as_str()));
    batch.replace_paths = changed.iter().cloned().collect();
    batch.removed_paths = removed.iter().cloned().collect();
}

fn source_partition(batch: &IndexBatchRequest) -> SemanticPartition {
    SemanticPartition {
        project_root: batch.project_root.clone(),
        provider_instance_id: Some(batch.provider_instance_id.clone()),
        replace_paths: batch
            .replace_paths
            .iter()
            .chain(&batch.removed_paths)
            .cloned()
            .collect(),
    }
}
