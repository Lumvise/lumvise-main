//! Accept only the acknowledged scope; unrelated paths never adopt another instance's head.
use super::*;
use lumvise_db_core::{
    SemanticRecordKind, SemanticRecordRef, SemanticRevisionConnectionQuery,
    SemanticRevisionPublicationReceipt,
};
use std::collections::{BTreeMap, BTreeSet};

struct AcknowledgedScope<'a> {
    journal: &'a IndexPublicationJournal,
    paths: BTreeMap<String, CheckpointPath>,
    owner_paths: BTreeMap<String, String>,
    previous: Vec<CheckpointPath>,
    replaced: BTreeSet<String>,
}

impl IndexPublicationJournal {
    /// Advances acknowledged paths and connection ownership atomically with clearing pending.
    /// Example: accepting a.rs leaves this checkout's older b.rs base unchanged.
    pub(crate) fn acknowledge(
        &self,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        let original = self.header(control)?;
        let Some(identity) = &original.pending else {
            if original.last_acknowledged_operation.as_deref() == Some(&receipt.operation_id) {
                return Ok(());
            }
            return Err(invalid_checkpoint(
                &receipt.operation_id,
                "this checkout's pending or last acknowledged operation",
            ));
        };
        let Some(publication) = self.pending(control)? else {
            return self.already_acknowledged(receipt, control);
        };
        validate_receipt(&self.binding, &publication, receipt, original.generation)?;
        let producer_generation =
            accepted_producer_generation(&publication, receipt, original.producer_generation)?;
        let mut scope = AcknowledgedScope::new(self, &publication, control)?;
        scope.replace_elements(&publication, receipt)?;
        scope.remove_connections(&publication.observation()?.expected.connection_ids, control)?;
        scope.add_connections(receipt, control)?;
        let mut completed = original.clone();
        completed.generation = receipt.selection.generation;
        completed.producer_generation = producer_generation;
        completed.last_acknowledged_operation = Some(receipt.operation_id.clone());
        completed.pending = None;
        let mut changes = scope.mutations()?;
        changes.extend(self.pending_deletions(identity, &self.descriptor(identity, control)?));
        changes.extend([
            self.rows.require(Some(&original))?,
            self.rows.put("header", &completed)?,
        ]);
        self.rows.apply(changes, control)
    }

    fn already_acknowledged(
        &self,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        if self.header(control)?.last_acknowledged_operation.as_deref()
            == Some(&receipt.operation_id)
        {
            return Ok(());
        }
        Err(invalid_checkpoint(
            &receipt.operation_id,
            "this checkout's acknowledged operation after concurrent completion",
        ))
    }
}

impl<'a> AcknowledgedScope<'a> {
    fn new(
        journal: &'a IndexPublicationJournal,
        publication: &FrozenIndexPublication,
        control: &InvocationControl,
    ) -> Result<Self> {
        let partition = publication.partition();
        let previous = journal.paths_in_partition(&partition, control)?;
        let owner_paths = previous
            .iter()
            .flat_map(|path| {
                path.elements
                    .values()
                    .map(move |id| (id.clone(), path.digest.path.clone()))
            })
            .collect();
        Ok(Self {
            journal,
            paths: previous
                .iter()
                .map(|path| (path.digest.path.clone(), path.clone()))
                .collect(),
            owner_paths,
            replaced: previous
                .iter()
                .map(|path| path.digest.path.clone())
                .collect(),
            previous,
        })
    }

    fn replace_elements(
        &mut self,
        publication: &FrozenIndexPublication,
        receipt: &SemanticRevisionPublicationReceipt,
    ) -> Result<()> {
        let remaps = receipt
            .remaps
            .iter()
            .map(|remap| (&remap.incoming, &remap.accepted))
            .collect::<BTreeMap<_, _>>();
        let mut accepted = BTreeSet::new();
        for digest in &publication.inventory {
            self.replaced.insert(digest.path.clone());
            let row = self
                .paths
                .entry(digest.path.clone())
                .or_insert_with(|| CheckpointPath {
                    digest: digest.clone(),
                    elements: BTreeMap::new(),
                    connections: BTreeMap::new(),
                });
            row.digest = digest.clone();
            row.elements.clear();
        }
        for element in &publication.batch.semantic_elements {
            let input = SemanticRecordRef::element(&element.semantic_element_id);
            let output = remaps
                .get(&input)
                .filter(|record| record.kind == SemanticRecordKind::Element)
                .ok_or_else(|| {
                    invalid_checkpoint(&element.semantic_element_id, "an accepted element remap")
                })?;
            self.paths.get_mut(&element.path).unwrap().elements.insert(
                element.semantic_element_id.clone(),
                output.record_id.clone(),
            );
            self.owner_paths
                .insert(output.record_id.clone(), element.path.clone());
            accepted.insert((*output).clone());
        }
        if accepted != receipt.accepted_scope.records.iter().cloned().collect() {
            return Err(invalid_checkpoint(
                &receipt.operation_id,
                "accepted membership exactly matching this source scope",
            ));
        }
        let retained = publication
            .inventory
            .iter()
            .map(|digest| digest.path.as_str())
            .collect::<BTreeSet<_>>();
        for path in &self.replaced {
            if !retained.contains(path.as_str()) {
                self.paths.get_mut(path).unwrap().elements.clear();
            }
        }
        Ok(())
    }

