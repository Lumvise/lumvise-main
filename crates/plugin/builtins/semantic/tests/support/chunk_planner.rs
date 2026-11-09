//! Deterministic source-range fixture; real model tokenization is tested in Neural Core.
use lumvise_plugin_runtime::HostCapabilityError;
use serde_json::{Value, json};

pub(super) const PROFILE: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

pub(super) struct FixtureChunkPlanner;
impl FixtureChunkPlanner {
    pub fn plan(input: &Value) -> Result<Value, HostCapabilityError> {
        let text = input["text"]
            .as_str()
            .ok_or_else(|| super::host_error("neural.chunk_text", "invalid_input"))?;
        let mut ranges = Vec::new();
        let mut start = 0;
        if !text.trim().is_empty() {
            while start < text.len() {
                let end = text[start..]
                    .char_indices()
                    .nth(1022)
                    .map_or(text.len(), |(offset, _)| start + offset);
                ranges.push(json!({"start_byte":start,"end_byte":end,"token_count":text[start..end].chars().count()+2}));
                start = end;
            }
        }
        Ok(
            json!({"engine_id":"test-fixture-embed","model":"test-model","profile_fingerprint":PROFILE,"ranges":ranges}),
        )
    }
}
