//! Establish a base only from matching indexed bytes, then read the durable base rather than head.
use super::*;
use lumvise_contracts::SemanticIndexObservation;
use lumvise_db_core::{
    SemanticPartition, SemanticRecordRef, SemanticRevisionSelectionSnapshot,
    SemanticSelectionExpectation,
};
use std::collections::{BTreeMap, BTreeSet};

impl IndexPublicationJournal {
    /// Establishes a new checkout from an empty source or matching indexed file inventory.
    /// Example: call once after scanning; later calls preserve offline edits and pending work.
    pub(crate) fn attach(
        &self,
        inventory: &[IndexPathDigest],
        control: &InvocationControl,
    ) -> Result<()> {
        self.rows.ensure(control)?;
        if self
            .rows
            .get::<CheckpointHeader>("header", control)?
            .is_some()
        {
            self.header(control)?;
            return Ok(());
        }
        let manifest = inventory_map(inventory)?;
        let snapshot = self.source_snapshot(control)?;
        let paths = match &snapshot {
            Some(snapshot) => verified_paths(snapshot, &self.binding, &manifest)?,
            None => Vec::new(), // An authoritative empty source is a valid base for first publication.
        };
        let header = CheckpointHeader {
            binding: self.binding.clone(),
            generation: snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.selection.generation),
            producer_generation: 0,
            last_acknowledged_operation: None,
            pending: None,
        };
        let mut changes = vec![self.rows.require(None)?, self.rows.put("header", &header)?];
        for path in paths {
            changes.extend(self.path_puts(&path)?);
        }
        self.rows.apply(changes, control)
    }

    /// Supplies the checkout's composite observed scope without consulting a newer server head.
    /// Example: use this token before preparing a replacement of src/a.rs.
    pub(crate) fn observation(
        &self,
        partition: &SemanticPartition,
        control: &InvocationControl,
    ) -> Result<SemanticIndexObservation> {
        self.validate_partition(partition)?;
        let header = self.header(control)?;
        let paths = self.paths_in_partition(partition, control)?;
        let expected = SemanticSelectionExpectation {
            artifact_edits: Default::default(),
            generation: header.generation,
            records: paths
                .iter()
                .flat_map(|path| path.elements.values().map(SemanticRecordRef::element))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            connection_ids: paths
                .iter()
                .flat_map(|path| path.connections.keys().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        };
        if self.header(control)? != header {
            return Err(invalid_checkpoint(
                &self.binding.checkout_id,
                "a stable checkpoint while observing source scope",
            ));
        }
        Ok(SemanticIndexObservation {
            project_id: self.binding.project_id.clone(),
            producer_base: Some(lumvise_contracts::SemanticProducerBase {
                selection_id: self.rows.producer_selection_id(),
                expected_generation: header.producer_generation,
            }),
            selection_id: self.binding.selection_id.clone(),
            source_context: self.binding.source_context.clone(),
            expected,
        })
    }

    /// Returns acknowledged digests for restart comparison; parser caches remain volatile.
    /// Example: publish only paths whose source or projection digest changed.
    pub(crate) fn inventory(&self, control: &InvocationControl) -> Result<Vec<IndexPathDigest>> {
        self.header(control)?;
        Ok(self
            .rows
            .scan::<CheckpointPath>("path/", control)?
            .into_iter()
            .map(|path| path.digest)
            .collect())
    }

    /// Resolves a producer ID against this checkout's accepted base, including an older revision.
    /// Example: rewrite a reference to an untouched file using its acknowledged ID.
    pub(crate) fn accepted_element(
        &self,
        producer_id: &str,
        control: &InvocationControl,
    ) -> Result<Option<String>> {
        let binding: Option<ParserBinding> =
            self.rows.get(&storage::parser_key(producer_id), control)?;
        Ok(binding.map(|binding| binding.accepted_id))
    }

    pub(super) fn paths_in_partition(
        &self,
        partition: &SemanticPartition,
        control: &InvocationControl,
    ) -> Result<Vec<CheckpointPath>> {
        let mut paths = BTreeMap::new();
        for prefix in partition
            .replace_paths
            .iter()
            .map(String::as_str)
            .chain(partition.replace_paths.is_empty().then_some(""))
        {
            for path in self
                .rows
                .scan::<CheckpointPath>(&storage::path_key(prefix), control)?
            {
                if partition.contains_path(&path.digest.path) {
                    paths.insert(path.digest.path.clone(), path);
                }
            }
        }
        Ok(paths.into_values().collect())
    }

    pub(super) fn validate_partition(&self, partition: &SemanticPartition) -> Result<()> {
        partition
            .validate()
            .map_err(lumvise_db_core::DbError::from)?;
        if partition.project_root != self.project_root
            || partition.provider_instance_id.as_deref() != Some(&self.binding.provider_instance_id)
        {
            return Err(invalid_checkpoint(
                &partition.project_root,
                "this checkout's project route and source provider",
            ));
        }
        Ok(())
    }

    fn source_snapshot(
        &self,
        control: &InvocationControl,
    ) -> Result<Option<SemanticRevisionSelectionSnapshot>> {
        let identity = self.semantic(
            SemanticOperation::ProjectIdentity {
                project_root: self.project_root.clone(),
            },
            control,
        )?;
        if !matches!(identity, SemanticResult::ProjectIdentity(id) if id.project_id == self.binding.project_id)
        {
            return Err(invalid_checkpoint(
                &self.binding.project_id,
                "the current project lineage",
            ));
        }
        let result = self.semantic(
            SemanticOperation::RevisionSelection {
                project_root: self.project_root.clone(),
                selection_id: self.binding.selection_id.clone(),
            },
            control,
        )?;
        let SemanticResult::RevisionSelection(snapshot) = result else {
            return Err(invalid_checkpoint(
                &self.binding.selection_id,
                "an exact revision selection",
            ));
        };
        if snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.selection.source_context != self.binding.source_context
        }) {
            return Err(invalid_checkpoint(
                &self.binding.source_context,
                "the selected source context",
            ));
        }
        Ok(snapshot)
    }

    pub(super) fn path_puts(
        &self,
        path: &CheckpointPath,
    ) -> Result<Vec<lumvise_db_core::PluginDataMutation>> {
        let mut changes = vec![self.rows.put(&storage::path_key(&path.digest.path), path)?];
        for (producer, accepted) in &path.elements {
            changes.push(self.rows.put(
                &storage::parser_key(producer),
                &ParserBinding {
                    accepted_id: accepted.clone(),
                    path: path.digest.path.clone(),
                },
            )?);
        }
        Ok(changes)
    }
}

