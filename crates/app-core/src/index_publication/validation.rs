//! Validate producer identity and inventory without duplicating Semantic's normalization.
use super::*;
use std::collections::BTreeSet;

impl IndexPublicationJournal {
    pub(super) fn validate_frozen(&self, publication: &FrozenIndexPublication) -> Result<()> {
        self.validate_partition(&publication.partition())?;
        self.validate_observation(publication)?;
        self.validate_projected_paths(publication)?;
        self.validate_projected_source(publication)
    }

    fn validate_observation(&self, publication: &FrozenIndexPublication) -> Result<()> {
        let observed = publication.observation()?;
        if publication.operation_id.trim().is_empty()
            || observed.project_id != self.binding.project_id
            || observed.selection_id != self.binding.selection_id
            || observed.source_context != self.binding.source_context
            || observed.producer_base.as_ref().is_none_or(|base| {
                base.selection_id != self.rows.producer_selection_id()
                    || base.expected_generation < 0
            })
        {
            return Err(invalid_checkpoint(
                &publication.operation_id,
                "this checkout's observed project, selection and source context",
            ));
        }
        Ok(())
    }

    fn validate_projected_paths(&self, publication: &FrozenIndexPublication) -> Result<()> {
        let manifest = adoption::inventory_map(&publication.inventory)?;
        let paths = publication
            .batch
            .semantic_elements
            .iter()
            .map(|element| element.path.as_str())
            .collect::<BTreeSet<_>>();
        if paths != manifest.keys().copied().collect() {
            return Err(invalid_checkpoint(
                &publication.operation_id,
                "a frozen digest for every projected path, including reference updates",
            ));
        }
        let partition = publication.partition();
        let exact = partition.replace_paths.iter().collect::<BTreeSet<_>>();
        for element in &publication.batch.semantic_elements {
            if !exact.contains(&element.path) && !partition.contains_path(&element.path) {
                return Err(invalid_checkpoint(
                    &element.path,
                    "a path in the frozen source partition",
                ));
            }
        }
        Ok(())
    }

    fn validate_projected_source(&self, publication: &FrozenIndexPublication) -> Result<()> {
        for id in publication
            .batch
            .semantic_elements
            .iter()
            .map(|element| &element.semantic_source_id)
            .chain(
                publication
                    .batch
                    .semantic_sources
                    .iter()
                    .map(|source| &source.semantic_source_id),
            )
        {
            if id != &self.binding.semantic_source_id {
                return Err(invalid_checkpoint(
                    id,
                    "this checkout's established logical source",
                ));
            }
        }
        Ok(())
    }
}
