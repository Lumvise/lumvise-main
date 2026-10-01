//! Generates JSON Schema for Obsidian contracts.
use schemars::schema_for;
use serde_json::{json, to_string_pretty};

#[path = "../obsidian.rs"]
#[allow(dead_code)]
mod obsidian;
use obsidian::{ObsidianSyncBatchV1, ObsidianSyncRequestV1};

fn main() -> Result<(), serde_json::Error> {
    let schema = json!({
        "obSyncRequestV1": schema_for!(ObsidianSyncRequestV1),
        "obSyncBatchV1": schema_for!(ObsidianSyncBatchV1),
    });
    println!("{}", to_string_pretty(&schema)?);
    Ok(())
}
