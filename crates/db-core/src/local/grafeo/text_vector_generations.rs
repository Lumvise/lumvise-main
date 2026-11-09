//! Local section-vector attempts. Only committed vectors use the searchable/exported label.
use super::graph_rows::{
    apply_element_text_vector_replace, nodes_by_label_and_property,
    prepare_element_text_vector_replace, semantic_element_by_id_selective, string_property,
};
use super::graph_store::GraphTransaction;
use super::semantic_storage::SemanticStorage;
use crate::{
    ArtifactTextVector, DbError, ElementTextVectorGenerationKey as Key,
    ElementTextVectorGenerationSpec as Spec, ElementTextVectorGenerationState as State,
    ElementTextVectorGenerationStatus as Status, Result, StoredSemanticElementTextVector,
};
use grafeo::{GrafeoDB, NodeId, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const RECEIPT: &str = "ElementTextVectorGeneration";
const STAGED: &str = "ElementTextVectorStage";
const IDLE_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Serialize, Deserialize)]
struct GenerationReceipt {
    key: Key,
    #[serde(skip_serializing_if = "Option::is_none")]
    spec: Option<Spec>,
    state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_digest: Option<String>,
    chunk_digests: Vec<String>,
    touched_ms: i64,
}

impl GenerationReceipt {
    fn require_spec(&self) -> Result<&Spec> {
        self.spec.as_ref().ok_or_else(|| {
            DbError::invalid_value(
                &self.key.generation_id,
                "generation spec for a begun publication",
            )
        })
    }

    fn validate(&self, key: &Key) -> Result<()> {
        if self.key != *key || !self.valid_spec() {
            return Err(DbError::invalid_value(
                &key.generation_id,
                "matching generation key and spec, or key-only Aborted receipt",
            ));
        }
        Ok(())
    }

    fn valid_spec(&self) -> bool {
        match &self.spec {
            Some(spec) => spec.key == self.key,
            None => {
                self.state == State::Aborted
                    && self.chunk_digests.is_empty()
                    && self.published_digest.is_none()
            }
        }
    }

    fn touch_open(&mut self, now: i64) {
        if self.state == State::Open {
            self.touched_ms = now;
        }
    }

    fn status(&self) -> Status {
        let chunk_count = self.spec.as_ref().map_or(0, |spec| spec.chunk_count);
        let published_count = if self.state == State::Committed {
            chunk_count as usize
        } else {
            0
        };
        Status {
            key: self.key.clone(),
            state: self.state,
            next_chunk: self.chunk_digests.len() as u32,
            chunk_count,
            published_count,
        }
    }
}

#[derive(Clone)]
struct ReceiptWrite {
    node: Option<NodeId>,
    receipt: GenerationReceipt,
    discard: Vec<NodeId>,
    append: Option<(u32, Vec<f32>)>,
    terminal_transition: bool,
}

impl ReceiptWrite {
    fn new(node: Option<NodeId>, receipt: GenerationReceipt, terminal_transition: bool) -> Self {
        Self {
            node,
            receipt,
            discard: Vec::new(),
            append: None,
            terminal_transition,
        }
    }
}

impl SemanticStorage<'_> {
    pub(crate) fn begin_text_vector_generation(&self, spec: &Spec) -> Result<Status> {
        validate_spec(spec)?;
        let now = self.clock.now().timestamp_millis();
        self.publish_generation_receipt(|graph| prepare_begin(graph, spec, now))
    }

    pub(crate) fn stage_text_vector_chunk(
        &self,
        key: &Key,
        chunk: u32,
        vector: &[f32],
    ) -> Result<Status> {
        let now = self.clock.now().timestamp_millis();
        self.publish_generation_receipt(|graph| prepare_stage(graph, key, chunk, vector, now))
    }

    pub(crate) fn commit_text_vector_generation(&self, key: &Key) -> Result<Status> {
        let prepare = |graph: &GrafeoDB| prepare_commit(graph, key);
        let initial = self.graph.plan(&prepare)?;
        if !initial.value().0.terminal_transition {
            return Ok(initial.value().0.receipt.status());
        }
        self.commit_prepared_graph_write(initial, prepare, |graph, _, _, (plan, replacement)| {
            if let Some(replacement) = replacement {
                apply_element_text_vector_replace(graph, replacement)?;
            }
            apply_receipt(graph, plan)
        })
    }

    pub(crate) fn abort_text_vector_generation(&self, key: &Key) -> Result<Status> {
        self.publish_generation_receipt(|graph| {
            let mut plan = prepare_abort(graph, key)?;
            if plan.receipt.state == State::Open {
                plan.receipt.state = State::Aborted;
                plan.terminal_transition = true;
                plan.discard = staged_node_ids(graph, key)?;
            }
            Ok(plan)
        })
    }

    fn publish_generation_receipt(
        &self,
        prepare: impl Fn(&GrafeoDB) -> Result<ReceiptWrite>,
    ) -> Result<Status> {
        let initial = self.graph.plan(&prepare)?;
        if initial.value().receipt.state != State::Open && !initial.value().terminal_transition {
            return Ok(initial.value().receipt.status());
        }
        self.commit_prepared_graph_write(initial, prepare, |graph, _, _, plan| {
            apply_receipt(graph, plan)
        })
    }

    pub(crate) fn expire_text_vector_generations(&self) -> Result<()> {
        let now = self.clock.now().timestamp_millis();
        let inspect = |graph: &GrafeoDB| expired_receipts(graph, now);
        let initial = self.graph.plan(&inspect)?;
        if initial.value().is_empty() {
            return Ok(());
        }
        self.commit_prepared_graph_write(initial, inspect, |graph, _, _, plans| {
            for plan in plans {
                apply_receipt(graph, plan)?;
            }
            Ok(())
        })
    }
}

