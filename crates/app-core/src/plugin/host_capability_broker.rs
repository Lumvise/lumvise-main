//! App Core's public boundary for compiled-plugin Host Capabilities.
//!
//! Plugin Runtime may call [`AppCoreHostCapabilityBroker`]. Storage layout,
//! App Core ownership, and plugin namespace enforcement remain internal.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lumvise_db_core::{
    ArtifactTextVectorizer, PluginDataMutation, RelationalOperation, RelationalPersistence,
    RelationalResult, SemanticPersistence,
};
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, PluginInvocationContext,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Map, Value, json};

use super::PluginHostServices;
use super::host_capability_catalog::{
    HostCapabilityRoute, NEURAL_EMBED_CAPABILITY, PLUGIN_STORAGE_CAPABILITY,
    SEMANTIC_SNAPSHOT_CAPABILITY, host_capability_definition,
};
use super::plugin_invoke_authorization::authorize_plugin_invoke;
use super::semantic_snapshot_writes::SemanticSnapshotWrites;
use super::semantic_storage_capability;

const MAX_EMBED_TEXTS: usize = 128;
const MAX_EMBED_TEXT_BYTES: usize = 32 * 1024;
const MAX_EMBED_TOTAL_BYTES: usize = 256 * 1024;
const MAX_EMBED_DIMENSIONS: usize = 4_096;
const MAX_STAGED_CHANGES: usize = 500;
const MAX_STAGED_BYTES: usize = 1024 * 1024;
const MAX_ACTIVE_STORAGE_TRANSACTIONS: usize = 8;
const MAX_GLOBAL_STORAGE_TRANSACTIONS: usize = 32;
/// Total deadline from begin; staging does not extend transaction lifetime.
const STORAGE_TRANSACTION_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_GLOBAL_STAGED_BYTES: usize = 256 * 1024 * 1024;
const MAX_PLUGIN_DATA_MUTATIONS: usize = 250_000;
const MAX_PLUGIN_DATA_MUTATION_BYTES: usize = 128 * 1024 * 1024;

pub(crate) type SharedPluginVectorizer =
    Arc<Mutex<Option<Arc<dyn ArtifactTextVectorizer + Send + Sync>>>>;

/// Identity of the currently installed `neural.embed` vectorizer, tracked
/// alongside the vectorizer instance so background vector maintenance can
/// discover which project vectors are stale without an extra embed call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VectorEngineIdentity {
    pub(crate) engine_id: String,
    pub(crate) model: Option<String>,
}

/// App Core adapter for permission-gated compiled-plugin storage calls.
///
/// The adapter receives only the storage dependency it needs. It cannot call
/// persistence outside the capability routes authorized by App Core.
pub struct AppCoreHostCapabilityBroker {
    semantic: Arc<dyn SemanticPersistence>,
    relational: Arc<dyn RelationalPersistence>,
    vectorizer: SharedPluginVectorizer,
    storage_transactions: Mutex<HashMap<String, StagedStorageTransaction>>,
    semantic_snapshot_writes: SemanticSnapshotWrites,
    semantic_snapshots: Arc<crate::SemanticSnapshotService>,
    services: Option<Arc<PluginHostServices>>,
}

struct StagedStorageTransaction {
    plugin_id: String,
    created_at: Instant,
    mutations: Vec<PluginDataMutation>,
    serialized_bytes: usize,
}

impl AppCoreHostCapabilityBroker {
    /// Creates a broker using the application's selected persistence resources.
    pub fn new(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
    ) -> Self {
        let semantic_snapshots =
            Arc::new(crate::SemanticSnapshotService::new(Arc::clone(&semantic)));
        Self::with_services(
            semantic,
            relational,
            Arc::new(Mutex::new(None)),
            None,
            semantic_snapshots,
        )
    }

    pub(crate) fn with_services(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        vectorizer: SharedPluginVectorizer,
        services: Option<Arc<PluginHostServices>>,
        semantic_snapshots: Arc<crate::SemanticSnapshotService>,
    ) -> Self {
        Self {
            semantic,
            relational,
            vectorizer,
            storage_transactions: Mutex::new(HashMap::new()),
            semantic_snapshots,
            semantic_snapshot_writes: SemanticSnapshotWrites::new(),
            services,
        }
    }
    pub(crate) fn semantic_snapshot_service(&self) -> Arc<crate::SemanticSnapshotService> {
        Arc::clone(&self.semantic_snapshots)
    }

    #[cfg(test)]
    pub(crate) fn shares_semantic_snapshot_service(
        &self,
        service: &Arc<crate::SemanticSnapshotService>,
    ) -> bool {
        Arc::ptr_eq(&self.semantic_snapshots, service)
    }
}

