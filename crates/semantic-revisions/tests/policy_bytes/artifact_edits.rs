//! The same byte protocol preserves working tokens and explicit checkpoint decisions in WebAssembly.
use super::*;

pub(super) fn cases() -> Vec<Vec<u8>> {
    let mut project = ProjectFixture::new();
    project.initial();
    let create = edit(&project, "create-note", false, "Initial");
    project.publish(&create);
    [false, true]
        .into_iter()
        .map(|checkpoint| {
            let request = edit(&project, "edit-note", checkpoint, "Changed");
            let basis = SemanticRevisionPublicationBasis {
                structure_coverage: SemanticRevisionStructureCoverage::None,
                artifact_ids: vec!["note".into()],
                facts: project.state.clone(),
            };
            serde_json::to_vec(
                &json!({"operation":"publication", "basis":basis, "request":request,
            "timestamp":timestamp(5000), "next_commit":project.state.commit_version+1}),
            )
            .unwrap()
        })
        .collect()
}

fn edit(
    project: &ProjectFixture,
    operation: &str,
    checkpoint: bool,
    content: &str,
) -> SemanticRevisionPublication {
    let mut request = project.edit(operation, vec![]);
    request.structure = None;
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint,
        artifact: artifact("note", "a", content),
        media_type: "text/plain".into(),
    });
    request
}