fn validate_key(key: &Key) -> Result<()> {
    for value in [
        &key.project_root,
        &key.semantic_element_id,
        &key.generation_id,
    ] {
        crate::local::sql::validation::require_non_empty(
            value,
            "non-empty generation identity field",
        )?;
    }
    Ok(())
}

fn validate_spec(spec: &Spec) -> Result<()> {
    validate_key(&spec.key)?;
    for value in [
        &spec.content_fingerprint,
        &spec.engine_id,
        &spec.profile_fingerprint,
        &spec.chunk_plan_fingerprint,
    ] {
        crate::local::sql::validation::require_non_empty(
            value,
            "non-empty immutable generation field",
        )?;
    }
    validate_dimensions(spec)
}

fn validate_dimensions(spec: &Spec) -> Result<()> {
    if spec.dimensions == 0 || spec.dimensions > i64::MAX as usize || spec.chunk_count == 0 {
        return Err(DbError::invalid_value(
            format!(
                "{} dimensions, {} chunks",
                spec.dimensions, spec.chunk_count
            ),
            "positive dimensions and chunk count",
        ));
    }
    Ok(())
}

fn identity(key: &Key) -> Result<String> {
    validate_key(key)?;
    Ok(serde_json::to_string(key)?)
}

fn matching_element(graph: &GrafeoDB, spec: &Spec) -> Result<bool> {
    Ok(
        semantic_element_by_id_selective(graph, &spec.key.semantic_element_id)?.is_some_and(
            |element| {
                element.project_root == spec.key.project_root
                    && element.element_kind == "html_section"
                    && element.content_fingerprint.as_deref()
                        == Some(spec.content_fingerprint.as_str())
            },
        ),
    )
}

fn read_receipt(graph: &GrafeoDB, key: &Key) -> Result<Option<(NodeId, GenerationReceipt)>> {
    let nodes =
        nodes_by_label_and_property(graph, RECEIPT, "generation_identity", &identity(key)?)?;
    if nodes.len() > 1 {
        return Err(DbError::invalid_value(
            &key.generation_id,
            "one durable receipt per generation key",
        ));
    }
    nodes
        .first()
        .map(|node| decode_receipt(node, key))
        .transpose()
}

fn decode_receipt(node: &grafeo::store::Node, key: &Key) -> Result<(NodeId, GenerationReceipt)> {
    let body = string_property(node, "receipt_json").ok_or_else(|| {
        DbError::invalid_value(&key.generation_id, "durable generation receipt JSON")
    })?;
    let receipt: GenerationReceipt = serde_json::from_str(&body)?;
    receipt.validate(key)?;
    Ok((node.id, receipt))
}

fn existing_receipt(graph: &GrafeoDB, key: &Key) -> Result<ReceiptWrite> {
    let (node, receipt) = read_receipt(graph, key)?
        .ok_or_else(|| DbError::invalid_value(&key.generation_id, "existing generation key"))?;
    Ok(ReceiptWrite::new(Some(node), receipt, false))
}

fn prepare_abort(graph: &GrafeoDB, key: &Key) -> Result<ReceiptWrite> {
    if let Some((node, receipt)) = read_receipt(graph, key)? {
        return Ok(ReceiptWrite::new(Some(node), receipt, false));
    }
    validate_key(key)?;
    let receipt = GenerationReceipt {
        key: key.clone(),
        spec: None,
        state: State::Aborted,
        published_digest: None,
        chunk_digests: Vec::new(),
        touched_ms: 0,
    };
    Ok(ReceiptWrite::new(None, receipt, true))
}