impl AppCoreHostCapabilityBroker {
    fn invoke_semantic_snapshot(&self, input: Value) -> Result<Value, HostCapabilityError> {
        let operation = input
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("create");
        let snapshot_operation = match operation {
            "create" => self.create_semantic_snapshot_for_input(&input)?,
            "status" => {
                let operation_id = required_snapshot_operation_id(&input)?;
                self.semantic_snapshots
                    .status(operation_id)
                    .map_err(|error| snapshot_error(error.to_string()))?
            }
            "cancel" => {
                let operation_id = required_snapshot_operation_id(&input)?;
                self.semantic_snapshots
                    .cancel(operation_id)
                    .map_err(|error| snapshot_error(error.to_string()))?
            }
            other => {
                return Err(snapshot_error(format!(
                    "unknown semantic snapshot operation `{other}`"
                )));
            }
        };
        serde_json::to_value(snapshot_operation).map_err(|error| snapshot_error(error.to_string()))
    }

    fn create_semantic_snapshot_for_input(
        &self,
        input: &Value,
    ) -> Result<crate::SemanticSnapshotOperation, HostCapabilityError> {
        let fields = input
            .as_object()
            .ok_or_else(|| invalid_input(input, "semantic snapshot object"))?;
        let operation = match optional_string(fields, "project_root")? {
            Some(root) => self.semantic_snapshots.create_for_project(root),
            None => self.semantic_snapshots.create(),
        };
        operation.map_err(|error| snapshot_error(error.to_string()))
    }

    fn invoke_semantic_snapshot_controlled(
        &self,
        input: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        require_active_host_call(SEMANTIC_SNAPSHOT_CAPABILITY, context)?;
        self.invoke_semantic_snapshot(input)
    }
}

impl HostCapabilityBroker for AppCoreHostCapabilityBroker {
    fn invoke(&self, request: HostCapabilityRequest) -> Result<Value, HostCapabilityError> {
        let capability_id = request.capability_id.as_str();
        let definition = host_capability_definition(capability_id)
            .ok_or_else(|| unknown_capability(capability_id))?;
        match definition.route {
            HostCapabilityRoute::ProjectSource => crate::app::invoke_project_source(
                self.semantic.as_ref(),
                request.input,
                &InvocationControl::sixty_seconds(),
            )
            .map_err(|message| {
                HostCapabilityError::new("project.source", "source_access_failed", message, false)
            }),
            HostCapabilityRoute::PluginStorage => {
                self.invoke_plugin_storage(&request.plugin_id, request.input)
            }
            HostCapabilityRoute::SemanticStorage => semantic_storage_capability::invoke(
                self.semantic.as_ref(),
                &self.semantic_snapshot_writes,
                &request.plugin_id,
                request.input,
            ),
            HostCapabilityRoute::SemanticSnapshot => self.invoke_semantic_snapshot(request.input),
            HostCapabilityRoute::NeuralEmbed => {
                invoke_neural_embed(&self.vectorizer, request.input)
            }
            HostCapabilityRoute::PluginInvokeAuthorization => {
                authorize_plugin_invoke(request.input)
            }
            HostCapabilityRoute::Services => self.services.as_ref().map_or_else(
                || Err(unknown_capability(capability_id)),
                |services| services.invoke(&request.plugin_id, capability_id, request.input),
            ),
        }
    }

    fn invoke_controlled(
        &self,
        request: HostCapabilityRequest,
        context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        require_active_host_call(&request.capability_id, context)?;
        let capability_id = request.capability_id.clone();
        let definition = host_capability_definition(&capability_id)
            .ok_or_else(|| unknown_capability(&capability_id))?;
        let output = match definition.route {
            HostCapabilityRoute::ProjectSource => {
                let bridge = super::plugin_host_capabilities::invocation_control(context);
                crate::app::invoke_project_source(
                    self.semantic.as_ref(),
                    request.input,
                    &bridge.control(),
                )
                .map_err(|message| {
                    HostCapabilityError::new(
                        "project.source",
                        "source_access_failed",
                        message,
                        false,
                    )
                })?
            }
            HostCapabilityRoute::SemanticStorage => semantic_storage_capability::invoke_controlled(
                self.semantic.as_ref(),
                &self.semantic_snapshot_writes,
                &request.plugin_id,
                request.input,
                context,
            )?,
            HostCapabilityRoute::SemanticSnapshot => {
                self.invoke_semantic_snapshot_controlled(request.input, context)?
            }
            HostCapabilityRoute::Services => self.services.as_ref().map_or_else(
                || Err(unknown_capability(&capability_id)),
                |services| {
                    services.invoke_controlled(
                        &request.plugin_id,
                        &capability_id,
                        request.input,
                        context,
                    )
                },
            )?,
            _ => self.invoke(request)?,
        };
        require_active_host_call(&capability_id, context)?;
        Ok(output)
    }
}

