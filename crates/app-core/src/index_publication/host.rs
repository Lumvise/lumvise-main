//! Compiled producers call project.index; checkpoint storage and source rules stay in this owner.
use super::*;
use lumvise_contracts::{IndexBatchRequest, SourceIndexPreparation, SourceIndexPublication};
use lumvise_db_core::{RelationalPersistence, SemanticRevisionPublicationReceipt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum SourceIndexRequest {
    Prepare {
        folder_path: String,
        project_root: String,
        provider_instance_id: String,
    },
    Freeze {
        folder_path: String,
        base_token: String,
        full_snapshot: bool,
        batch: IndexBatchRequest,
    },
    Acknowledge {
        folder_path: String,
        project_root: String,
        provider_instance_id: String,
        receipt: SemanticRevisionPublicationReceipt,
    },
}

pub(crate) fn invoke_index_publication(
    semantic: Arc<dyn SemanticPersistence>,
    relational: Arc<dyn RelationalPersistence>,
    client: Option<&str>,
    input: Value,
    control: &InvocationControl,
) -> Result<Value> {
    let request: SourceIndexRequest = serde_json::from_value(input.clone()).map_err(|error| {
        invalid_checkpoint(
            format!("{}: {error}", input["operation"]),
            "project.index prepare/freeze/acknowledge input",
        )
    })?;
    let client = client.ok_or_else(|| {
        invalid_checkpoint(
            "unbound",
            "the persisted startup client identity before source publication",
        )
    })?;
    let (folder, project, provider) = request.route();
    if lumvise_contracts::is_knowledge_space_root(project) {
        return Err(invalid_checkpoint(
            project,
            "source project root outside reserved document spaces",
        ));
    }
    if !Path::new(folder).is_dir() {
        return Err(invalid_checkpoint(folder, "an existing source directory"));
    }
    let checkout = IndexSourceCheckout::open(
        semantic,
        relational,
        client,
        Path::new(folder),
        project,
        provider,
        control,
    )?;
    execute_source_index(&checkout, request, control)
}

impl SourceIndexRequest {
    fn route(&self) -> (&str, &str, &str) {
        match self {
            Self::Prepare {
                folder_path,
                project_root,
                provider_instance_id,
            }
            | Self::Acknowledge {
                folder_path,
                project_root,
                provider_instance_id,
                ..
            } => (folder_path, project_root, provider_instance_id),
            Self::Freeze {
                folder_path, batch, ..
            } => (
                folder_path,
                &batch.project_root,
                &batch.provider_instance_id,
            ),
        }
    }
}

fn execute_source_index(
    checkout: &IndexSourceCheckout,
    request: SourceIndexRequest,
    control: &InvocationControl,
) -> Result<Value> {
    match request {
        SourceIndexRequest::Prepare { .. } => {
            serde_json::to_value(prepare_source_index(checkout, control)?)
                .map_err(|error| lumvise_db_core::DbError::from(error).into())
        }
        SourceIndexRequest::Freeze {
            batch,
            base_token,
            full_snapshot,
            ..
        } => {
            let before = checkout.before_scan(control)?;
            if before.token != base_token {
                return Err(invalid_checkpoint(
                    base_token,
                    "the unchanged checkpoint captured before parsing",
                ));
            }
            let pending = checkout.freeze_batch(batch, full_snapshot, before, control)?;
            serde_json::to_value(pending.map(producer_input))
                .map_err(|error| lumvise_db_core::DbError::from(error).into())
        }
        SourceIndexRequest::Acknowledge { receipt, .. } => {
            checkout.acknowledge(&receipt, control)?;
            Ok(json!({"acknowledged": true, "operation_id": receipt.operation_id}))
        }
    }
}

fn prepare_source_index(
    checkout: &IndexSourceCheckout,
    control: &InvocationControl,
) -> Result<SourceIndexPreparation> {
    let source = checkout.source().clone();
    if let Some(pending) = checkout.pending(control)? {
        return Ok(SourceIndexPreparation::Pending {
            source,
            publication: producer_input(pending),
        });
    }
    let before = checkout.before_scan(control)?;
    Ok(SourceIndexPreparation::Scan {
        source,
        base_token: before.token,
        requires_full_scan: before.requires_full_scan,
    })
}

fn producer_input(publication: FrozenIndexPublication) -> SourceIndexPublication {
    SourceIndexPublication {
        operation_id: publication.operation_id,
        batch: publication.batch,
    }
}
