//! Freezes only exact references. Live references keep their explicit selection and create no claim.
use super::*;
use lumvise_contracts::{
    KnowledgeDocument, KnowledgeDocumentReference, SemanticCitationRetentionAcquire,
    SemanticCitationRetentionIntent,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(in crate::documents) struct DocumentCitationEvidence {
    pub dependencies: Vec<Value>,
    pub claims: Vec<SemanticCitationRetentionAcquire>,
}

pub(in crate::documents) fn prepare(
    context: &mut PluginContext<'_>,
    document: &mut KnowledgeDocument,
    operation_id: &str,
) -> Result<DocumentCitationEvidence, PluginError> {
    let mut evidence = DocumentCitationEvidence::default();
    let mut groups = BTreeMap::<(String, String, String), ProjectEvidence>::new();
    for reference in &document.references {
        let KnowledgeDocumentReference::Qualified(citation) = reference else {
            continue;
        };
        let exact = is_exact(&citation.target);
        let read = resolve(context, citation, exact)?;
        if !exact {
            continue;
        }
        if read.state != KnowledgeCitationReadState::Available {
            return Err(invalid(
                &json!(citation),
                "available exact evidence before publication",
            ));
        }
        collect(
            &mut evidence,
            &mut groups,
            &document.project_root,
            &read.reference,
        )?;
    }
    if !groups.is_empty() {
        let intent = intent(context, document, operation_id)?;
        evidence.claims = groups
            .into_values()
            .map(|group| group.claim(&intent))
            .collect();
    }
    // Citation reads can add source provenance; capture it before freezing the payload.
    super::super::spaces::protect_write(context, document)?;
    Ok(evidence)
}

fn is_exact(target: &KnowledgeCitationTarget) -> bool {
    match target {
        KnowledgeCitationTarget::SemanticElement { selection, .. }
        | KnowledgeCitationTarget::Artifact { selection, .. } => {
            matches!(selection, KnowledgeRevisionCitationSelection::Exact { .. })
        }
        KnowledgeCitationTarget::Document { selection, .. } => {
            matches!(selection, KnowledgeDocumentCitationSelection::Exact { .. })
        }
    }
}

fn collect(
    evidence: &mut DocumentCitationEvidence,
    groups: &mut BTreeMap<(String, String, String), ProjectEvidence>,
    destination_root: &str,
    citation: &KnowledgeQualifiedCitation,
) -> Result<(), PluginError> {
    let (record, edit) = record(&citation.target)?;
    if citation.project.project_root == destination_root {
        let target = match record.kind {
            lumvise_contracts::SemanticRecordKind::Element => {
                json!({"target_kind":"semantic_element", "semantic_element_id":record.record_id})
            }
            lumvise_contracts::SemanticRecordKind::Artifact => {
                json!({"target_kind":"artifact", "artifact_id":record.record_id, "edit_version":edit})
            }
        };
        let dependency = json!({"target":target});
        if !evidence.dependencies.contains(&dependency) {
            evidence.dependencies.push(dependency);
        }
        return Ok(());
    }
    let project = &citation.project;
    let key = (
        project.storage_authority.clone(),
        project.project_id.clone(),
        project.project_root.clone(),
    );
    groups
        .entry(key)
        .or_insert_with(|| ProjectEvidence::new(project.clone()))
        .add(record, edit)
}

fn record(
    target: &KnowledgeCitationTarget,
) -> Result<(SemanticRecordRef, Option<u64>), PluginError> {
    match target {
        KnowledgeCitationTarget::SemanticElement {
            semantic_element_id,
            ..
        } => Ok((SemanticRecordRef::element(semantic_element_id), None)),
        KnowledgeCitationTarget::Artifact {
            artifact_id,
            edit_version,
            ..
        } => Ok((SemanticRecordRef::artifact(artifact_id), *edit_version)),
        KnowledgeCitationTarget::Document {
            selection:
                KnowledgeDocumentCitationSelection::Exact {
                    semantic_element_id,
                    ..
                },
            ..
        } => Ok((SemanticRecordRef::element(semantic_element_id), None)),
        _ => Err(invalid(
            &json!(target),
            "the exact observed document locator",
        )),
    }
}

fn intent(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    operation_id: &str,
) -> Result<SemanticCitationRetentionIntent, PluginError> {
    let output = context.host_call("runtime.knowledge_scope", json!({"operation":"citation", "request":{
        "operation":"intent", "project_root":document.project_root, "document_id":document.document_id, "publication_id":operation_id}}))?;
    crate::parse(
        output["intent"].clone(),
        "the host-issued citing space and publication intent",
    )
}

struct ProjectEvidence {
    project: KnowledgeProjectIdentity,
    records: BTreeSet<SemanticRecordRef>,
    edits: BTreeMap<String, u64>,
}

impl ProjectEvidence {
    fn new(project: KnowledgeProjectIdentity) -> Self {
        Self {
            project,
            records: BTreeSet::new(),
            edits: BTreeMap::new(),
        }
    }

    fn add(&mut self, record: SemanticRecordRef, edit: Option<u64>) -> Result<(), PluginError> {
        if let Some(edit) = edit {
            if self
                .edits
                .insert(record.record_id.clone(), edit)
                .is_some_and(|previous| previous != edit)
            {
                return Err(invalid(
                    &json!(record),
                    "one original edit per cited artifact",
                ));
            }
        }
        self.records.insert(record);
        Ok(())
    }

    fn claim(self, intent: &SemanticCitationRetentionIntent) -> SemanticCitationRetentionAcquire {
        SemanticCitationRetentionAcquire {
            project_root: self.project.project_root,
            project_id: self.project.project_id,
            // This pin never follows a family. Each reference preserves its original source context.
            source_context: "knowledge:citations".into(),
            intent: intent.clone(),
            records: self.records.into_iter().collect(),
            artifact_edits: self.edits,
        }
    }
}