fn require_active_host_call(
    capability_id: &str,
    context: &PluginInvocationContext,
) -> Result<(), HostCapabilityError> {
    if context.cancellation().is_cancelled() {
        return Err(HostCapabilityError::new(
            capability_id,
            "host_capability_cancelled",
            "parent Plugin Invocation was cancelled",
            false,
        ));
    }
    if Instant::now() >= context.deadline() {
        return Err(HostCapabilityError::new(
            capability_id,
            "host_capability_deadline_exceeded",
            "parent Plugin Invocation deadline elapsed",
            true,
        ));
    }
    Ok(())
}

impl AppCoreHostCapabilityBroker {
    fn invoke_plugin_storage(
        &self,
        plugin_id: &str,
        input: Value,
    ) -> Result<Value, HostCapabilityError> {
        let fields = input.as_object().ok_or_else(|| {
            invalid_input(
                &input,
                "object with operation and operation-specific fields",
            )
        })?;
        let operation = required_string(fields, "operation")?;
        match operation {
            "begin_mutations" => self.begin_mutations(plugin_id, fields),
            "stage_mutations" => self.stage_mutations(plugin_id, fields),
            "commit_mutations" => self.commit_mutations(plugin_id, fields),
            "abort_mutations" => self.abort_mutations(plugin_id, fields),
            "mutate_rows" => mutate_rows(self.relational.as_ref(), plugin_id, fields),
            _ => invoke_plugin_storage_row(self.relational.as_ref(), plugin_id, operation, fields),
        }
    }

    fn begin_mutations(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        require_fields(fields, &["operation"])?;
        let mut transactions = self.lock_storage_transactions()?;
        discard_expired_transactions(&mut transactions);
        if transactions.len() >= MAX_GLOBAL_STORAGE_TRANSACTIONS {
            return Err(storage_limit(
                "global active transaction count",
                transactions.len() + 1,
                MAX_GLOBAL_STORAGE_TRANSACTIONS,
            ));
        }
        let active = transactions
            .values()
            .filter(|transaction| transaction.plugin_id == plugin_id)
            .count();
        if active >= MAX_ACTIVE_STORAGE_TRANSACTIONS {
            return Err(storage_limit(
                "active transaction count",
                active + 1,
                MAX_ACTIVE_STORAGE_TRANSACTIONS,
            ));
        }
        let transaction_id = next_storage_transaction_id()?;
        transactions.insert(
            transaction_id.clone(),
            StagedStorageTransaction {
                plugin_id: plugin_id.into(),
                created_at: Instant::now(),
                mutations: Vec::new(),
                serialized_bytes: 0,
            },
        );
        Ok(json!({"transaction_id": transaction_id}))
    }

    fn stage_mutations(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        require_fields(fields, &["operation", "transaction_id", "mutations"])?;
        let transaction_id = required_string(fields, "transaction_id")?;
        let mutations = parse_mutations(fields, MAX_STAGED_CHANGES, MAX_STAGED_BYTES)?;
        let chunk_bytes = serde_json::to_vec(&mutations)
            .map_err(|error| invalid_input(&json!(error.to_string()), "serializable mutations"))?
            .len();
        let mut transactions = self.lock_storage_transactions()?;
        discard_expired_transactions(&mut transactions);
        require_owned_transaction(&transactions, transaction_id, plugin_id)?;
        let plugin_count = transactions
            .values()
            .filter(|transaction| transaction.plugin_id == plugin_id)
            .map(|transaction| transaction.mutations.len())
            .sum::<usize>()
            + mutations.len();
        let plugin_bytes = transactions
            .values()
            .filter(|transaction| transaction.plugin_id == plugin_id)
            .map(|transaction| transaction.serialized_bytes)
            .sum::<usize>()
            + chunk_bytes;
        let global_bytes = transactions
            .values()
            .map(|transaction| transaction.serialized_bytes)
            .sum::<usize>()
            + chunk_bytes;
        let exceeded = quota_exceeded(plugin_count, plugin_bytes, global_bytes);
        if let Some((subject, actual, maximum)) = exceeded {
            transactions.remove(transaction_id);
            return Err(storage_limit(subject, actual, maximum));
        }
        let transaction = owned_transaction_mut(&mut transactions, transaction_id, plugin_id)?;
        transaction.mutations.extend(mutations);
        transaction.serialized_bytes += chunk_bytes;
        Ok(json!({"staged_mutations": transaction.mutations.len()}))
    }

