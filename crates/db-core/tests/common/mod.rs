//! Named in-process transport fake exercises the public resource codec.
#[allow(dead_code)] // Some suites exercise only the shared transport fake.
mod revision_client;
use lumvise_db_core::{
    CentralizedPersistence, LocalPersistence, RelationalPersistence, SemanticPersistence,
};
use lumvise_resource_routing::{
    InvocationControl, ResourceInvocationClient, TransportError,
    protocol::{
        InvocationEnvelopeV1, InvocationTerminalStatusV1, InvocationTerminalV1, ReadinessRequestV1,
        ReadinessResponseV1, invocation_envelope_v1::Payload, invocation_start_v1::Operation,
        invocation_terminal_v1::Result as TerminalResult,
    },
};
#[allow(unused_imports)]
pub use revision_client::RevisionTestClient;
use std::sync::Arc;

#[allow(dead_code)] // Some suites share only the revision client fixture.
pub struct LocalPublicationServer(pub Arc<LocalPersistence>);

impl ResourceInvocationClient for LocalPublicationServer {
    fn readiness(
        &self,
        _: &ReadinessRequestV1,
        _: &InvocationControl,
    ) -> Result<ReadinessResponseV1, TransportError> {
        unreachable!("execution-only fake")
    }

    fn invoke(
        &self,
        requests: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> Result<Vec<InvocationEnvelopeV1>, TransportError> {
        let Some(Payload::Start(start)) = &requests[0].payload else {
            panic!("expected start")
        };
        let result = match &start.operation {
            Some(Operation::SemanticOperation(encoded)) => {
                let operation = CentralizedPersistence::decode_semantic_operation(encoded).unwrap();
                SemanticPersistence::execute(&*self.0, operation, control).map(|result| {
                    TerminalResult::Semantic(
                        CentralizedPersistence::encode_semantic_result(result).unwrap(),
                    )
                })
            }
            Some(Operation::RelationalOperation(encoded)) => {
                let operation =
                    CentralizedPersistence::decode_relational_operation(encoded).unwrap();
                RelationalPersistence::execute(&*self.0, operation, control).map(|result| {
                    TerminalResult::Relational(
                        CentralizedPersistence::encode_relational_result(result).unwrap(),
                    )
                })
            }
            other => panic!("expected semantic/relational operation, got {other:?}"),
        };
        let terminal = match result {
            Ok(result) => InvocationTerminalV1 {
                status: InvocationTerminalStatusV1::Completed as i32,
                result: Some(result),
                ..Default::default()
            },
            Err(error) => InvocationTerminalV1 {
                status: InvocationTerminalStatusV1::Failed as i32,
                message: Some(error.to_string()),
                ..Default::default()
            },
        };
        Ok(vec![InvocationEnvelopeV1 {
            payload: Some(Payload::Terminal(terminal)),
            ..requests[0].clone()
        }])
    }

    fn cancel(
        &self,
        _: &InvocationEnvelopeV1,
        _: &InvocationControl,
    ) -> Result<InvocationTerminalV1, TransportError> {
        unreachable!("execution-only fake")
    }
}