fn prepare_begin(graph: &GrafeoDB, spec: &Spec, now: i64) -> Result<ReceiptWrite> {
    if let Some((node, receipt)) = read_receipt(graph, &spec.key)? {
        return replay_begin(node, receipt, spec);
    }
    ensure_no_open_attempt(graph, spec)?;
    if !matching_element(graph, spec)? {
        return Err(DbError::invalid_value(
            &spec.key.semantic_element_id,
            "current html_section in requested project with matching full content fingerprint",
        ));
    }
    let receipt = open_receipt(graph, spec, now)?;
    Ok(ReceiptWrite::new(None, receipt, false))
}

fn replay_begin(node: NodeId, receipt: GenerationReceipt, spec: &Spec) -> Result<ReceiptWrite> {
    if receipt
        .spec
        .as_ref()
        .is_some_and(|original| original != spec)
    {
        return Err(DbError::invalid_value(
            &spec.key.generation_id,
            "identical immutable generation spec on replay",
        ));
    }
    Ok(ReceiptWrite::new(Some(node), receipt, false))
}

fn open_receipt(graph: &GrafeoDB, spec: &Spec, now: i64) -> Result<GenerationReceipt> {
    Ok(GenerationReceipt {
        key: spec.key.clone(),
        spec: Some(spec.clone()),
        state: State::Open,
        published_digest: Some(published_digest(graph, &spec.key)?),
        chunk_digests: Vec::new(),
        touched_ms: now,
    })
}

fn ensure_no_open_attempt(graph: &GrafeoDB, spec: &Spec) -> Result<()> {
    let element_id = &spec.key.semantic_element_id;
    let nodes = nodes_by_label_and_property(graph, RECEIPT, "semantic_element_id", element_id)?;
    for node in nodes {
        let body = string_property(&node, "receipt_json").ok_or_else(|| {
            DbError::invalid_value(node.id.to_string(), "generation receipt JSON")
        })?;
        let receipt: GenerationReceipt = serde_json::from_str(&body)?;
        if receipt.key.project_root == spec.key.project_root && receipt.state == State::Open {
            return Err(DbError::invalid_value(
                &spec.key.generation_id,
                "at most one open generation per element",
            ));
        }
    }
    Ok(())
}

fn prepare_stage(
    graph: &GrafeoDB,
    key: &Key,
    chunk: u32,
    vector: &[f32],
    now: i64,
) -> Result<ReceiptWrite> {
    let mut plan = existing_receipt(graph, key)?;
    let digest = vector_digest(vector);
    if let Some(previous) = plan.receipt.chunk_digests.get(chunk as usize) {
        validate_replayed_chunk(chunk, previous, &digest)?;
        plan.receipt.touch_open(now);
        return Ok(plan);
    }
    if plan.receipt.state == State::Open {
        append_chunk(&mut plan, chunk, vector, digest, now)?;
    }
    Ok(plan)
}

fn validate_replayed_chunk(chunk: u32, previous: &str, digest: &str) -> Result<()> {
    if previous != digest {
        return Err(DbError::invalid_value(
            chunk.to_string(),
            "identical chunk vector on replay",
        ));
    }
    Ok(())
}

fn append_chunk(
    plan: &mut ReceiptWrite,
    chunk: u32,
    vector: &[f32],
    digest: String,
    now: i64,
) -> Result<()> {
    validate_chunk(plan.receipt.require_spec()?, chunk, vector)?;
    if chunk as usize != plan.receipt.chunk_digests.len() {
        return Err(DbError::invalid_value(
            chunk.to_string(),
            "next consecutive ordinal in an open generation",
        ));
    }
    plan.receipt.chunk_digests.push(digest);
    plan.receipt.touched_ms = now;
    plan.append = Some((chunk, vector.to_vec()));
    Ok(())
}

fn validate_chunk(spec: &Spec, chunk: u32, vector: &[f32]) -> Result<()> {
    if chunk >= spec.chunk_count
        || vector.len() != spec.dimensions
        || !vector.iter().all(|value| value.is_finite())
    {
        return Err(DbError::invalid_value(
            format!("chunk {chunk}, {} dimensions, {vector:?}", vector.len()),
            format!(
                "ordinal below {} and {} finite dimensions",
                spec.chunk_count, spec.dimensions
            ),
        ));
    }
    Ok(())
}