    fn commit_mutations(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        require_fields(fields, &["operation", "transaction_id"])?;
        let transaction_id = required_string(fields, "transaction_id")?;
        let transaction = {
            let mut transactions = self.lock_storage_transactions()?;
            discard_expired_transactions(&mut transactions);
            require_owned_transaction(&transactions, transaction_id, plugin_id)?;
            transactions
                .remove(transaction_id)
                .ok_or_else(|| unknown_transaction(transaction_id))?
        };
        mutation_result_value(relational_execute(
            self.relational.as_ref(),
            RelationalOperation::ApplyMutations {
                plugin_id: plugin_id.to_owned(),
                mutations: transaction.mutations,
            },
        )?)
    }

    fn abort_mutations(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        require_fields(fields, &["operation", "transaction_id"])?;
        let transaction_id = required_string(fields, "transaction_id")?;
        let mut transactions = self.lock_storage_transactions()?;
        discard_expired_transactions(&mut transactions);
        require_owned_transaction(&transactions, transaction_id, plugin_id)?;
        transactions.remove(transaction_id);
        Ok(json!({"aborted": true}))
    }

    fn lock_storage_transactions(
        &self,
    ) -> Result<
        std::sync::MutexGuard<'_, HashMap<String, StagedStorageTransaction>>,
        HostCapabilityError,
    > {
        self.storage_transactions
            .lock()
            .map_err(|_| storage_execution_failure("plugin storage transaction mutex poisoned"))
    }
}

fn quota_exceeded(
    plugin_count: usize,
    plugin_bytes: usize,
    global_bytes: usize,
) -> Option<(&'static str, usize, usize)> {
    if plugin_count > MAX_PLUGIN_DATA_MUTATIONS {
        return Some((
            "per-plugin staged mutation count",
            plugin_count,
            MAX_PLUGIN_DATA_MUTATIONS,
        ));
    }
    if plugin_bytes > MAX_PLUGIN_DATA_MUTATION_BYTES {
        return Some((
            "per-plugin staged bytes",
            plugin_bytes,
            MAX_PLUGIN_DATA_MUTATION_BYTES,
        ));
    }
    (global_bytes > MAX_GLOBAL_STAGED_BYTES).then_some((
        "global staged bytes",
        global_bytes,
        MAX_GLOBAL_STAGED_BYTES,
    ))
}

fn unknown_capability(capability_id: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability_id,
        "unknown_host_capability",
        format!("unknown Host Capability `{capability_id}`; expected a host-catalog capability"),
        false,
    )
}

fn snapshot_error(message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new(
        SEMANTIC_SNAPSHOT_CAPABILITY,
        "host_capability_execution_failed",
        message,
        false,
    )
}

fn required_snapshot_operation_id(input: &Value) -> Result<&str, HostCapabilityError> {
    input
        .get("operation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| snapshot_error("operation_id is required"))
}

fn invoke_neural_embed(
    vectorizer: &SharedPluginVectorizer,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    let texts = embed_texts(&input)?;
    let vectorizer = vectorizer
        .lock()
        .map_err(|_| embed_failure("plugin vectorizer mutex poisoned"))?
        .clone()
        .ok_or_else(|| embed_unavailable("no text vectorizer is configured"))?;
    let embedded = texts
        .iter()
        .map(|text| embed_one(vectorizer.as_ref(), text))
        .collect::<Result<Vec<_>, _>>()?;
    let vectors = embedded
        .iter()
        .map(|result| result.vector.clone())
        .collect::<Vec<_>>();
    validate_vectors(&vectors, texts.len())?;
    let (engine_id, model) = embedded
        .first()
        .map(|result| (result.engine_id.clone(), result.model.clone()))
        .unwrap_or_default();
    Ok(json!({"vectors": vectors, "engine_id": engine_id, "model": model}))
}

fn embed_texts(input: &Value) -> Result<Vec<&str>, HostCapabilityError> {
    let fields = input
        .as_object()
        .ok_or_else(|| invalid_embed_input(input, "object with exactly field `texts`"))?;
    require_embed_fields(fields)?;
    let values = fields["texts"]
        .as_array()
        .ok_or_else(|| invalid_embed_input(&fields["texts"], "non-empty array field `texts`"))?;
    validate_embed_count(values.len(), &fields["texts"])?;
    let texts = values
        .iter()
        .map(embed_text)
        .collect::<Result<Vec<_>, _>>()?;
    validate_total_text_bytes(&texts, &fields["texts"])?;
    Ok(texts)
}

fn require_embed_fields(fields: &Map<String, Value>) -> Result<(), HostCapabilityError> {
    if fields.len() == 1 && fields.contains_key("texts") {
        return Ok(());
    }
    Err(invalid_embed_input(
        &Value::Object(fields.clone()),
        "object with exactly field `texts`",
    ))
}

fn validate_embed_count(count: usize, value: &Value) -> Result<(), HostCapabilityError> {
    if (1..=MAX_EMBED_TEXTS).contains(&count) {
        return Ok(());
    }
    Err(invalid_embed_input(
        value,
        &format!("array containing 1 through {MAX_EMBED_TEXTS} texts"),
    ))
}

fn embed_text(value: &Value) -> Result<&str, HostCapabilityError> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= MAX_EMBED_TEXT_BYTES)
        .ok_or_else(|| {
            invalid_embed_input(
                value,
                &format!("non-empty UTF-8 string no larger than {MAX_EMBED_TEXT_BYTES} bytes"),
            )
        })
}