pub(super) fn inventory_map(
    inventory: &[IndexPathDigest],
) -> Result<BTreeMap<&str, &IndexPathDigest>> {
    let mut paths = BTreeMap::new();
    for digest in inventory {
        digest.validate()?;
        if paths.insert(digest.path.as_str(), digest).is_some() {
            return Err(invalid_checkpoint(
                &digest.path,
                "one digest per source path",
            ));
        }
    }
    Ok(paths)
}

fn verified_paths(
    snapshot: &SemanticRevisionSelectionSnapshot,
    binding: &IndexCheckoutBinding,
    inventory: &BTreeMap<&str, &IndexPathDigest>,
) -> Result<Vec<CheckpointPath>> {
    let owned = snapshot
        .elements
        .iter()
        .filter(|element| {
            element.metadata["provider_instance_id"].as_str() == Some(&binding.provider_instance_id)
        })
        .collect::<Vec<_>>();
    if owned.is_empty() {
        return Ok(Vec::new());
    }
    let mut paths = BTreeMap::<String, CheckpointPath>::new();
    for element in &owned {
        add_observed_element(&mut paths, inventory, binding, element)?;
    }
    if paths.len() != inventory.len() {
        return Err(invalid_checkpoint(
            inventory.len().to_string(),
            "the indexed source's complete path inventory",
        ));
    }
    for path in paths.values() {
        verify_path_bytes(path, &owned)?;
    }
    let owners = paths
        .iter()
        .flat_map(|(path, row)| {
            row.elements
                .values()
                .map(move |id| (id.clone(), path.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    for edge in &snapshot.connections {
        let observed = ObservedConnection {
            owners: edge
                .relationship
                .owned_element_ids()
                .map(str::to_owned)
                .collect(),
        };
        for owner in &observed.owners {
            if let Some(path) = owners.get(owner.as_str()) {
                paths
                    .get_mut(path)
                    .unwrap()
                    .connections
                    .insert(edge.connection_id.clone(), observed.clone());
            }
        }
    }
    Ok(paths.into_values().collect())
}

fn add_observed_element(
    paths: &mut BTreeMap<String, CheckpointPath>,
    inventory: &BTreeMap<&str, &IndexPathDigest>,
    binding: &IndexCheckoutBinding,
    element: &lumvise_db_core::SemanticElement,
) -> Result<()> {
    if element.semantic_source_id != binding.semantic_source_id {
        return Err(invalid_checkpoint(
            &element.semantic_source_id,
            "one established logical source in this provider partition",
        ));
    }
    let digest = inventory.get(element.path.as_str()).ok_or_else(|| {
        invalid_checkpoint(&element.path, "an identical indexed path in this checkout")
    })?;
    let producer = element.metadata["producer_element_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            invalid_checkpoint(&element.semantic_element_id, "an indexed producer identity")
        })?;
    let row = paths
        .entry(element.path.clone())
        .or_insert_with(|| CheckpointPath {
            digest: IndexPathDigest {
                projection_sha256: None,
                ..(**digest).clone()
            },
            elements: BTreeMap::new(),
            connections: BTreeMap::new(),
        });
    if row
        .elements
        .insert(producer.into(), element.semantic_element_id.clone())
        .is_some()
    {
        return Err(invalid_checkpoint(
            producer,
            "one indexed revision per producer identity",
        ));
    }
    Ok(())
}

fn verify_path_bytes(
    path: &CheckpointPath,
    elements: &[&lumvise_db_core::SemanticElement],
) -> Result<()> {
    let roots = elements
        .iter()
        .filter(|element| {
            element.path == path.digest.path
                && (element.metadata["indexer_metadata"]["kind"] == "directory"
                    || element.metadata["indexer_metadata"]["source_digest_sha256"].is_string())
        })
        .collect::<Vec<_>>();
    let [root] = roots.as_slice() else {
        return Err(invalid_checkpoint(
            &path.digest.path,
            "one indexed file/directory with a captured source digest",
        ));
    };
    let directory = root.metadata["indexer_metadata"]["kind"] == "directory";
    let digest = root.metadata["indexer_metadata"]["source_digest_sha256"].as_str();
    if directory != path.digest.directory || digest != path.digest.source_sha256.as_deref() {
        return Err(invalid_checkpoint(
            &path.digest.path,
            "local bytes matching the indexed revision; select a known anchor or resolve the source difference",
        ));
    }
    Ok(())
}
