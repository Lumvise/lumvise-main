//! Capture artifact acceptance before the existing journal compacts its frozen proposal.
use super::*;

pub(super) fn capture_artifact(publication: &Value, output: &mut Value) -> Result<(), PluginError> {
    if output["publication"]["outcome"] != "accepted" {
        return Ok(());
    }
    let receipt = &mut output["publication"]["value"];
    let (artifact_id, revision) = artifact_acceptance(publication, receipt)?;
    receipt["artifact_revision"] =
        json!({"project_root":publication["project_root"],"artifact_id":artifact_id});
    receipt["lifecycle_revision"] = json!(revision);
    Ok(())
}

fn artifact_acceptance(
    publication: &Value,
    receipt: &Value,
) -> Result<(String, Option<u64>), PluginError> {
    let artifacts = publication["artifacts"]
        .as_array()
        .ok_or_else(|| invalid_frozen_publication(publication, "frozen artifact revisions"))?;
    let removed = publication["removed_artifact_ids"]
        .as_array()
        .ok_or_else(|| invalid_frozen_publication(publication, "frozen removed artifact IDs"))?;
    match (artifacts.as_slice(), removed.as_slice()) {
        ([body], []) => accepted_body(&body["artifact"], receipt),
        ([], [removed]) => {
            let id = removed
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| {
                    invalid_frozen_publication(removed, "one exact removed artifact ID")
                })?;
            Ok((id.into(), None))
        }
        _ => Err(invalid_frozen_publication(
            publication,
            "one Knowledge artifact write or removal",
        )),
    }
}

fn accepted_body(body: &Value, receipt: &Value) -> Result<(String, Option<u64>), PluginError> {
    let incoming = body["artifact_id"]
        .as_str()
        .ok_or_else(|| invalid_frozen_publication(body, "the frozen incoming artifact ID"))?;
    let accepted = crate::artifact_publication::accepted_artifact_id(incoming, Some(receipt))?;
    let revision = body["metadata"]["knowledge"]["lifecycle"]["revision"]
        .as_u64()
        .ok_or_else(|| {
            invalid_frozen_publication(body, "the original nonnegative artifact lifecycle counter")
        })?;
    Ok((accepted.into(), Some(revision)))
}
