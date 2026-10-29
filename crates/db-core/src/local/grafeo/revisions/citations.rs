//! Local citation claims share the existing graph/SQL selection commit boundary.
use super::*;

// Local has one owning profile; remote adapters must supply their verified account subject.
const LOCAL_CITATION_ACTOR: &str = "local";

impl SemanticStorage<'_> {
    pub(crate) fn acquire_citation_retention(
        &self,
        request: &SemanticCitationRetentionAcquire,
    ) -> Result<SemanticRevisionPublicationOutcome> {
        self.commit_selection_plan(&request.project_root, |state, timestamp, commit| {
            state.plan_citation_acquisition(request, LOCAL_CITATION_ACTOR, timestamp, commit)
        })
    }

    pub(crate) fn release_citation_retention(
        &self,
        request: &SemanticCitationRetentionRelease,
    ) -> Result<SemanticRevisionPublicationOutcome> {
        self.commit_selection_plan(&request.project_root, |state, timestamp, commit| {
            state.plan_citation_release(request, LOCAL_CITATION_ACTOR, timestamp, commit)
        })
    }
}
