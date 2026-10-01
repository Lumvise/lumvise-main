use lumvise_neural_core::llm_providers::{
    LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest,
};

#[test]
fn request_serializes_screen_frame_inputs_as_first_class_modalities() {
    let request = LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "what is visible?".to_string(),
        }],
        stream: true,
        provider_id: Some("gemini".to_string()),
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: vec![LlmModalityInput {
            input_id: "frame-1".to_string(),
            kind: LlmModalityInputKind::ScreenFrame,
            media_type: "image/png".to_string(),
            bytes: vec![137, 80, 78, 71],
            metadata: serde_json::json!({ "source": "dashboard" }),
        }],
    };

    let encoded = serde_json::to_value(&request).unwrap();

    assert_eq!(
        encoded["modality_inputs"][0]["kind"],
        serde_json::json!("screen_frame")
    );
    assert_eq!(
        request.modality_inputs[0].kind,
        LlmModalityInputKind::ScreenFrame
    );
}
