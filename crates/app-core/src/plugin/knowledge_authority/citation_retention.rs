//! Authenticates Knowledge's frozen destination intent before source-owned retention.
//! The journal stays Knowledge-owned; this boundary checks opaque original observations.
use super::*;
use lumvise_db_core::{
    SemanticCitationRetentionAcquire, SemanticCitationRetentionIntent,
    SemanticCitationRetentionRelease, SemanticRecordRef, SemanticRevisionBody,
};

const KNOWLEDGE: &str = "builtin.knowledge";

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CitationAuthorityRequest {
    Project {
        project_root: String,
    },
    Intent {
        project_root: String,
        document_id: String,
        publication_id: String,
    },
    Acquire {
        claim: SemanticCitationRetentionAcquire,
        observation: CitationPublicationObservation,
    },
    Release {
        claim: SemanticCitationRetentionRelease,
        observation: CitationPublicationObservation,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CitationPublicationObservation {
    project_root: String,
    table: String,
    expected: Value,
}

pub(super) struct CitationAuthority<'a> {
    authority: &'a KnowledgeAuthority,
    semantic: &'a dyn SemanticPersistence,
    context: &'a PluginInvocationContext,
    control: &'a InvocationControl,
}

impl<'a> CitationAuthority<'a> {
    pub(super) fn new(
        authority: &'a KnowledgeAuthority,
        plugin: &str,
        semantic: &'a dyn SemanticPersistence,
        context: &'a PluginInvocationContext,
        control: &'a InvocationControl,
    ) -> Result<Self, HostCapabilityError> {
        if plugin != KNOWLEDGE
            || context
                .plugin_chain()
                .last()
                .is_none_or(|frame| frame.plugin_id != KNOWLEDGE)
        {
            return Err(invalid(
                &json!(plugin),
                "a signed Knowledge invocation for citation retention",
            ));
        }
        Ok(Self {
            authority,
            semantic,
            context,
            control,
        })
    }

    pub(super) fn invoke(
        &self,
        request: CitationAuthorityRequest,
    ) -> Result<Value, HostCapabilityError> {
        match request {
            CitationAuthorityRequest::Project { project_root } => self
                .project(&project_root, false)
                .map(|project| json!({"project":project})),
            CitationAuthorityRequest::Intent {
                project_root,
                document_id,
                publication_id,
            } => self
                .intent(&project_root, document_id, publication_id)
                .map(|intent| json!({"intent":intent})),
            CitationAuthorityRequest::Acquire { claim, observation } => {
                self.acquire(claim, observation)
            }
            CitationAuthorityRequest::Release { claim, observation } => {
                self.release(claim, observation)
            }
        }
    }

    fn project(
        &self,
        root: &str,
        writable: bool,
    ) -> Result<KnowledgeProjectIdentity, HostCapabilityError> {
        if !is_memory_root(root) {
            self.authority.authorize_source(
                self.context,
                self.semantic,
                root,
                writable,
                self.control,
            )?;
            let grant = resolve_grant(
                self.semantic,
                root,
                &self.authority.identity(self.control)?,
                self.control,
            )?;
            if writable && !grant.writable {
                return Err(invalid(&json!(root), "current source write permission"));
            }
            return Ok(grant.project);
        }
        let scope = self
            .authority
            .current(self.semantic, self.context, self.control)?;
        if writable && !super::project_access::memory_writable(&scope) {
            return Err(invalid(&json!(root), "current memory write permission"));
        }
        let space = scope
            .space
            .filter(|space| space.project_root == root)
            .ok_or_else(|| invalid(&json!(root), "the caller's owned citation space"))?;
        let SemanticResult::ProjectIdentity(identity) = self
            .semantic
            .execute(
                SemanticOperation::ProjectIdentity {
                    project_root: root.into(),
                },
                self.control,
            )
            .map_err(scope_failure)?
        else {
            return Err(invalid(&json!(root), "canonical citation space lineage"));
        };
        Ok(KnowledgeProjectIdentity {
            project_id: identity.project_id,
            project_root: root.into(),
            storage_authority: space.storage_authority,
        })
    }

    fn intent(
        &self,
        root: &str,
        document: String,
        publication: String,
    ) -> Result<SemanticCitationRetentionIntent, HostCapabilityError> {
        if document.trim().is_empty() || publication.trim().is_empty() {
            return Err(invalid(
                &json!([document, publication]),
                "non-empty citing document and publication identities",
            ));
        }
        let project = self.project(root, true)?;
        let space_id = if is_memory_root(root) {
            self.authority
                .current(self.semantic, self.context, self.control)?
                .space
                .unwrap()
                .space_id
        } else {
            format!(
                "project:{}:{}",
                project.storage_authority, project.project_id
            )
        };
        Ok(SemanticCitationRetentionIntent {
            space_id,
            document_id: document,
            publication_id: publication,
        })
    }

    fn require_observation(
        &self,
        intent: &SemanticCitationRetentionIntent,
        observation: &CitationPublicationObservation,
    ) -> Result<(), HostCapabilityError> {
        let expected_intent = self.intent(
            &observation.project_root,
            intent.document_id.clone(),
            intent.publication_id.clone(),
        )?;
        if expected_intent != *intent
            || observation.expected.is_null()
            || observation.table.trim().is_empty()
        {
            return Err(invalid(
                &json!(intent),
                "the owned destination and original frozen Knowledge journal observation",
            ));
        }
        storage::require_owner_observation(
            self.authority.relational.as_ref(),
            KNOWLEDGE,
            &observation.table,
            &intent.publication_id,
            Some(&observation.expected),
            self.control,
        )
    }

    fn acquire(
        &self,
        claim: SemanticCitationRetentionAcquire,
        observation: CitationPublicationObservation,
    ) -> Result<Value, HostCapabilityError> {
        self.require_observation(&claim.intent, &observation)?;
        self.require_source(&claim)?;
        let output = self
            .semantic
            .execute(
                SemanticOperation::AcquireCitationRetention(claim.clone()),
                self.control,
            )
            .map_err(scope_failure)?;
        self.require_source(&claim)?;
        self.require_observation(&claim.intent, &observation)?;
        publication_result(output)
    }

    fn require_source(
        &self,
        claim: &SemanticCitationRetentionAcquire,
    ) -> Result<(), HostCapabilityError> {
        let project = self.project(&claim.project_root, false)?;
        if project.project_id != claim.project_id {
            return Err(invalid(
                &json!(claim.project_id),
                "the original authorized source lineage",
            ));
        }
        for record in &claim.records {
            self.require_record(&claim.project_root, record)?;
        }
        Ok(())
    }

    fn require_record(
        &self,
        root: &str,
        record: &SemanticRecordRef,
    ) -> Result<(), HostCapabilityError> {
        let read = semantic_access::KnowledgeSemanticAccess {
            semantic: self.semantic,
            authority: self.authority,
            context: self.context,
            control: self.control,
        };
        match read
            .execute(SemanticOperation::ResolveRevision {
                project_root: root.into(),
                record: record.clone(),
            })
            .map_err(scope_failure)?
        {
            SemanticResult::RevisionResolution(Some(revision))
                if revision.body != SemanticRevisionBody::Pruned =>
            {
                Ok(())
            }
            _ => Err(invalid(
                &json!(record),
                "an authorized exact citation body in the source project",
            )),
        }
    }

    fn release(
        &self,
        claim: SemanticCitationRetentionRelease,
        observation: CitationPublicationObservation,
    ) -> Result<Value, HostCapabilityError> {
        self.require_observation(&claim.intent, &observation)?;
        let output = self
            .semantic
            .execute(
                SemanticOperation::ReleaseCitationRetention(claim.clone()),
                self.control,
            )
            .map_err(scope_failure)?;
        self.require_observation(&claim.intent, &observation)?;
        publication_result(output)
    }
}

fn publication_result(result: SemanticResult) -> Result<Value, HostCapabilityError> {
    match result {
        SemanticResult::RevisionPublication(outcome) => Ok(json!({"publication":outcome})),
        other => Err(invalid(
            &json!(other),
            "source-owned citation publication result",
        )),
    }
}