fn validate_total_text_bytes(texts: &[&str], value: &Value) -> Result<(), HostCapabilityError> {
    let total = texts.iter().map(|text| text.len()).sum::<usize>();
    if total <= MAX_EMBED_TOTAL_BYTES {
        return Ok(());
    }
    Err(invalid_embed_input(
        value,
        &format!("texts totaling no more than {MAX_EMBED_TOTAL_BYTES} UTF-8 bytes"),
    ))
}

fn embed_one(
    vectorizer: &dyn ArtifactTextVectorizer,
    text: &str,
) -> Result<lumvise_db_core::ArtifactTextVector, HostCapabilityError> {
    vectorizer
        .vectorize_artifact_text(text)
        .map_err(|error| embed_failure(format!("text vectorization failed: {error}")))
}

fn validate_vectors(vectors: &[Vec<f32>], expected: usize) -> Result<(), HostCapabilityError> {
    let dimensions = vectors.first().map(Vec::len).unwrap_or_default();
    let valid = vectors.len() == expected
        && (1..=MAX_EMBED_DIMENSIONS).contains(&dimensions)
        && vectors.iter().all(|vector| {
            vector.len() == dimensions && vector.iter().all(|value| value.is_finite())
        });
    if valid {
        return Ok(());
    }
    Err(embed_failure(format!(
        "invalid vectorizer output: received {} vectors with first dimension {dimensions}; expected {expected} finite vectors with equal dimensions from 1 through {MAX_EMBED_DIMENSIONS}",
        vectors.len()
    )))
}

fn invalid_embed_input(value: &Value, expected: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        NEURAL_EMBED_CAPABILITY,
        "invalid_host_capability_input",
        format!("invalid neural.embed input `{value}`; expected {expected}"),
        false,
    )
}

fn embed_unavailable(message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new(
        NEURAL_EMBED_CAPABILITY,
        "host_capability_unavailable",
        message,
        true,
    )
}

fn embed_failure(message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new(
        NEURAL_EMBED_CAPABILITY,
        "host_capability_execution_failed",
        message,
        true,
    )
}

fn invoke_plugin_storage_row(
    relational: &dyn RelationalPersistence,
    plugin_id: &str,
    operation: &str,
    fields: &Map<String, Value>,
) -> Result<Value, HostCapabilityError> {
    let table_name = required_string(fields, "table_name")?;
    match operation {
        "ensure_table" => ensure_table(relational, plugin_id, table_name, fields),
        "put_row" => put_row(relational, plugin_id, table_name, fields),
        "get_row" => get_row(relational, plugin_id, table_name, fields),
        "list_rows" => list_rows(relational, plugin_id, table_name, fields),
        "trim_rows_by_key" => trim_rows_by_key(relational, plugin_id, table_name, fields),
        _ => Err(invalid_input(
            &Value::String(operation.into()),
            "supported storage.plugin operation",
        )),
    }
}

fn trim_rows_by_key(
    relational: &dyn RelationalPersistence,
    plugin_id: &str,
    table_name: &str,
    fields: &Map<String, Value>,
) -> Result<Value, HostCapabilityError> {
    require_fields(fields, &["operation", "table_name", "retained_rows"])?;
    let retained_rows = fields["retained_rows"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid_input(&fields["retained_rows"], "positive retained_rows integer"))?;
    match relational_execute(
        relational,
        RelationalOperation::TrimPluginData {
            plugin_id: plugin_id.to_owned(),
            table_name: table_name.to_owned(),
            retained_rows,
        },
    )? {
        RelationalResult::PluginDataTrimmed { removed_rows } => {
            Ok(json!({"rows_deleted": removed_rows}))
        }
        result => unexpected_relational_result("PluginDataTrimmed", result),
    }
}

fn mutate_rows(
    relational: &dyn RelationalPersistence,
    plugin_id: &str,
    fields: &Map<String, Value>,
) -> Result<Value, HostCapabilityError> {
    require_fields(fields, &["operation", "mutations"])?;
    let mutations = parse_mutations(fields, MAX_STAGED_CHANGES, MAX_STAGED_BYTES)?;
    mutation_result_value(relational_execute(
        relational,
        RelationalOperation::ApplyMutations {
            plugin_id: plugin_id.to_owned(),
            mutations,
        },
    )?)
}

