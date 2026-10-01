use super::codec;
use std::sync::Arc;

use lumvise_resource_routing::{
    InvocationControl, ResourceInvocationClient, TransportError,
    protocol::{
        CapabilityReadinessV1, InvocationEnvelopeV1, InvocationTerminalStatusV1, PROTOCOL_MAJOR,
        PROTOCOL_MINOR, PersistenceResultV1, ReadinessRequestV1, RelationalOperationV1,
        ResourceCapabilityV1, SemanticOperationV1, TypedBinaryChunkV1,
        invocation_envelope_v1::Payload, invocation_start_v1::Operation,
        invocation_terminal_v1::Result as TerminalResult,
    },
};
use uuid::Uuid;

use crate::interface::{
    PersistenceResult, RelationalOperation, RelationalPersistence, RelationalReadiness,
    RelationalResult, SemanticOperation, SemanticPersistence, SemanticReadiness, SemanticResult,
};
use crate::{DbError, Result};

const SEMANTIC_OPERATION_TYPE: &str = "lumvise.db-core.semantic-operation.bincode.v3";
const SEMANTIC_RESULT_TYPE: &str = "lumvise.db-core.semantic-result.bincode.v3";
const RELATIONAL_OPERATION_TYPE: &str = "lumvise.db-core.relational-operation.bincode.v1";
const RELATIONAL_RESULT_TYPE: &str = "lumvise.db-core.relational-result.bincode.v1";

/// Centralized persistence facade over one authenticated resource client.
///
/// Both persistence traits deliberately share this client and instance id.
pub struct CentralizedPersistence {
    client: Arc<dyn ResourceInvocationClient>,
    client_instance_id: String,
}

impl CentralizedPersistence {
    pub fn new(
        client: Arc<dyn ResourceInvocationClient>,
        client_instance_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            client_instance_id: client_instance_id.into(),
        }
    }

    pub fn encode_semantic_operation(operation: SemanticOperation) -> Result<SemanticOperationV1> {
        codec::encode_semantic_operation(operation)
    }

    pub fn decode_semantic_operation(operation: &SemanticOperationV1) -> Result<SemanticOperation> {
        codec::decode_semantic_operation(operation)
    }

    pub fn encode_semantic_result(result: SemanticResult) -> Result<PersistenceResultV1> {
        codec::encode_semantic_result(result)
    }

    pub fn decode_semantic_result(result: &PersistenceResultV1) -> Result<SemanticResult> {
        codec::decode_semantic_result(result)
    }

    pub fn encode_relational_operation(
        operation: RelationalOperation,
    ) -> Result<RelationalOperationV1> {
        codec::encode_relational_operation(operation)
    }

    pub fn decode_relational_operation(
        operation: &RelationalOperationV1,
    ) -> Result<RelationalOperation> {
        codec::decode_relational_operation(operation)
    }

    pub fn encode_relational_result(result: RelationalResult) -> Result<PersistenceResultV1> {
        codec::encode_relational_result(result)
    }

    pub fn decode_relational_result(result: &PersistenceResultV1) -> Result<RelationalResult> {
        codec::decode_relational_result(result)
    }
}

impl SemanticPersistence for CentralizedPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        ensure_active(control)?;
        let request_id = Uuid::new_v4().to_string();
        let operation = encode_semantic_operation(operation)?;
        let envelopes = self
            .client
            .invoke(
                &[start_envelope(
                    request_id,
                    self.client_instance_id.clone(),
                    Operation::SemanticOperation(operation),
                    control,
                )],
                control,
            )
            .map_err(transport_error)?;
        let terminal = terminal(&envelopes)?;
        ensure_completed(terminal)?;
        let Some(TerminalResult::Semantic(result)) = terminal.result.as_ref() else {
            return Err(protocol_error("semantic terminal result"));
        };
        decode_semantic_result(result)
    }

    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        Ok(SemanticReadiness {
            ready: capability_ready(
                self.client.as_ref(),
                &self.client_instance_id,
                ResourceCapabilityV1::GraphPersistence,
            )?,
        })
    }
}

