//! The staged-vector exception belongs only to the innermost derived rebuild.
use super::*;
use lumvise_db_core::{
    ElementTextVectorGenerationKey, ElementTextVectorGenerationSpec, SemanticOperation,
};

fn generation_writes() -> Vec<SemanticOperation> {
    let key = ElementTextVectorGenerationKey {
        project_root: "/repo".into(),
        semantic_element_id: "section".into(),
        generation_id: "run".into(),
    };
    let spec = ElementTextVectorGenerationSpec {
        key: key.clone(),
        content_fingerprint: "source-fingerprint".into(),
        engine_id: "embed".into(),
        model: Some("nomic".into()),
        profile_fingerprint: "profile".into(),
        dimensions: 2,
        chunk_count: 1,
        chunk_plan_fingerprint: "plan".into(),
    };
    vec![
        SemanticOperation::BeginElementTextVectorGeneration { spec },
        SemanticOperation::StageElementTextVectorChunk {
            key: key.clone(),
            chunk: 0,
            vector: vec![0.0, 1.0],
        },
        SemanticOperation::CommitElementTextVectorGeneration { key: key.clone() },
        SemanticOperation::AbortElementTextVectorGeneration { key },
    ]
}

fn rebuild_origin(context: &PluginInvocationContext) -> PluginInvocationContext {
    PluginInvocationContext::new(
        "stage",
        "agent",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_mcp_origin(context.mcp_origin().unwrap().clone())
    .enter_plugin("builtin.semantic", "rebuild_search_index")
}

#[test]
fn only_innermost_rebuild_may_publish_or_abort_a_generation() {
    let app = AppCore::in_memory().unwrap();
    let (_, context) = connected(&app);
    let rebuild = rebuild_origin(&context);
    let nested = rebuild
        .clone()
        .enter_plugin("builtin.canvas", "canvas_save");
    let authority = app.connector_authority();
    for operation in generation_writes() {
        assert!(
            authority
                .authorize_connector_semantic(&rebuild, &operation)
                .is_ok()
        );
        assert!(
            authority
                .authorize_connector_semantic(&context, &operation)
                .is_err()
        );
        assert!(
            authority
                .authorize_connector_semantic(&nested, &operation)
                .is_err()
        );
        assert_eq!(operation.project_root(), Some("/repo"));
    }
}
