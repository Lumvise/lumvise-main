//! Durable preparation precedes authoritative publication; partial storage never becomes publishable.
use super::*;
use lumvise_contracts::{IndexBatchRequest, SemanticElementUpsert, SemanticRelationshipUpsert};
use lumvise_db_core::DbError;
use sha2::{Digest, Sha256};

const PAGE_RECORDS: usize = 1000;

impl IndexPublicationJournal {
    /// Freezes the producer's exact input before Semantic normalizes or publishes it.
    /// Example: a restart replays these bytes even when filesystem content has changed.
    pub(crate) fn freeze(
        &self,
        publication: &FrozenIndexPublication,
        control: &InvocationControl,
    ) -> Result<()> {
        self.validate_frozen(publication)?;
        let original = self.header(control)?;
        let digest = publication_digest(publication)?;
        if let Some(pending) = &original.pending {
            if pending.operation_id != publication.operation_id || pending.digest != digest {
                return Err(invalid_checkpoint(
                    &publication.operation_id,
                    "the identical pending request before starting another publication",
                ));
            }
            if pending.ready {
                self.pending(control)?;
                return Ok(());
            }
        }
        let partition = publication.partition();
        if &self.observation(&partition, control)? != publication.observation()? {
            return Err(invalid_checkpoint(
                &publication.operation_id,
                "the checkout's observed scope before indexing",
            ));
        }
        let mut writing = original.clone();
        writing.pending = Some(PendingIdentity {
            operation_id: publication.operation_id.clone(),
            digest: digest.clone(),
            ready: false,
        });
        let descriptor = describe(publication);
        self.rows.apply(
            vec![
                self.rows.require(Some(&original))?,
                self.rows.put("header", &writing)?,
                self.rows
                    .put(&storage::pending_key(&digest, "descriptor"), &descriptor)?,
            ],
            control,
        )?;
        self.write_pages(
            &writing,
            "elements",
            &publication.batch.semantic_elements,
            control,
        )?;
        self.write_pages(
            &writing,
            "relationships",
            &publication.batch.semantic_relationships,
            control,
        )?;
        let mut ready = writing.clone();
        ready.pending = Some(PendingIdentity {
            operation_id: publication.operation_id.clone(),
            digest,
            ready: true,
        });
        self.rows.apply(
            vec![
                self.rows.require(Some(&writing))?,
                self.rows.put("header", &ready)?,
            ],
            control,
        )
    }

    /// Loads the frozen operation; a draft is never returned as a complete request.
    /// Example: consult this before scanning again after a restart.
    pub(crate) fn pending(
        &self,
        control: &InvocationControl,
    ) -> Result<Option<FrozenIndexPublication>> {
        let header = self.header(control)?;
        let Some(identity) = &header.pending else {
            return Ok(None);
        };
        if !identity.ready {
            return Err(invalid_checkpoint(
                &identity.operation_id,
                "a fully prepared request or recovery of its unpublished draft",
            ));
        }
        let mut descriptor = self.descriptor(identity, control)?;
        let batch = &mut descriptor.publication.batch;
        batch.semantic_elements = self.read_pages::<SemanticElementUpsert>(
            identity,
            "elements",
            descriptor.element_pages,
            control,
        )?;
        batch.semantic_relationships = self.read_pages::<SemanticRelationshipUpsert>(
            identity,
            "relationships",
            descriptor.relationship_pages,
            control,
        )?;
        if publication_digest(&descriptor.publication)? != identity.digest
            || self.header(control)? != header
        {
            return Err(invalid_checkpoint(
                &identity.operation_id,
                "an intact frozen request and unchanged checkpoint",
            ));
        }
        Ok(Some(descriptor.publication))
    }

    /// Recovers storage interrupted before readiness; published/ready work must be replayed.
    /// Example: discard an unpublished draft before a new scan, never a ready operation.
    pub(crate) fn discard_unpublished_draft(&self, control: &InvocationControl) -> Result<()> {
        let original = self.header(control)?;
        let Some(identity) = &original.pending else {
            return Ok(());
        };
        if identity.ready {
            return Err(invalid_checkpoint(
                &identity.operation_id,
                "replay or explicit conflict resolution for ready work",
            ));
        }
        let descriptor = self.descriptor(identity, control)?;
        let mut cleared = original.clone();
        cleared.pending = None;
        let mut changes = self.pending_deletions(identity, &descriptor);
        changes.extend([
            self.rows.require(Some(&original))?,
            self.rows.put("header", &cleared)?,
        ]);
        self.rows.apply(changes, control)
    }