fn parse_mutations(
    fields: &Map<String, Value>,
    max_count: usize,
    max_bytes: usize,
) -> Result<Vec<PluginDataMutation>, HostCapabilityError> {
    let value = required_value(fields, "mutations")?;
    let count = value
        .as_array()
        .map(Vec::len)
        .ok_or_else(|| invalid_input(value, "non-empty array field `mutations`"))?;
    if !(1..=max_count).contains(&count) {
        return Err(storage_limit("mutation chunk count", count, max_count));
    }
    let bytes = serde_json::to_vec(value)
        .map_err(|error| invalid_input(&json!(error.to_string()), "serializable mutations"))?
        .len();
    if bytes > max_bytes {
        return Err(storage_limit("mutation chunk bytes", bytes, max_bytes));
    }
    serde_json::from_value(value.clone()).map_err(|error| {
        invalid_input(
            value,
            &format!("put/delete mutations with valid fields: {error}"),
        )
    })
}

fn next_storage_transaction_id() -> Result<String, HostCapabilityError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| {
        storage_execution_failure(format!(
            "failed to generate opaque storage transaction id: {error}"
        ))
    })?;
    let encoded = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("storage-transaction-{encoded}"))
}

fn discard_expired_transactions(transactions: &mut HashMap<String, StagedStorageTransaction>) {
    transactions
        .retain(|_, transaction| transaction.created_at.elapsed() < STORAGE_TRANSACTION_TTL);
}

fn owned_transaction_mut<'map>(
    transactions: &'map mut HashMap<String, StagedStorageTransaction>,
    transaction_id: &str,
    plugin_id: &str,
) -> Result<&'map mut StagedStorageTransaction, HostCapabilityError> {
    require_owned_transaction(transactions, transaction_id, plugin_id)?;
    transactions
        .get_mut(transaction_id)
        .ok_or_else(|| unknown_transaction(transaction_id))
}

fn require_owned_transaction(
    transactions: &HashMap<String, StagedStorageTransaction>,
    transaction_id: &str,
    plugin_id: &str,
) -> Result<(), HostCapabilityError> {
    if transactions
        .get(transaction_id)
        .is_some_and(|transaction| transaction.plugin_id == plugin_id)
    {
        Ok(())
    } else {
        Err(unknown_transaction(transaction_id))
    }
}

fn unknown_transaction(transaction_id: &str) -> HostCapabilityError {
    invalid_input(
        &Value::String(transaction_id.into()),
        "active storage transaction owned by invoking plugin",
    )
}

fn storage_limit(subject: &str, actual: usize, maximum: usize) -> HostCapabilityError {
    HostCapabilityError::new(
        PLUGIN_STORAGE_CAPABILITY,
        "host_capability_quota_exceeded",
        format!("storage.plugin {subject} `{actual}` exceeds maximum `{maximum}`"),
        false,
    )
}

fn storage_execution_failure(message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new(
        PLUGIN_STORAGE_CAPABILITY,
        "host_capability_execution_failed",
        message,
        false,
    )
}

fn ensure_table(
    relational: &dyn RelationalPersistence,
    plugin_id: &str,
    table_name: &str,
    fields: &Map<String, Value>,
) -> Result<Value, HostCapabilityError> {
    require_fields(fields, &["operation", "table_name", "schema"])?;
    let schema = required_value(fields, "schema")?.clone();
    match relational_execute(
        relational,
        RelationalOperation::EnsurePluginDataTable {
            plugin_id: plugin_id.to_owned(),
            table_name: table_name.to_owned(),
            schema,
        },
    )? {
        RelationalResult::PluginDataTable(table) => {
            Ok(json!({"table_name": table.table_name, "schema": table.schema}))
        }
        result => unexpected_relational_result("PluginDataTable", result),
    }
}

fn put_row(
    relational: &dyn RelationalPersistence,
    plugin_id: &str,
    table_name: &str,
    fields: &Map<String, Value>,
) -> Result<Value, HostCapabilityError> {
    require_fields(fields, &["operation", "table_name", "row_key", "value"])?;
    let row_key = required_string(fields, "row_key")?.to_owned();
    let value = required_value(fields, "value")?.clone();
    let metric_value = value.clone();
    let result = match relational_execute(
        relational,
        RelationalOperation::PutPluginData {
            plugin_id: plugin_id.to_owned(),
            table_name: table_name.to_owned(),
            row_key,
            value,
        },
    )? {
        RelationalResult::PluginDataRow(Some(row)) => {
            Ok(json!({"row_key": row.row_key, "value": row.value}))
        }
        result => unexpected_relational_result("PluginDataRow", result),
    };
    if result.is_ok() {
        record_assistant_metric(plugin_id, table_name, &metric_value);
    }
    result
}