fn vector_digest(vector: &[f32]) -> String {
    let mut digest = Sha256::new();
    for value in vector {
        digest.update(value.to_bits().to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn published_digest(graph: &GrafeoDB, key: &Key) -> Result<String> {
    let nodes = nodes_by_label_and_property(
        graph,
        "SemanticElementTextVector",
        "semantic_element_id",
        &key.semantic_element_id,
    )?;
    let mut records = nodes
        .iter()
        .map(published_node_digest_record)
        .collect::<Vec<_>>();
    records.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&records)?)
    ))
}

fn published_node_digest_record(
    node: &grafeo::store::Node,
) -> (String, std::collections::BTreeMap<String, String>) {
    let properties = node
        .properties
        .iter()
        .map(|(name, value)| (name.to_string(), format!("{value:?}")))
        .collect();
    (node.id.to_string(), properties)
}

fn staged_nodes(graph: &GrafeoDB, key: &Key) -> Result<Vec<grafeo::store::Node>> {
    nodes_by_label_and_property(graph, STAGED, "generation_identity", &identity(key)?)
}

fn staged_node_ids(graph: &GrafeoDB, key: &Key) -> Result<Vec<NodeId>> {
    Ok(staged_nodes(graph, key)?
        .into_iter()
        .map(|node| node.id)
        .collect())
}

type CommitPlan = (
    ReceiptWrite,
    Option<super::graph_rows::TextVectorChunksPlan>,
);

fn prepare_commit(graph: &GrafeoDB, key: &Key) -> Result<CommitPlan> {
    let mut plan = existing_receipt(graph, key)?;
    if plan.receipt.state != State::Open {
        return Ok((plan, None));
    }
    require_complete_prefix(&plan.receipt)?;
    let nodes = staged_nodes(graph, key)?;
    let replacement = prepare_replacement(graph, &plan.receipt, &nodes)?;
    plan.terminal_transition = true;
    plan.receipt.state = if replacement.is_some() {
        State::Committed
    } else {
        State::Conflicted
    };
    plan.discard = nodes.iter().map(|node| node.id).collect();
    Ok((plan, replacement))
}

fn require_complete_prefix(receipt: &GenerationReceipt) -> Result<()> {
    if receipt.chunk_digests.len() != receipt.require_spec()?.chunk_count as usize {
        return Err(DbError::invalid_value(
            &receipt.key.generation_id,
            "all consecutive chunks staged before commit",
        ));
    }
    Ok(())
}

fn prepare_replacement(
    graph: &GrafeoDB,
    receipt: &GenerationReceipt,
    nodes: &[grafeo::store::Node],
) -> Result<Option<super::graph_rows::TextVectorChunksPlan>> {
    if !matching_element(graph, receipt.require_spec()?)?
        || Some(published_digest(graph, &receipt.key)?) != receipt.published_digest
    {
        return Ok(None);
    }
    let chunks = prepared_chunks(receipt, nodes)?;
    prepare_element_text_vector_replace(
        graph,
        &receipt.key.semantic_element_id,
        &chunks.iter().collect::<Vec<_>>(),
    )
    .map(Some)
}

fn prepared_chunks(
    receipt: &GenerationReceipt,
    nodes: &[grafeo::store::Node],
) -> Result<Vec<StoredSemanticElementTextVector>> {
    let spec = receipt.require_spec()?;
    let mut chunks = std::collections::BTreeMap::new();
    for node in nodes {
        insert_staged_chunk(receipt, node, &mut chunks)?;
    }
    require_complete_staging(chunks.len(), spec)?;
    let published = chunks
        .into_iter()
        .map(|(chunk, vector)| published_chunk(spec, chunk, vector));
    Ok(published.collect())
}

fn insert_staged_chunk(
    receipt: &GenerationReceipt,
    node: &grafeo::store::Node,
    chunks: &mut std::collections::BTreeMap<u32, Vec<f32>>,
) -> Result<()> {
    let (ordinal, vector) = decode_staged_chunk(node)?;
    validate_chunk(receipt.require_spec()?, ordinal, &vector)?;
    if receipt.chunk_digests.get(ordinal as usize) != Some(&vector_digest(&vector))
        || chunks.insert(ordinal, vector).is_some()
    {
        return Err(DbError::invalid_value(
            ordinal.to_string(),
            "unique staged chunk matching durable digest",
        ));
    }
    Ok(())
}

