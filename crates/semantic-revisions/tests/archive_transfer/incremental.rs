//! Named in-memory adapter exercises only the public opaque policy interface.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

type Rows = BTreeMap<(String, [u8; 32]), Value>;

pub(super) struct ArchiveCatalog {
    pub catalogs: BTreeMap<String, Rows>,
    pub header: String,
    pub context: Value,
    pub peak_checkpoint: usize,
    pub steps: usize,
    pub advances: usize,
    pub total_queries: usize,
    pub peak_fact_bytes: usize,
    pub peak_queries: usize,
    pub peak_write_bytes: usize,
    pub largest_row: usize,
    removed: BTreeSet<(String, [u8; 32])>,
}

pub(super) fn invoke(request: Value) -> Result<Value, String> {
    let response: Value =
        serde_json::from_slice(&lumvise_semantic_revisions::evaluate_revision_policy(
            &serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap(),
        ))
        .unwrap();
    if response["status"] != "accepted" {
        return Err(response["message"].as_str().unwrap().into());
    }
    let result = &response["result"]["value"]["value"];
    Ok(serde_json::from_str(result["value_json"].as_str().unwrap()).unwrap())
}

impl ArchiveCatalog {
    pub fn new(pages: &[String], current: &Value, expected: i64) -> Self {
        let first: Value = serde_json::from_str(&pages[0]).unwrap();
        let header = first["header"].to_string();
        let root = current.as_array().unwrap().iter().find(|row| row["key"]["kind"] == "selection")
            .map(|row| serde_json::from_str::<Value>(row["metadata_json"].as_str().unwrap()).unwrap()["value"]["project_root"].as_str().unwrap().to_owned())
            .unwrap_or_else(|| "/online".into());
        let mut fixture = Self {
            catalogs: ["incoming", "current", "candidate", "scratch"]
                .into_iter()
                .map(|name| (name.into(), Rows::new()))
                .collect(),
            context: json!({"project_root":root,"project_id":first["header"]["project_id"],"authenticated_actor":"archive-owner",
                "operation_id":"large-recovery-import-v1","archive_digest":"a".repeat(64),"expected_commit":expected.to_string(),
                "archive_commit":first["header"]["archive_commit"],"now_ms":MERGE_NOW_MS.to_string()}),
            header,
            peak_checkpoint: 0,
            steps: 0,
            advances: 0,
            total_queries: 0,
            peak_fact_bytes: 0,
            peak_queries: 0,
            peak_write_bytes: 0,
            largest_row: 0,
            removed: BTreeSet::new(),
        };
        for row in current.as_array().unwrap() {
            fixture
                .catalogs
                .get_mut("current")
                .unwrap()
                .insert(storage_position(row), row.clone());
        }
        for page in pages {
            fixture.stage(page).unwrap();
        }
        fixture
    }

    pub fn stage(&mut self, page: &str) -> Result<(), String> {
        let reply = invoke(
            json!({"action":"archive_import_page","header_json":self.header,"page_json":page}),
        )?;
        for row in reply["rows"].as_array().unwrap() {
            self.largest_row = self.largest_row.max(row.to_string().len());
            if self
                .catalogs
                .get_mut("incoming")
                .unwrap()
                .insert(storage_position(row), row.clone())
                .is_some()
            {
                return Err("duplicate source identity across immutable parts".into());
            }
        }
        Ok(())
    }

    pub fn run(&mut self) -> Result<Value, String> {
        let mut checkpoint = Value::Null;
        let mut facts = json!([]);
        let mut previous_wait = None;
        for _ in 0..200_000 {
            self.steps += 1;
            let result = invoke(json!({"action":"archive_import_step","context":self.context,"header_json":self.header,
                "checkpoint_json":checkpoint,"facts":facts})).map_err(|error| format!("checkpoint {checkpoint}: {error}"))?;
            if result["state"] == "need_facts" {
                let wait = (checkpoint.clone(), result["queries"].clone());
                if previous_wait.as_ref() == Some(&wait) {
                    return Err(
                        "unchanged fact request cannot progress under the bounded loader".into(),
                    );
                }
                previous_wait = Some(wait);
                let queries = result["queries"].as_array().unwrap();
                self.total_queries += queries.len();
                self.peak_queries = self.peak_queries.max(queries.len());
                facts = Value::Array(self.bounded_facts(queries));
                self.peak_fact_bytes = self.peak_fact_bytes.max(facts.to_string().len());
                continue;
            }
            self.advances += 1;
            self.peak_write_bytes = self
                .peak_write_bytes
                .max(result["writes"].to_string().len());
            self.write(&result["writes"]);
            previous_wait = None;
            checkpoint = result["checkpoint_json"].clone();
            self.peak_checkpoint = self.peak_checkpoint.max(checkpoint.as_str().unwrap().len());
            facts = json!([]);
            if result["state"] == "ready" {
                let receipt = &result["acceptance"]["receipt_row"];
                self.catalogs
                    .get_mut("candidate")
                    .unwrap()
                    .insert(storage_position(receipt), receipt.clone());
                return Ok(result["acceptance"].clone());
            }
        }
        Err("archive validation did not converge".into())
    }