impl RelationalPersistence for CentralizedPersistence {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        ensure_active(control)?;
        let request_id = Uuid::new_v4().to_string();
        let operation = encode_relational_operation(operation)?;
        let envelopes = self
            .client
            .invoke(
                &[start_envelope(
                    request_id,
                    self.client_instance_id.clone(),
                    Operation::RelationalOperation(operation),
                    control,
                )],
                control,
            )
            .map_err(transport_error)?;
        let terminal = terminal(&envelopes)?;
        ensure_completed(terminal)?;
        let Some(TerminalResult::Relational(result)) = terminal.result.as_ref() else {
            return Err(protocol_error("relational terminal result"));
        };
        decode_relational_result(result)
    }

    fn readiness(&self) -> PersistenceResult<RelationalReadiness> {
        Ok(RelationalReadiness {
            ready: capability_ready(
                self.client.as_ref(),
                &self.client_instance_id,
                ResourceCapabilityV1::SqlPersistence,
            )?,
        })
    }
}

pub(super) fn encode_semantic_operation(
    operation: SemanticOperation,
) -> Result<SemanticOperationV1> {
    let operation_name = semantic_operation_name(&operation).into();
    Ok(SemanticOperationV1 {
        operation_name,
        records: vec![binary_record(SEMANTIC_OPERATION_TYPE, &operation)?],
    })
}

pub(super) fn decode_semantic_operation(
    operation: &SemanticOperationV1,
) -> Result<SemanticOperation> {
    let decoded: SemanticOperation = decode_record(operation, SEMANTIC_OPERATION_TYPE)?;
    verify_name(&operation.operation_name, semantic_operation_name(&decoded))?;
    Ok(decoded)
}

pub(super) fn encode_semantic_result(result: SemanticResult) -> Result<PersistenceResultV1> {
    if let SemanticResult::ScopedGraph(graph) = result {
        return Ok(PersistenceResultV1 {
            operation_name: "ScopedGraph".into(),
            records: vec![binary_record(
                "lumvise.db-core.scoped-graph.bincode.v1",
                &super::scoped_graph_codec::encode(graph)?,
            )?],
        });
    }
    let operation_name = semantic_result_name(&result).into();
    Ok(PersistenceResultV1 {
        operation_name,
        records: vec![binary_record(SEMANTIC_RESULT_TYPE, &result)?],
    })
}

pub(super) fn decode_semantic_result(result: &PersistenceResultV1) -> Result<SemanticResult> {
    if result.operation_name == "ScopedGraph" {
        return super::scoped_graph_codec::decode(decode_record(
            result,
            "lumvise.db-core.scoped-graph.bincode.v1",
        )?)
        .map(SemanticResult::ScopedGraph);
    }
    let decoded: SemanticResult = decode_record(result, SEMANTIC_RESULT_TYPE)?;
    verify_name(&result.operation_name, semantic_result_name(&decoded))?;
    Ok(decoded)
}

pub(super) fn encode_relational_operation(
    operation: RelationalOperation,
) -> Result<RelationalOperationV1> {
    let operation_name = relational_operation_name(&operation).into();
    Ok(RelationalOperationV1 {
        operation_name,
        records: vec![binary_record(RELATIONAL_OPERATION_TYPE, &operation)?],
    })
}

pub(super) fn decode_relational_operation(
    operation: &RelationalOperationV1,
) -> Result<RelationalOperation> {
    let decoded: RelationalOperation = decode_record(operation, RELATIONAL_OPERATION_TYPE)?;
    verify_name(
        &operation.operation_name,
        relational_operation_name(&decoded),
    )?;
    Ok(decoded)
}

pub(super) fn encode_relational_result(result: RelationalResult) -> Result<PersistenceResultV1> {
    let operation_name = relational_result_name(&result).into();
    Ok(PersistenceResultV1 {
        operation_name,
        records: vec![binary_record(RELATIONAL_RESULT_TYPE, &result)?],
    })
}