fn decode_staged_chunk(node: &grafeo::store::Node) -> Result<(u32, Vec<f32>)> {
    let ordinal = match node.get_property("chunk") {
        Some(Value::Int64(ordinal)) => u32::try_from(*ordinal).ok(),
        _ => None,
    }
    .ok_or_else(|| {
        DbError::invalid_value(node.id.to_string(), "native non-negative chunk ordinal")
    })?;
    match node.get_property("vector") {
        Some(Value::Vector(vector)) => Ok((ordinal, vector.to_vec())),
        _ => Err(DbError::invalid_value(
            node.id.to_string(),
            "native staged vector property",
        )),
    }
}

fn require_complete_staging(count: usize, spec: &Spec) -> Result<()> {
    if count != spec.chunk_count as usize {
        return Err(DbError::invalid_value(
            count.to_string(),
            "complete durable staged chunk set",
        ));
    }
    Ok(())
}

fn published_chunk(spec: &Spec, chunk: u32, vector: Vec<f32>) -> StoredSemanticElementTextVector {
    StoredSemanticElementTextVector {
        semantic_element_id: spec.key.semantic_element_id.clone(),
        project_root: spec.key.project_root.clone(),
        content_fingerprint: spec.content_fingerprint.clone(),
        chunk,
        vector: ArtifactTextVector {
            engine_id: spec.engine_id.clone(),
            model: spec.model.clone(),
            dimensions: spec.dimensions,
            vector,
            normalized: false,
            metadata: serde_json::json!({"profile_fingerprint": spec.profile_fingerprint, "chunk_plan_fingerprint": spec.chunk_plan_fingerprint}),
        },
    }
}

fn apply_receipt(graph: &GraphTransaction<'_>, plan: ReceiptWrite) -> Result<Status> {
    let receipt = &plan.receipt;
    let identity = identity(&receipt.key)?;
    for node in plan.discard {
        graph.delete_node(node)?;
    }
    if let Some((ordinal, vector)) = plan.append {
        apply_staged_chunk(graph, &receipt.key, &identity, ordinal, vector)?;
    }
    let properties = receipt_properties(receipt, identity)?;
    if let Some(node) = plan.node {
        for (name, value) in properties {
            graph.set_node_property(node, name, value)?;
        }
    } else {
        graph.create_node_with_props(&[RECEIPT], properties)?;
    }
    Ok(receipt.status())
}

fn apply_staged_chunk(
    graph: &GraphTransaction<'_>,
    key: &Key,
    identity: &str,
    ordinal: u32,
    vector: Vec<f32>,
) -> Result<()> {
    let element_id = Value::from(key.semantic_element_id.clone());
    graph.create_node_with_props(
        &[STAGED],
        [
            ("generation_identity", Value::from(identity)),
            ("semantic_element_id", element_id),
            ("project_root", Value::from(key.project_root.clone())),
            ("chunk", Value::from(i64::from(ordinal))),
            ("vector", Value::Vector(vector.into())),
        ],
    )?;
    Ok(())
}

fn receipt_properties(
    receipt: &GenerationReceipt,
    identity: String,
) -> Result<Vec<(&'static str, Value)>> {
    let state = if receipt.state == State::Open {
        "open"
    } else {
        "terminal"
    };
    let element_id = Value::from(receipt.key.semantic_element_id.clone());
    let project_root = Value::from(receipt.key.project_root.clone());
    Ok(vec![
        ("generation_identity", Value::from(identity)),
        ("semantic_element_id", element_id),
        ("project_root", project_root),
        ("generation_state", Value::from(state)),
        ("receipt_json", Value::from(serde_json::to_string(receipt)?)),
    ])
}

fn expired_receipts(graph: &GrafeoDB, now: i64) -> Result<Vec<ReceiptWrite>> {
    let mut expired = Vec::new();
    for node in nodes_by_label_and_property(graph, RECEIPT, "generation_state", "open")? {
        if let Some(plan) = expired_receipt(graph, &node, now)? {
            expired.push(plan);
        }
    }
    Ok(expired)
}

fn expired_receipt(
    graph: &GrafeoDB,
    node: &grafeo::store::Node,
    now: i64,
) -> Result<Option<ReceiptWrite>> {
    let body = string_property(node, "receipt_json").ok_or_else(|| {
        DbError::invalid_value(node.id.to_string(), "durable generation receipt JSON")
    })?;
    let mut receipt: GenerationReceipt = serde_json::from_str(&body)?;
    receipt.validate(&receipt.key)?;
    if now.saturating_sub(receipt.touched_ms) < IDLE_MS {
        return Ok(None);
    }
    receipt.state = State::Expired;
    let discard = staged_node_ids(graph, &receipt.key)?;
    let mut plan = ReceiptWrite::new(Some(node.id), receipt, false);
    plan.discard = discard;
    Ok(Some(plan))
}
