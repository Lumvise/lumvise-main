//! Portable semantic calls over the production authenticated TLS/H2 client.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use lumvise_db_core::{
    CentralizedPersistence, SemanticArtifactRevisionWrite, SemanticElement, SemanticOperation,
    SemanticPartition, SemanticPersistence, SemanticRecordRef, SemanticResult,
    SemanticRevisionPublication, SemanticRevisionPublicationOutcome,
    SemanticRevisionPublicationReceipt, SemanticRevisionResolution, SemanticRevisionSelectionQuery,
    SemanticRevisionSelectionSnapshot, SemanticRevisionStructure,
    SemanticRevisionStructureCoverage, SemanticSelectionExpectation,
};
use lumvise_resource_routing::{
    AuthenticatedFramedClient, BlockingAuthenticatedFramedClient, Http2CentralTransport,
    InvocationControl, ResourceInvocationClient, TransportError,
    protocol::{
        InvocationEnvelopeV1, InvocationTerminalV1, ReadinessRequestV1, ReadinessResponseV1,
        SemanticOperationV1,
    },
};
use prost::Message;
use serde_json::json;

pub(super) const ROOT: &str = "/shared-revision-project";

pub(super) fn client(instance: &str, drop_reply: bool) -> CentralizedPersistence {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let endpoint = std::env::var("LUMVISE_REVISION_TEST_ENDPOINT")
        .unwrap()
        .parse()
        .unwrap();
    let transport = Http2CentralTransport::from_endpoint(
        endpoint,
        Some(&fixture_root().join("certificate.pem")),
    )
    .unwrap();
    let framed = AuthenticatedFramedClient::new(Arc::new(transport), instance);
    let network: Arc<dyn ResourceInvocationClient> =
        Arc::new(BlockingAuthenticatedFramedClient::new(framed).unwrap());
    let network = Arc::new(DroppedAcknowledgement {
        network,
        drop_reply: AtomicBool::new(drop_reply),
    });
    CentralizedPersistence::new(network, instance)
}

struct DroppedAcknowledgement {
    network: Arc<dyn ResourceInvocationClient>,
    drop_reply: AtomicBool,
}

impl ResourceInvocationClient for DroppedAcknowledgement {
    fn readiness(
        &self,
        request: &ReadinessRequestV1,
        control: &InvocationControl,
    ) -> Result<ReadinessResponseV1, TransportError> {
        self.network.readiness(request, control)
    }

    fn invoke(
        &self,
        request: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> Result<Vec<InvocationEnvelopeV1>, TransportError> {
        let response = self.network.invoke(request, control)?;
        if self.drop_reply.swap(false, Ordering::SeqCst) {
            return Err(TransportError::EmptyResponse);
        }
        Ok(response)
    }

    fn cancel(
        &self,
        request: &InvocationEnvelopeV1,
        control: &InvocationControl,
    ) -> Result<InvocationTerminalV1, TransportError> {
        self.network.cancel(request, control)
    }
}

pub(super) fn execute(
    client: &CentralizedPersistence,
    operation: SemanticOperation,
) -> SemanticResult {
    SemanticPersistence::execute(client, operation, &InvocationControl::sixty_seconds()).unwrap()
}

pub(super) fn accepted(
    client: &CentralizedPersistence,
    request: SemanticRevisionPublication,
) -> SemanticRevisionPublicationReceipt {
    match execute(client, SemanticOperation::PublishRevisions(request)) {
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) => receipt,
        other => panic!("result `{other:?}`; expected accepted revision publication"),
    }
}

pub(super) fn selected(client: &CentralizedPersistence) -> SemanticRevisionSelectionSnapshot {
    match execute(
        client,
        SemanticOperation::RevisionSelection {
            project_root: ROOT.into(),
            selection_id: "working".into(),
        },
    ) {
        SemanticResult::RevisionSelection(Some(snapshot)) => snapshot,
        other => panic!("result `{other:?}`; expected working selection"),
    }
}

pub(super) fn selected_element(client: &CentralizedPersistence, path: &str) -> SemanticElement {
    selected(client)
        .elements
        .into_iter()
        .find(|element| element.path == path)
        .unwrap_or_else(|| panic!("path `{path}`; expected one selected source element"))
}

pub(super) fn partition(path: &str) -> SemanticPartition {
    SemanticPartition {
        project_root: ROOT.into(),
        replace_paths: vec![path.into()],
        provider_instance_id: None,
    }
}