pub(super) fn decode_relational_result(result: &PersistenceResultV1) -> Result<RelationalResult> {
    let decoded: RelationalResult = decode_record(result, RELATIONAL_RESULT_TYPE)?;
    verify_name(&result.operation_name, relational_result_name(&decoded))?;
    Ok(decoded)
}

fn binary_record<T: serde::Serialize>(type_name: &str, value: &T) -> Result<TypedBinaryChunkV1> {
    Ok(TypedBinaryChunkV1 {
        type_name: type_name.into(),
        bytes: bincode::serialize(value).map_err(|error| {
            protocol_error(&format!("serializable persistence payload: {error}"))
        })?,
        metadata_json: None,
        final_chunk: true,
    })
}

fn decode_record<T: serde::de::DeserializeOwned>(
    value: &impl PersistenceMessage,
    expected_type: &str,
) -> Result<T> {
    let records = value.records();
    let [record] = records else {
        return Err(protocol_error(
            "exactly one typed binary persistence record",
        ));
    };
    if record.type_name != expected_type || !record.final_chunk || record.metadata_json.is_some() {
        return Err(protocol_error(
            "one final typed binary persistence record without metadata",
        ));
    }
    bincode::deserialize(&record.bytes)
        .map_err(|error| protocol_error(&format!("valid binary persistence payload: {error}")))
}

trait PersistenceMessage {
    fn records(&self) -> &[TypedBinaryChunkV1];
}
impl PersistenceMessage for SemanticOperationV1 {
    fn records(&self) -> &[TypedBinaryChunkV1] {
        &self.records
    }
}
impl PersistenceMessage for RelationalOperationV1 {
    fn records(&self) -> &[TypedBinaryChunkV1] {
        &self.records
    }
}
impl PersistenceMessage for PersistenceResultV1 {
    fn records(&self) -> &[TypedBinaryChunkV1] {
        &self.records
    }
}

fn start_envelope(
    request_id: String,
    client_instance_id: String,
    operation: Operation,
    control: &InvocationControl,
) -> InvocationEnvelopeV1 {
    InvocationEnvelopeV1 {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        request_id,
        sequence: 0,
        payload: Some(Payload::Start(
            lumvise_resource_routing::protocol::InvocationStartV1 {
                deadline_unix_ms: control.deadline_unix_ms(),
                client_instance_id,
                operation: Some(operation),
            },
        )),
    }
}

fn terminal(
    envelopes: &[InvocationEnvelopeV1],
) -> Result<&lumvise_resource_routing::protocol::InvocationTerminalV1> {
    envelopes
        .last()
        .and_then(|envelope| match &envelope.payload {
            Some(Payload::Terminal(terminal)) => Some(terminal),
            _ => None,
        })
        .ok_or_else(|| protocol_error("one terminal invocation frame"))
}

fn ensure_completed(
    terminal: &lumvise_resource_routing::protocol::InvocationTerminalV1,
) -> Result<()> {
    if terminal.status == InvocationTerminalStatusV1::Completed as i32 {
        Ok(())
    } else {
        Err(protocol_error(
            terminal
                .message
                .as_deref()
                .unwrap_or("completed persistence terminal"),
        ))
    }
}

fn capability_ready(
    client: &dyn ResourceInvocationClient,
    client_instance_id: &str,
    capability: ResourceCapabilityV1,
) -> Result<bool> {
    let control = InvocationControl::sixty_seconds();
    let readiness = client
        .readiness(
            &ReadinessRequestV1 {
                supported_majors: vec![PROTOCOL_MAJOR],
                supported_minors: vec![PROTOCOL_MINOR],
                deadline_unix_ms: control.deadline_unix_ms(),
                client_instance_id: client_instance_id.into(),
                requested_capabilities: vec![capability as i32],
            },
            &control,
        )
        .map_err(transport_error)?;
    Ok(readiness.capabilities.iter().any(|entry| {
        entry.capability == capability as i32 && entry.status == CapabilityReadinessV1::Ready as i32
    }))
}

