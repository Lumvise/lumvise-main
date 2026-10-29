//! Projects accepted source descriptors; revision policy remains with the shared publisher.
//! Ingestion calls `publish`; watermark encoding and conditional row writes stay internal.
use crate::{
    models::{SemanticSource, SemanticSourceInput},
    parse, require_non_empty, storage,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const WATERMARK: &str = "source_publication";

#[cfg(test)]
mod tests;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourcePublicationWatermark {
    project_id: String,
    operation_id: String,
    commit_version: i64,
}

struct SourceDescriptorProjection<'a> {
    project_root: &'a str,
    watermark: SourcePublicationWatermark,
}

/// Publishes descriptor projections only after revision acceptance.
/// Example: `publish(context, root, &accepted_commit)` also handles receipt replay.
pub(super) fn publish(
    context: &mut PluginContext<'_>,
    root: &str,
    commit: &Value,
) -> Result<usize, PluginError> {
    let sources: Vec<SemanticSourceInput> =
        parse(commit["sources"].clone(), "accepted source descriptors")?;
    if sources.is_empty() {
        return Ok(0);
    }
    let projection = SourceDescriptorProjection::capture(root, commit)?;
    projection.require_lineage(context)?;
    let mutations = projection.pending(context, &sources)?;
    if let Err(error) = projection.apply(context, &mutations) {
        // Another instance may have already projected this receipt or a later one.
        projection.require_lineage(context)?;
        if !projection.pending(context, &sources)?.is_empty() {
            return Err(error);
        }
    }
    Ok(sources.len())
}

impl<'a> SourceDescriptorProjection<'a> {
    fn pending(
        &self,
        context: &mut PluginContext<'_>,
        sources: &[SemanticSourceInput],
    ) -> Result<Vec<storage::Mutation>, PluginError> {
        let mut mutations = Vec::new();
        for source in sources {
            self.append(context, source, &mut mutations)?;
        }
        Ok(mutations)
    }

    fn apply(
        &self,
        context: &mut PluginContext<'_>,
        mutations: &[storage::Mutation],
    ) -> Result<(), PluginError> {
        storage::apply_mutations(context, mutations).map_err(|error| {
            PluginError::new("source_projection_retry", format!(
                "source projection for operation `{}` at commit `{}` failed: {} {}; expected retry of the identical frozen request",
                self.watermark.operation_id, self.watermark.commit_version,
                error.code, error.message,
            ), true)
        })
    }

    fn capture(root: &'a str, commit: &Value) -> Result<Self, PluginError> {
        if commit["project_root"] != root || commit["publication"]["outcome"] != "accepted" {
            return Err(invalid(
                commit,
                "an accepted publication bound to the requested project root",
            ));
        }
        let receipt = &commit["publication"]["value"];
        let watermark: SourcePublicationWatermark = parse(
            json!({"project_id":commit["project_id"],
            "operation_id":receipt["operation_id"], "commit_version":receipt["commit_version"]}),
            "accepted source publication identity and ordering",
        )?;
        watermark.validate()?;
        Ok(Self {
            project_root: root,
            watermark,
        })
    }

    fn require_lineage(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let output = context.host_call(
            "storage.semantic",
            json!({"operation":"project_identity", "project_root":self.project_root}),
        )?;
        if output["identity"]["project_id"] == self.watermark.project_id {
            return Ok(());
        }
        Err(invalid(
            &output,
            "the original accepted source publication's project identity",
        ))
    }

    fn append(
        &self,
        context: &mut PluginContext<'_>,
        source: &SemanticSourceInput,
        mutations: &mut Vec<storage::Mutation>,
    ) -> Result<(), PluginError> {
        let key = storage::project_key(self.project_root, "s", &[&source.semantic_source_id]);
        let saved: Option<Value> = storage::get(context, storage::SOURCES, &key)?;
        if !self.should_advance(saved.as_ref())? {
            return Ok(());
        }
        let mut projected = source_value(self.project_root, source)?;
        projected[WATERMARK] = json!(self.watermark);
        mutations.push(storage::require_row_mutation(
            storage::SOURCES,
            &key,
            saved.as_ref(),
        ));
        mutations.push(storage::put_mutation(storage::SOURCES, &key, &projected)?);
        Ok(())
    }

    fn should_advance(&self, saved: Option<&Value>) -> Result<bool, PluginError> {
        let Some(value) = saved.and_then(|saved| saved.get(WATERMARK)) else {
            return Ok(true);
        };
        let previous: SourcePublicationWatermark =
            parse(value.clone(), "a stored source publication watermark")?;
        previous.validate()?;
        if previous.project_id != self.watermark.project_id {
            return Ok(true);
        }
        if previous.commit_version == self.watermark.commit_version
            && previous.operation_id != self.watermark.operation_id
        {
            return Err(invalid(
                value,
                "one accepted operation identity per project commit",
            ));
        }
        Ok(previous.commit_version < self.watermark.commit_version)
    }
}

impl SourcePublicationWatermark {
    fn validate(&self) -> Result<(), PluginError> {
        require_non_empty(&self.project_id, "source publication project_id")?;
        require_non_empty(&self.operation_id, "source publication operation_id")?;
        if self.commit_version < 0 {
            return Err(invalid(
                &json!(self.commit_version),
                "a nonnegative accepted commit version",
            ));
        }
        Ok(())
    }
}

fn source_value(root: &str, input: &SemanticSourceInput) -> Result<Value, PluginError> {
    serde_json::to_value(SemanticSource {
        semantic_source_id: input.semantic_source_id.clone(),
        kind: input.kind.clone(),
        name: input.name.clone(),
        root_path: input.root_path.clone(),
        root_uri: input.root_uri.clone(),
        project_root: root.into(),
    })
    .map_err(|error| {
        invalid(
            &json!(input.semantic_source_id),
            &format!("a serializable source descriptor: {error}"),
        )
    })
}

fn invalid(value: &Value, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_source_projection",
        format!("source projection `{value}`; expected {expected}"),
        false,
    )
}