fn expectation(
    client: &CentralizedPersistence,
    structure_coverage: SemanticRevisionStructureCoverage,
    artifact_ids: Vec<String>,
) -> SemanticSelectionExpectation {
    let query = SemanticRevisionSelectionQuery {
        project_root: ROOT.into(),
        selection_id: "working".into(),
        structure_coverage,
        artifact_ids,
    };
    match execute(client, SemanticOperation::RevisionSelectionMembers(query)) {
        SemanticResult::RevisionObservation(observed) => {
            observed.map(|value| value.expected).unwrap_or_default()
        }
        other => panic!("result `{other:?}`; expected scoped membership"),
    }
}

pub(super) fn publication(
    client: &CentralizedPersistence,
    operation: &str,
    coverage: SemanticRevisionStructureCoverage,
    artifacts: Vec<String>,
) -> SemanticRevisionPublication {
    let SemanticResult::ProjectIdentity(identity) = execute(
        client,
        SemanticOperation::ProjectIdentity {
            project_root: ROOT.into(),
        },
    ) else {
        panic!("expected stable project lineage")
    };
    let expected = expectation(client, coverage, artifacts);
    SemanticRevisionPublication {
        operation_id: operation.into(),
        project_root: ROOT.into(),
        project_id: identity.project_id,
        selection_id: "working".into(),
        source_context: "local".into(),
        producer_base: None,
        expected,
        structure: None,
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    }
}

pub(super) fn source_edit(
    client: &CentralizedPersistence,
    path: &str,
    revision: u64,
) -> SemanticRevisionPublication {
    let mut element = selected_element(client, path);
    element.metadata["revision"] = json!(revision);
    let partition = partition(path);
    let mut request = publication(
        client,
        &format!("source-{path}-{revision}"),
        SemanticRevisionStructureCoverage::Partition(partition.clone()),
        vec![],
    );
    request.structure = Some(SemanticRevisionStructure {
        partition: Some(partition),
        elements: vec![element],
        relationships: vec![],
    });
    request
}

pub(super) fn source_element(id: &str, path: &str) -> SemanticElement {
    serde_json::from_value(json!({
        "project_root":ROOT, "semantic_element_id":id, "semantic_source_id":"logical-source",
        "path":path, "element_kind":"file", "name":id, "parent_element_id":null,
        "content_fingerprint":null, "start_line":null, "end_line":null,
        "lifecycle":"active", "match_evidence":null, "metadata":{"revision":1}, "revision_dependencies":[]
    })).unwrap()
}

pub(super) fn artifact(id: &str, owner: &str, body: &str) -> SemanticArtifactRevisionWrite {
    SemanticArtifactRevisionWrite {
        artifact: serde_json::from_value(json!({
            "artifact_id":id, "semantic_element_id":owner, "artifact_kind":"annotation", "title":"Working note",
            "content":body, "dependencies":[], "metadata":{}
        })).unwrap(), media_type: "text/plain".into(), checkpoint: false,
    }
}

pub(super) fn save_artifact(
    client: &CentralizedPersistence,
    id: &str,
    owner: &str,
    body: &str,
    operation: &str,
) -> String {
    let mut request = publication(
        client,
        operation,
        SemanticRevisionStructureCoverage::None,
        vec![id.into()],
    );
    request.artifacts.push(artifact(id, owner, body));
    accepted(client, request)
        .remaps
        .into_iter()
        .find(|remap| remap.incoming == SemanticRecordRef::artifact(id))
        .expect("accepted artifact remap")
        .accepted
        .record_id
}

pub(super) fn resolution(client: &CentralizedPersistence, id: &str) -> SemanticRevisionResolution {
    match execute(
        client,
        SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record: SemanticRecordRef::artifact(id),
        },
    ) {
        SemanticResult::RevisionResolution(Some(resolution)) => resolution,
        other => panic!("result `{other:?}`; expected exact artifact `{id}`"),
    }
}

pub(super) fn fixture_root() -> PathBuf {
    PathBuf::from(std::env::var_os("LUMVISE_REVISION_TEST_ROOT").unwrap())
}

pub(super) fn save_pending(request: SemanticRevisionPublication) {
    // shortcut: this frozen Protobuf fixture is not App Core's producer journal; verify that owner before rollout.
    let encoded = CentralizedPersistence::encode_semantic_operation(
        SemanticOperation::PublishRevisions(request),
    )
    .unwrap();
    let mut file = std::fs::File::create(fixture_root().join("pending-a.pb")).unwrap();
    std::io::Write::write_all(&mut file, &encoded.encode_to_vec()).unwrap();
    file.sync_all().unwrap();
}

pub(super) fn pending() -> SemanticRevisionPublication {
    let bytes = std::fs::read(fixture_root().join("pending-a.pb")).unwrap();
    let encoded = SemanticOperationV1::decode(bytes.as_slice()).unwrap();
    let SemanticOperation::PublishRevisions(request) =
        CentralizedPersistence::decode_semantic_operation(&encoded).unwrap()
    else {
        panic!("expected frozen publication fixture")
    };
    request
}