    fn remove_connections(
        &mut self,
        removed: &[String],
        control: &InvocationControl,
    ) -> Result<()> {
        for id in removed {
            let edge = self
                .previous
                .iter()
                .find_map(|path| path.connections.get(id))
                .ok_or_else(|| {
                    invalid_checkpoint(id, "an observed connection in the acknowledged scope")
                })?
                .clone();
            for owner in edge.owners {
                if let Some(path) = self.owner_path(&owner, control)? {
                    self.load_path(&path, control)?;
                    self.paths.get_mut(&path).unwrap().connections.remove(id);
                }
            }
        }
        Ok(())
    }

    fn add_connections(
        &mut self,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        let result = self.journal.semantic(
            SemanticOperation::RevisionConnections(SemanticRevisionConnectionQuery {
                project_root: self.journal.project_root.clone(),
                project_id: self.journal.binding.project_id.clone(),
                connection_ids: receipt.accepted_scope.connection_ids.clone(),
            }),
            control,
        )?;
        let SemanticResult::RevisionConnections(edges) = result else {
            return Err(invalid_checkpoint(
                &receipt.operation_id,
                "exact accepted connection definitions",
            ));
        };
        for edge in edges {
            let observed = ObservedConnection {
                owners: edge
                    .relationship
                    .owned_element_ids()
                    .map(str::to_owned)
                    .collect(),
            };
            let mut placed = false;
            for owner in &observed.owners {
                if let Some(path) = self.owner_path(owner, control)? {
                    self.load_path(&path, control)?;
                    self.paths
                        .get_mut(&path)
                        .unwrap()
                        .connections
                        .insert(edge.connection_id.clone(), observed.clone());
                    placed = true;
                }
            }
            if !placed {
                return Err(invalid_checkpoint(
                    &edge.connection_id,
                    "an accepted source connection owner",
                ));
            }
        }
        Ok(())
    }

    fn owner_path(&mut self, id: &str, control: &InvocationControl) -> Result<Option<String>> {
        if let Some(path) = self.owner_paths.get(id) {
            return Ok(Some(path.clone()));
        }
        let result = self.journal.semantic(
            SemanticOperation::ResolveRevision {
                project_root: self.journal.project_root.clone(),
                record: SemanticRecordRef::element(id),
            },
            control,
        )?;
        let SemanticResult::RevisionResolution(Some(record)) = result else {
            return Ok(None);
        };
        let Some(scope) = record.element_scope.filter(|scope| {
            scope.provider_instance_id.as_deref()
                == Some(self.journal.binding.provider_instance_id.as_str())
        }) else {
            return Ok(None);
        };
        if self
            .journal
            .rows
            .get::<CheckpointPath>(&storage::path_key(&scope.path), control)?
            .is_none()
        {
            return Ok(None);
        }
        self.owner_paths.insert(id.into(), scope.path.clone());
        Ok(Some(scope.path))
    }

    fn load_path(&mut self, path: &str, control: &InvocationControl) -> Result<()> {
        if self.paths.contains_key(path) {
            return Ok(());
        }
        let row = self
            .journal
            .rows
            .get::<CheckpointPath>(&storage::path_key(path), control)?
            .ok_or_else(|| invalid_checkpoint(path, "an observed source owner path"))?;
        self.paths.insert(path.into(), row);
        Ok(())
    }

    fn mutations(&self) -> Result<Vec<lumvise_db_core::PluginDataMutation>> {
        let mut changes = Vec::new();
        for path in &self.previous {
            for original in path.elements.keys() {
                changes.push(self.journal.rows.delete(&storage::parser_key(original)));
            }
        }
        for (path, row) in &self.paths {
            if row.elements.is_empty() && self.replaced.contains(path) {
                changes.push(self.journal.rows.delete(&storage::path_key(path)));
            } else {
                changes.extend(self.journal.path_puts(row)?);
            }
        }
        Ok(changes)
    }
}

fn validate_receipt(
    binding: &IndexCheckoutBinding,
    publication: &FrozenIndexPublication,
    receipt: &SemanticRevisionPublicationReceipt,
    generation: i64,
) -> Result<()> {
    if receipt.operation_id != publication.operation_id
        || receipt.selection.project_root != publication.batch.project_root
        || receipt.selection.selection_id != binding.selection_id
        || receipt.selection.source_context != binding.source_context
        || receipt.selection.generation <= generation
        || receipt.accepted_scope.generation != receipt.selection.generation
    {
        return Err(invalid_checkpoint(
            &receipt.operation_id,
            "the accepted receipt for this frozen checkout publication",
        ));
    }
    Ok(())
}

fn accepted_producer_generation(
    publication: &FrozenIndexPublication,
    receipt: &SemanticRevisionPublicationReceipt,
    expected_generation: i64,
) -> Result<i64> {
    let observed = publication.observation()?.producer_base.as_ref();
    let accepted = receipt.producer_base.as_ref();
    if let (Some(observed), Some(accepted)) = (observed, accepted) {
        if observed.expected_generation == expected_generation
            && accepted.selection_id == observed.selection_id
            && expected_generation.checked_add(1) == Some(accepted.generation)
            && accepted.project_root == receipt.selection.project_root
            && accepted.source_context == receipt.selection.source_context
            && accepted.mode == lumvise_db_core::SemanticSelectionMode::Pinned
        {
            return Ok(accepted.generation);
        }
    }
    Err(invalid_checkpoint(
        &receipt.operation_id,
        "the producer pin accepted atomically with this frozen publication",
    ))
}