fn record_assistant_metric(plugin_id: &str, table_name: &str, value: &Value) {
    if plugin_id != "builtin.assistant" || table_name != "assistant_session_events" {
        return;
    }
    let Some(event_kind) = value["event_kind"].as_str() else {
        return;
    };
    let payload = &value["payload"];
    let engine = value["provider_id"].as_str().unwrap_or("unknown");
    match event_kind {
        "provider_turn_metric" => crate::observability::record_assistant_provider_turn(
            engine,
            payload["transport"].as_str().unwrap_or("unknown"),
            payload["outcome"].as_str().unwrap_or("unknown"),
            payload["code"].as_str(),
        ),
        "session_metric" => crate::observability::record_assistant_session(
            engine,
            payload["outcome"].as_str().unwrap_or("unknown"),
            payload["code"].as_str(),
        ),
        _ => {}
    }
}

fn get_row(
    relational: &dyn RelationalPersistence,
    plugin_id: &str,
    table_name: &str,
    fields: &Map<String, Value>,
) -> Result<Value, HostCapabilityError> {
    require_fields(fields, &["operation", "table_name", "row_key"])?;
    let row_key = required_string(fields, "row_key")?.to_owned();
    match relational_execute(
        relational,
        RelationalOperation::GetPluginData {
            plugin_id: plugin_id.to_owned(),
            table_name: table_name.to_owned(),
            row_key,
        },
    )? {
        RelationalResult::PluginDataRow(row) => {
            Ok(json!({"row": row.map(|row| json!({"row_key": row.row_key, "value": row.value}))}))
        }
        result => unexpected_relational_result("PluginDataRow", result),
    }
}

fn list_rows(
    relational: &dyn RelationalPersistence,
    plugin_id: &str,
    table_name: &str,
    fields: &Map<String, Value>,
) -> Result<Value, HostCapabilityError> {
    require_fields(
        fields,
        &[
            "operation",
            "table_name",
            "key_prefix",
            "after_key",
            "limit",
        ],
    )?;
    let key_prefix = optional_string(fields, "key_prefix")?.map(str::to_owned);
    let after_key = optional_string(fields, "after_key")?.map(str::to_owned);
    let limit = optional_limit(fields)?.unwrap_or(500);
    match relational_execute(
        relational,
        RelationalOperation::PagePluginData {
            plugin_id: plugin_id.to_owned(),
            table_name: table_name.to_owned(),
            key_prefix,
            after_key,
            limit,
        },
    )? {
        RelationalResult::PluginDataPage(page) => {
            let rows = page
                .rows
                .into_iter()
                .map(|row| json!({"row_key": row.row_key, "value": row.value}))
                .collect::<Vec<_>>();
            Ok(json!({"rows": rows, "next_after_key": page.next_after_key}))
        }
        result => unexpected_relational_result("PluginDataPage", result),
    }
}

fn optional_string<'input>(
    fields: &'input Map<String, Value>,
    field: &str,
) -> Result<Option<&'input str>, HostCapabilityError> {
    let Some(value) = fields.get(field).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(Some)
        .ok_or_else(|| invalid_input(value, &format!("optional string field `{field}`")))
}

fn optional_limit(fields: &Map<String, Value>) -> Result<Option<usize>, HostCapabilityError> {
    let Some(value) = fields.get("limit").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let limit = value
        .as_u64()
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|limit| (1..=1_000).contains(limit))
        .ok_or_else(|| {
            invalid_input(value, "optional integer field `limit` from 1 through 1000")
        })?;
    Ok(Some(limit))
}

fn required_string<'input>(
    fields: &'input Map<String, Value>,
    field: &str,
) -> Result<&'input str, HostCapabilityError> {
    fields
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            invalid_input(
                fields.get(field).unwrap_or(&Value::Null),
                &format!("non-empty string field `{field}`"),
            )
        })
}

fn required_value<'input>(
    fields: &'input Map<String, Value>,
    field: &str,
) -> Result<&'input Value, HostCapabilityError> {
    fields.get(field).ok_or_else(|| {
        invalid_input(
            &Value::Null,
            &format!("present JSON field `{field}`, including null when intentional"),
        )
    })
}