fn ensure_active(control: &InvocationControl) -> Result<()> {
    if control.is_cancelled() {
        return Err(protocol_error("active, non-cancelled invocation"));
    }
    if control.is_expired() {
        return Err(protocol_error("invocation before its absolute deadline"));
    }
    Ok(())
}

fn verify_name(actual: &str, expected: &str) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(protocol_error(&format!(
            "{expected} persistence operation name"
        )))
    }
}

fn transport_error(error: TransportError) -> DbError {
    protocol_error(&format!("central resource transport: {error}"))
}
fn protocol_error(expected: &str) -> DbError {
    DbError::invalid_value("centralized persistence protocol", expected)
}

fn semantic_operation_name(operation: &SemanticOperation) -> &'static str {
    match operation {
        SemanticOperation::CreatePzSnapshot { .. } => "CreatePzSnapshot",
        SemanticOperation::SyncStructure { .. } => "SyncStructure",
        SemanticOperation::BeginProjectSnapshot { .. } => "BeginProjectSnapshot",
        SemanticOperation::StageProjectSnapshot { .. } => "StageProjectSnapshot",
        SemanticOperation::CommitProjectSnapshot { .. } => "CommitProjectSnapshot",
        SemanticOperation::AbortProjectSnapshot { .. } => "AbortProjectSnapshot",
        SemanticOperation::SyncPartition { .. } => "SyncPartition",
        SemanticOperation::Element { .. } => "Element",
        SemanticOperation::ElementsByIds { .. } => "ElementsByIds",
        SemanticOperation::ElementsByIdsIncludingInactive { .. } => {
            "ElementsByIdsIncludingInactive"
        }
        SemanticOperation::ArtifactsByIds { .. } => "ArtifactsByIds",
        SemanticOperation::SearchElementCandidates { .. } => "SearchElementCandidates",
        SemanticOperation::SearchElementNameVectors { .. } => "SearchElementNameVectors",
        SemanticOperation::StoreElementNameVectors { .. } => "StoreElementNameVectors",
        SemanticOperation::SearchArtifactTextVectors { .. } => "SearchArtifactTextVectors",
        SemanticOperation::StoreArtifactTextVectors { .. } => "StoreArtifactTextVectors",
        SemanticOperation::RelationshipsFrom { .. } => "RelationshipsFrom",
        SemanticOperation::RelationshipsTouchingElements { .. } => "RelationshipsTouchingElements",
        SemanticOperation::Artifact { .. } => "Artifact",
        SemanticOperation::ArtifactBlobGet { .. } => "ArtifactBlobGet",
        SemanticOperation::ArtifactBlobPut { .. } => "ArtifactBlobPut",
        SemanticOperation::ArtifactsForElements { .. } => "ArtifactsForElements",
        SemanticOperation::ArtifactsForElementWithInheritance { .. } => {
            "ArtifactsForElementWithInheritance"
        }
        SemanticOperation::ArtifactDependents { .. } => "ArtifactDependents",
        SemanticOperation::UpsertArtifact { .. } => "UpsertArtifact",
        SemanticOperation::RemoveArtifact { .. } => "RemoveArtifact",
        SemanticOperation::RemoveElement { .. } => "RemoveElement",
        SemanticOperation::ProjectRoots => "ProjectRoots",
        SemanticOperation::ProjectIdentity { .. } => "ProjectIdentity",
        SemanticOperation::ProjectArtifacts { .. } => "ProjectArtifacts",
        SemanticOperation::SemanticRevision => "SemanticRevision",
        SemanticOperation::ScopedRead(_) => "ScopedRead",
        SemanticOperation::ProjectElementCounts { .. } => "ProjectElementCounts",
        SemanticOperation::ImportPzSnapshot { .. } => "ImportPzSnapshot",
        SemanticOperation::ProjectSnapshot { .. } => "ProjectSnapshot",
        SemanticOperation::SelectiveSubgraph { .. } => "SelectiveSubgraph",
        SemanticOperation::ProjectRendererGraph(_) => "ProjectRendererGraph",
        SemanticOperation::ChangesSinceRevision { .. } => "ChangesSinceRevision",
        SemanticOperation::RegisterChangeHook { .. } => "RegisterChangeHook",
        SemanticOperation::DeregisterChangeHook { .. } => "DeregisterChangeHook",
        SemanticOperation::ChangeHookRegistrations => "ChangeHookRegistrations",
        SemanticOperation::ChangeHookDirtyBatch { .. } => "ChangeHookDirtyBatch",
        SemanticOperation::AcknowledgeChangeHook { .. } => "AcknowledgeChangeHook",
        SemanticOperation::WaitForSemanticRevision { .. } => "WaitForSemanticRevision",
        SemanticOperation::Maintenance => "Maintenance",
        SemanticOperation::CandidateSourceElements { .. } => "CandidateSourceElements",
    }
}