    pub(super) fn descriptor(
        &self,
        identity: &PendingIdentity,
        control: &InvocationControl,
    ) -> Result<FrozenDescriptor> {
        self.rows
            .get(
                &storage::pending_key(&identity.digest, "descriptor"),
                control,
            )?
            .ok_or_else(|| {
                invalid_checkpoint(
                    &identity.operation_id,
                    "a durable frozen-request descriptor",
                )
            })
    }

    fn write_pages<T: serde::Serialize>(
        &self,
        header: &CheckpointHeader,
        kind: &str,
        records: &[T],
        control: &InvocationControl,
    ) -> Result<()> {
        let identity = header.pending.as_ref().ok_or_else(|| {
            invalid_checkpoint(
                &self.binding.checkout_id,
                "a draft identity for frozen request pages",
            )
        })?;
        for (index, page) in records.chunks(PAGE_RECORDS).enumerate() {
            self.rows.apply(
                vec![
                    self.rows.require(Some(header))?,
                    self.rows.put(
                        &storage::pending_key(&identity.digest, &format!("{kind}/{index:016}")),
                        &page,
                    )?,
                ],
                control,
            )?;
        }
        Ok(())
    }

    fn read_pages<T: serde::de::DeserializeOwned>(
        &self,
        identity: &PendingIdentity,
        kind: &str,
        count: usize,
        control: &InvocationControl,
    ) -> Result<Vec<T>> {
        let mut records = Vec::new();
        for index in 0..count {
            let page: Vec<T> = self
                .rows
                .get(
                    &storage::pending_key(&identity.digest, &format!("{kind}/{index:016}")),
                    control,
                )?
                .ok_or_else(|| {
                    invalid_checkpoint(&identity.operation_id, "every frozen request page")
                })?;
            records.extend(page);
        }
        Ok(records)
    }

    pub(super) fn pending_deletions(
        &self,
        identity: &PendingIdentity,
        descriptor: &FrozenDescriptor,
    ) -> Vec<lumvise_db_core::PluginDataMutation> {
        let mut changes = vec![
            self.rows
                .delete(&storage::pending_key(&identity.digest, "descriptor")),
        ];
        for (kind, count) in [
            ("elements", descriptor.element_pages),
            ("relationships", descriptor.relationship_pages),
        ] {
            for index in 0..count {
                changes.push(self.rows.delete(&storage::pending_key(
                    &identity.digest,
                    &format!("{kind}/{index:016}"),
                )));
            }
        }
        changes
    }
}

fn describe(publication: &FrozenIndexPublication) -> FrozenDescriptor {
    FrozenDescriptor {
        publication: FrozenIndexPublication {
            operation_id: publication.operation_id.clone(),
            batch: metadata_batch(&publication.batch),
            inventory: publication.inventory.clone(),
        },
        element_pages: publication
            .batch
            .semantic_elements
            .len()
            .div_ceil(PAGE_RECORDS),
        relationship_pages: publication
            .batch
            .semantic_relationships
            .len()
            .div_ceil(PAGE_RECORDS),
    }
}

fn metadata_batch(batch: &IndexBatchRequest) -> IndexBatchRequest {
    IndexBatchRequest {
        provider_instance_id: batch.provider_instance_id.clone(),
        project_root: batch.project_root.clone(),
        revision_observation: batch.revision_observation.clone(),
        replace_paths: batch.replace_paths.clone(),
        removed_paths: batch.removed_paths.clone(),
        semantic_sources: batch.semantic_sources.clone(),
        semantic_elements: Vec::new(),
        semantic_relationships: Vec::new(),
    }
}

fn publication_digest(publication: &FrozenIndexPublication) -> Result<String> {
    let bytes = serde_json::to_vec(publication).map_err(DbError::from)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