fn require_fields(
    fields: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), HostCapabilityError> {
    let allowed = allowed.iter().copied().collect::<BTreeSet<_>>();
    let unexpected = fields
        .keys()
        .filter(|field| !allowed.contains(field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if unexpected.is_empty() {
        return Ok(());
    }
    Err(invalid_input(
        &json!(unexpected),
        &format!(
            "only fields {}",
            allowed.into_iter().collect::<Vec<_>>().join(", ")
        ),
    ))
}

fn invalid_input(value: &Value, expected: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        PLUGIN_STORAGE_CAPABILITY,
        "invalid_host_capability_input",
        format!("invalid storage.plugin input `{value}`; expected {expected}"),
        false,
    )
}

fn storage_error(error: lumvise_db_core::DbError) -> HostCapabilityError {
    HostCapabilityError::new(
        PLUGIN_STORAGE_CAPABILITY,
        "host_capability_execution_failed",
        format!("storage.plugin operation failed: {error}"),
        false,
    )
}

fn relational_execute(
    relational: &dyn RelationalPersistence,
    operation: RelationalOperation,
) -> Result<RelationalResult, HostCapabilityError> {
    relational
        .execute(operation, &InvocationControl::sixty_seconds())
        .map_err(storage_error)
}

fn mutation_result_value(result: RelationalResult) -> Result<Value, HostCapabilityError> {
    match result {
        RelationalResult::PluginDataMutations(result) => {
            Ok(json!({"rows_put": result.rows_put, "rows_deleted": result.rows_deleted}))
        }
        result => unexpected_relational_result("PluginDataMutations", result),
    }
}

fn unexpected_relational_result(
    expected: &str,
    result: RelationalResult,
) -> Result<Value, HostCapabilityError> {
    Err(storage_execution_failure(format!(
        "relational persistence returned `{result:?}`; expected {expected}"
    )))
}

#[cfg(test)]
#[path = "host_capability_broker/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "host_capability_broker/snapshot_project_tests.rs"]
mod snapshot_project_tests;

#[cfg(test)]
mod semantic_snapshot_controlled_tests {
    use super::*;
    use lumvise_db_core::{
        DbError, LocalPersistence, PzSnapshotResult, SemanticOperation, SemanticReadiness,
        SemanticResult,
    };
    use lumvise_plugin_runtime::PluginInvocationClass;
    use std::sync::mpsc;

    struct DropProbePersistence {
        completed: mpsc::Sender<String>,
        dropped: mpsc::Sender<()>,
    }

    impl Drop for DropProbePersistence {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }

    impl SemanticPersistence for DropProbePersistence {
        fn execute(
            &self,
            operation: SemanticOperation,
            _control: &InvocationControl,
        ) -> std::result::Result<SemanticResult, DbError> {
            match operation {
                SemanticOperation::ProjectRoots => {
                    Ok(SemanticResult::ProjectRoots(vec!["/project".into()]))
                }
                SemanticOperation::CreatePzSnapshot {
                    project_root,
                    output_path,
                } => {
                    let _ = self.completed.send(project_root);
                    Ok(SemanticResult::PzSnapshot(PzSnapshotResult {
                        project_id: "project-id".into(),
                        snapshot_id: "snapshot-id".into(),
                        commit_version: 1,
                        published_at: "2026-01-01T00:00:00Z".into(),
                        output_path: output_path.into(),
                        output_bytes: 1,
                        row_counts: Default::default(),
                    }))
                }
                _ => Err(DbError::invalid_value(
                    "operation",
                    "drop probe snapshot operation",
                )),
            }
        }

        fn readiness(&self) -> std::result::Result<SemanticReadiness, DbError> {
            Ok(SemanticReadiness { ready: true })
        }
    }

    #[test]
    fn controlled_snapshot_uses_current_project_and_watcher_does_not_retain_service() {
        let (completed_tx, completed_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let semantic = Arc::new(DropProbePersistence {
            completed: completed_tx,
            dropped: dropped_tx,
        });
        let persistence = Arc::new(LocalPersistence::in_memory().expect("local persistence"));
        let relational: Arc<dyn RelationalPersistence> = persistence;
        let broker = AppCoreHostCapabilityBroker::new(
            Arc::clone(&semantic) as Arc<dyn SemanticPersistence>,
            relational,
        );
        drop(semantic);

        let context = PluginInvocationContext::new(
            "request-1",
            "owner-1",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        )
        .with_route_identity("session-1", Some("signed-scope-id".into()));
        let output = broker
            .invoke_controlled(
                HostCapabilityRequest {
                    plugin_id: "plugin.a".into(),
                    invocation_id: "invocation-1".into(),
                    call_id: "call-1".into(),
                    capability_id: SEMANTIC_SNAPSHOT_CAPABILITY.into(),
                    required_version: "1".into(),
                    input: json!({"operation": "create"}),
                },
                &context,
            )
            .expect("controlled snapshot create");
        assert!(output.get("operation_id").and_then(Value::as_str).is_some());
        assert_eq!(
            completed_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("snapshot worker reached persistence"),
            "/project"
        );

        drop(broker);
        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("broker drop releases semantic persistence");
    }
}