fn semantic_result_name(result: &SemanticResult) -> &'static str {
    match result {
        SemanticResult::PzSnapshot(_) => "PzSnapshot",
        SemanticResult::SyncStructure(_) => "SyncStructure",
        SemanticResult::SyncPartition(_) => "SyncPartition",
        SemanticResult::Element(_) => "Element",
        SemanticResult::Elements(_) => "Elements",
        SemanticResult::ElementVectorSearch(_) => "ElementVectorSearch",
        SemanticResult::StoredElementNameVectors { .. } => "StoredElementNameVectors",
        SemanticResult::ArtifactVectorSearch(_) => "ArtifactVectorSearch",
        SemanticResult::StoredArtifactTextVectors { .. } => "StoredArtifactTextVectors",
        SemanticResult::Relationships(_) => "Relationships",
        SemanticResult::Artifact(_) => "Artifact",
        SemanticResult::Artifacts(_) => "Artifacts",
        SemanticResult::UpsertedArtifact { .. } => "UpsertedArtifact",
        SemanticResult::ArtifactBlob(_) => "ArtifactBlob",
        SemanticResult::Removed { .. } => "Removed",
        SemanticResult::ProjectIdentity(_) => "ProjectIdentity",
        SemanticResult::ProjectRoots(_) => "ProjectRoots",
        SemanticResult::SemanticRevision { .. } => "SemanticRevision",
        SemanticResult::ScopedGraph(_) => "ScopedGraph",
        SemanticResult::ProjectElementCounts { .. } => "ProjectElementCounts",
        SemanticResult::PzImport(_) => "PzImport",
        SemanticResult::ProjectSnapshot(_) => "ProjectSnapshot",
        SemanticResult::SelectiveSubgraph(_) => "SelectiveSubgraph",
        SemanticResult::RendererGraphProjection(_) => "RendererGraphProjection",
        SemanticResult::ChangesSinceRevision(_) => "ChangesSinceRevision",
        SemanticResult::ChangeHookRegistration(_) => "ChangeHookRegistration",
        SemanticResult::ChangeHookRegistrations(_) => "ChangeHookRegistrations",
        SemanticResult::ChangeHookBatch(_) => "ChangeHookBatch",
        SemanticResult::ChangeHookDeregistered { .. } => "ChangeHookDeregistered",
        SemanticResult::ChangeHookAcknowledged { .. } => "ChangeHookAcknowledged",
        SemanticResult::MaintenanceCompleted => "MaintenanceCompleted",
        SemanticResult::SnapshotStaging { .. } => "SnapshotStaging",
        SemanticResult::SnapshotCommitted(_) => "SnapshotCommitted",
        SemanticResult::SnapshotAborted { .. } => "SnapshotAborted",
    }
}

