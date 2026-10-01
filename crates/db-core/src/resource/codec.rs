use lumvise_resource_routing::protocol::{
    PersistenceResultV1, RelationalOperationV1, SemanticOperationV1,
};

use crate::Result;
use crate::interface::{RelationalOperation, RelationalResult, SemanticOperation, SemanticResult};

pub(super) fn encode_semantic_operation(
    operation: SemanticOperation,
) -> Result<SemanticOperationV1> {
    super::persistence::encode_semantic_operation(operation)
}

pub(super) fn decode_semantic_operation(
    operation: &SemanticOperationV1,
) -> Result<SemanticOperation> {
    super::persistence::decode_semantic_operation(operation)
}

pub(super) fn encode_semantic_result(result: SemanticResult) -> Result<PersistenceResultV1> {
    super::persistence::encode_semantic_result(result)
}

pub(super) fn decode_semantic_result(result: &PersistenceResultV1) -> Result<SemanticResult> {
    super::persistence::decode_semantic_result(result)
}

pub(super) fn encode_relational_operation(
    operation: RelationalOperation,
) -> Result<RelationalOperationV1> {
    super::persistence::encode_relational_operation(operation)
}

pub(super) fn decode_relational_operation(
    operation: &RelationalOperationV1,
) -> Result<RelationalOperation> {
    super::persistence::decode_relational_operation(operation)
}

pub(super) fn encode_relational_result(result: RelationalResult) -> Result<PersistenceResultV1> {
    super::persistence::encode_relational_result(result)
}

pub(super) fn decode_relational_result(result: &PersistenceResultV1) -> Result<RelationalResult> {
    super::persistence::decode_relational_result(result)
}