    pub fn rows(&self) -> Value {
        Value::Array(self.catalogs["candidate"].values().cloned().collect())
    }

    pub fn applied_rows(&self) -> Value {
        let mut live = self.catalogs["current"].clone();
        for key in &self.removed {
            live.remove(key);
        }
        for (key, row) in &self.catalogs["candidate"] {
            live.insert(key.clone(), row.clone());
        }
        Value::Array(live.into_values().collect())
    }

    fn bounded_facts(&self, queries: &[Value]) -> Vec<Value> {
        let mut facts = Vec::new();
        let mut bytes = 0;
        for (index, query) in queries.iter().enumerate() {
            let fact = self.read(query);
            let size = fact["rows"].to_string().len();
            if index < 5
                || fact["rows"].as_array().unwrap().is_empty()
                || bytes + size <= 1024 * 1024
            {
                bytes += size;
                facts.push(fact);
            }
        }
        facts
    }

    fn read(&self, query: &Value) -> Value {
        let catalog = &self.catalogs[query["catalog"].as_str().unwrap()];
        let lookup = &query["lookup"];
        if lookup["kind"] == "key" {
            let position = storage_position(&json!({"key":lookup["key"]}));
            let rows = catalog
                .get(&position)
                .filter(|row| row["key"] == lookup["key"])
                .cloned()
                .into_iter()
                .collect::<Vec<_>>();
            return json!({"query_id":query["query_id"],"rows":rows,"next":null});
        }
        let after = lookup
            .get("after")
            .filter(|value| !value.is_null())
            .map(|value| storage_position(&json!({"key":value})));
        let rows = catalog.iter().filter(|(position, row)| {
            after.as_ref().is_none_or(|after| *position > after) && matches_lookup(row, lookup)
        });
        let target = lookup["target_bytes"].as_u64().unwrap_or(u64::MAX) as usize;
        let mut collected = Vec::new();
        let mut bytes = 0;
        let mut more = false;
        for (_, row) in rows {
            let size = row.to_string().len();
            if !collected.is_empty() && bytes + size > target {
                more = true;
                break;
            }
            bytes += size;
            collected.push(row.clone());
        }
        let next = if more {
            collected.last().unwrap()["key"].clone()
        } else {
            Value::Null
        };
        return json!({"query_id":query["query_id"],"rows":collected,"next":next});
    }

    fn write(&mut self, writes: &Value) {
        for write in writes.as_array().unwrap() {
            let catalog = self
                .catalogs
                .get_mut(write["catalog"].as_str().unwrap())
                .unwrap();
            for key in write["removed"].as_array().unwrap() {
                let key = storage_position(&json!({"key":key}));
                if write["catalog"] == "candidate" {
                    self.removed.insert(key.clone());
                }
                catalog.remove(&key);
            }
            for row in write["rows"].as_array().unwrap() {
                if write["catalog"] == "candidate" {
                    self.removed.remove(&storage_position(row));
                }
                catalog.insert(storage_position(row), row.clone());
            }
        }
    }
}

fn row_key(row: &Value) -> (String, String) {
    (
        row["key"]["kind"].as_str().unwrap().into(),
        row["key"]["identity"].as_str().unwrap().into(),
    )
}
fn storage_position(row: &Value) -> (String, [u8; 32]) {
    let (kind, identity) = row_key(row);
    (kind, Sha256::digest(identity.as_bytes()).into())
}
fn matches_lookup(row: &Value, lookup: &Value) -> bool {
    match lookup["kind"].as_str().unwrap() {
        "key" => row["key"] == lookup["key"],
        "scan" => lookup["row_kind"].is_null() || row["key"]["kind"] == lookup["row_kind"],
        "index" => row["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|index| index["name"] == lookup["name"] && index["value"] == lookup["value"]),
        other => panic!("unknown archive fact lookup {other}"),
    }
}