fn relational_operation_name(operation: &RelationalOperation) -> &'static str {
    match operation {
        RelationalOperation::GetPersistentSetting { .. } => "GetPersistentSetting",
        RelationalOperation::SetPersistentSetting { .. } => "SetPersistentSetting",
        RelationalOperation::GetPluginSetting { .. } => "GetPluginSetting",
        RelationalOperation::SetPluginSetting { .. } => "SetPluginSetting",
        RelationalOperation::EnsurePluginDataTable { .. } => "EnsurePluginDataTable",
        RelationalOperation::UpdatePluginDataTable { .. } => "UpdatePluginDataTable",
        RelationalOperation::ListPluginDataTables { .. } => "ListPluginDataTables",
        RelationalOperation::PutPluginData { .. } => "PutPluginData",
        RelationalOperation::GetPluginData { .. } => "GetPluginData",
        RelationalOperation::DeletePluginData { .. } => "DeletePluginData",
        RelationalOperation::ListPluginData { .. } => "ListPluginData",
        RelationalOperation::PagePluginData { .. } => "PagePluginData",
        RelationalOperation::TrimPluginData { .. } => "TrimPluginData",
        RelationalOperation::ApplyMutations { .. } => "ApplyMutations",
        RelationalOperation::EnqueueChangeHookBatch { .. } => "EnqueueChangeHookBatch",
        RelationalOperation::SyncBackgroundRegistrations { .. } => "SyncBackgroundRegistrations",
        RelationalOperation::ListBackgroundRegistrations => "ListBackgroundRegistrations",
        RelationalOperation::EnqueueRecurring { .. } => "EnqueueRecurring",
        RelationalOperation::DueDeliveriesByKind { .. } => "DueDeliveriesByKind",
        RelationalOperation::ClaimDelivery { .. } => "ClaimDelivery",
        RelationalOperation::CompleteDelivery { .. } => "CompleteDelivery",
        RelationalOperation::RetryDelivery { .. } => "RetryDelivery",
        RelationalOperation::DeadLetterDelivery { .. } => "DeadLetterDelivery",
        RelationalOperation::PruneDeadLetters { .. } => "PruneDeadLetters",
        RelationalOperation::DeliveryDiagnostics { .. } => "DeliveryDiagnostics",
        RelationalOperation::RegisterMcpInstance { .. } => "RegisterMcpInstance",
        RelationalOperation::HeartbeatMcpInstance { .. } => "HeartbeatMcpInstance",
        RelationalOperation::ActiveMcpInstances => "ActiveMcpInstances",
        RelationalOperation::AllMcpInstances => "AllMcpInstances",
        RelationalOperation::DeregisterMcpInstance { .. } => "DeregisterMcpInstance",
    }
}

fn relational_result_name(result: &RelationalResult) -> &'static str {
    match result {
        RelationalResult::PersistentSetting(_) => "PersistentSetting",
        RelationalResult::PersistentSettingUpdated(_) => "PersistentSettingUpdated",
        RelationalResult::PluginSetting(_) => "PluginSetting",
        RelationalResult::PluginSettingUpdated(_) => "PluginSettingUpdated",
        RelationalResult::PluginDataTable(_) => "PluginDataTable",
        RelationalResult::PluginDataTables(_) => "PluginDataTables",
        RelationalResult::PluginDataRow(_) => "PluginDataRow",
        RelationalResult::PluginDataRows(_) => "PluginDataRows",
        RelationalResult::PluginDataPage(_) => "PluginDataPage",
        RelationalResult::PluginDataTrimmed { .. } => "PluginDataTrimmed",
        RelationalResult::PluginDataMutations(_) => "PluginDataMutations",
        RelationalResult::ChangeHookBatchEnqueued { .. } => "ChangeHookBatchEnqueued",
        RelationalResult::BackgroundRegistrations(_) => "BackgroundRegistrations",
        RelationalResult::BackgroundDelivery(_) => "BackgroundDelivery",
        RelationalResult::BackgroundDeliveries(_) => "BackgroundDeliveries",
        RelationalResult::DeliveryTransition { .. } => "DeliveryTransition",
        RelationalResult::DeadLettersPruned { .. } => "DeadLettersPruned",
        RelationalResult::McpInstance(_) => "McpInstance",
        RelationalResult::McpInstances(_) => "McpInstances",
        RelationalResult::McpInstanceDeregistered { .. } => "McpInstanceDeregistered",
    }
}
