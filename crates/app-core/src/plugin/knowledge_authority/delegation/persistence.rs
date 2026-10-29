//! Frozen permission intents use the authority owner's existing conditional rows.
use super::*;

impl MemoryDelegationAuthority<'_> {
    pub(super) fn save(&self, intent: MemoryDelegationSave) -> Result<Value, HostCapabilityError> {
        let mut owner = self.owner()?;
        if owner
            .space
            .as_ref()
            .is_some_and(|space| space.space_kind == KnowledgeSpaceKind::Agent)
        {
            return Err(invalid(
                &json!(owner.space),
                "Assistant or MCP delegation; Agent hooks use their existing owner",
            ));
        }
        let id = delegation_id(&owner, &intent.document_id)?;
        let digest = permission_digest(&id, &intent);
        let receipt_key = format!("delegation-intent:{id}:{}", intent.operation_id);
        if let Some(receipt) = storage::read(
            self.authority.relational.as_ref(),
            &receipt_key,
            self.control,
        )? {
            return replay(receipt, &digest);
        }
        restrict_projects(&mut owner, &intent)?;
        let previous = read_delegation(self.authority, &id, self.control)?;
        let next = prepare(&id, owner, previous.as_ref(), &intent)?;
        let receipt = json!({"digest":digest, "delegation":next});
        self.persist(&id, previous, next, &receipt_key, receipt)
    }

    fn persist(
        &self,
        id: &str,
        previous: Option<StoredMemoryDelegation>,
        next: StoredMemoryDelegation,
        receipt_key: &str,
        receipt: Value,
    ) -> Result<Value, HostCapabilityError> {
        let result = storage::replace_pair(
            self.authority.relational.as_ref(),
            (
                &format!("delegation:{id}"),
                previous.map(|value| json!(value)),
                json!(next),
            ),
            (receipt_key, None, receipt.clone()),
            self.control,
        );
        // A lost acceptance or concurrent identical retry resolves through its original receipt.
        if let Some(saved) = storage::read(
            self.authority.relational.as_ref(),
            receipt_key,
            self.control,
        )? {
            return replay(saved, receipt["digest"].as_str().expect("intent digest"));
        }
        result?;
        Err(invalid(
            &json!(id),
            "durably acknowledged delegation permission",
        ))
    }
}

fn prepare(
    id: &str,
    mut owner: KnowledgeAccessContext,
    previous: Option<&StoredMemoryDelegation>,
    intent: &MemoryDelegationSave,
) -> Result<StoredMemoryDelegation, HostCapabilityError> {
    if intent.operation_id.trim().is_empty()
        || previous.map(|row| row.revision) != intent.expected_revision
    {
        return Err(invalid(
            &json!(intent),
            "a nonempty operation ID and original delegation revision",
        ));
    }
    let revision = previous
        .map_or(0, |row| row.revision)
        .checked_add(1)
        .ok_or_else(|| invalid(&json!(id), "delegation revision below u64::MAX"))?;
    retain_executor_sources(&mut owner, previous);
    Ok(StoredMemoryDelegation {
        delegation_id: id.into(),
        executor_agent_id: id.into(),
        revision,
        document_id: intent.document_id.clone(),
        enabled: intent.enabled,
        inputs: intent.inputs.clone(),
        owner,
        executor_observations: Vec::new(),
        executor_preparation: None,
    })
}

fn retain_executor_sources(
    owner: &mut KnowledgeAccessContext,
    previous: Option<&StoredMemoryDelegation>,
) {
    // Removing a watch never declassifies the executor's already persisted Mission history.
    if let Some(previous) = previous {
        for grant in &previous.owner.project_grants {
            if !owner
                .project_grants
                .iter()
                .any(|current| current.project.same_project(&grant.project))
            {
                owner.project_grants.push(grant.clone());
            }
        }
    }
    owner.source_projects = owner
        .project_grants
        .iter()
        .map(|grant| grant.project.clone())
        .collect();
}

fn restrict_projects(
    owner: &mut KnowledgeAccessContext,
    intent: &MemoryDelegationSave,
) -> Result<(), HostCapabilityError> {
    let mut required = intent.required_projects.clone();
    required.extend(owner.source_projects.clone());
    required.extend(
        owner
            .space
            .as_ref()
            .and_then(|space| space.shared_project.clone()),
    );
    for input in &intent.inputs {
        if let KnowledgeDocumentReference::Qualified(citation) = input {
            if !is_memory_root(&citation.project.project_root) {
                required.push(citation.project.clone());
            } else if owner
                .space
                .as_ref()
                .is_none_or(|space| space.project_root != citation.project.project_root)
            {
                return Err(invalid(
                    &json!(citation.project),
                    "the originating owner's private space",
                ));
            }
        }
    }
    for project in &required {
        if !owner
            .project_grants
            .iter()
            .any(|grant| grant.project.same_project(project))
        {
            return Err(invalid(
                &json!(project),
                "an input within the originating owner's current project rights",
            ));
        }
    }
    owner.project_grants.retain(|grant| {
        required
            .iter()
            .any(|project| project.same_project(&grant.project))
    });
    owner.source_projects = owner
        .project_grants
        .iter()
        .map(|grant| grant.project.clone())
        .collect();
    Ok(())
}

fn permission_digest(id: &str, intent: &MemoryDelegationSave) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([id, intent])).expect("serializable permission intent")
        )
    )
}

fn replay(receipt: Value, digest: &str) -> Result<Value, HostCapabilityError> {
    if receipt["digest"] != digest {
        return Err(invalid(
            &receipt["digest"],
            "the original delegation request",
        ));
    }
    Ok(json!({"delegation":receipt["delegation"]}))
}
