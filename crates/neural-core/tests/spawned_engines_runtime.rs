use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::text2vector::Text2VectorRequest;
use lumvise_neural_core::text2voice::{Text2VoiceRequest, Text2VoiceStreamEvent};
use lumvise_neural_core::voice2text::{Voice2TextRequest, Voice2TextStreamEvent};
use lumvise_neural_core::{
    EngineConfig, SpawnConfig, Text2VectorService, Text2VoiceService, Voice2TextService,
};

const FIXTURE: &str = env!("CARGO_BIN_EXE_spawned-engine-fixture");

#[test]
fn vector_process_round_trip_preserves_model_and_dimensions() {
    let service = Text2VectorService::new(engine_config("embed", "standard", Some(2))).unwrap();
    let response = service
        .embed(&Text2VectorRequest {
            text: "vectorize this".into(),
            model: Some("fixture-vector-v2".into()),
        })
        .unwrap();

    assert_eq!(response.vector, vec![0.4, 0.5]);
    assert_eq!(response.dimensions, 2);
    assert_eq!(
        response.metadata.model.as_deref(),
        Some("fixture-vector-v2")
    );
}

#[test]
fn vector_dimension_mismatch_is_rejected_after_process_round_trip() {
    let service = Text2VectorService::new(engine_config("embed", "standard", Some(3))).unwrap();
    let error = service.embed(&vector_request()).unwrap_err().to_string();

    assert!(error.contains("vector dimensions 3"));
    assert!(error.contains("2"));
}

#[test]
fn malformed_vector_dimensions_are_rejected_at_process_boundary() {
    let service =
        Text2VectorService::new(engine_config("embed", "malformed-vector", None)).unwrap();
    let error = service.embed(&vector_request()).unwrap_err().to_string();

    assert!(error.contains("vector length matching dimensions 99"));
}

#[test]
fn large_vector_crosses_dynamic_frame_without_transport_limit() {
    let service = Text2VectorService::new(engine_config("embed", "standard", None)).unwrap();
    let response = service
        .embed(&Text2VectorRequest {
            text: "large-vector".into(),
            model: None,
        })
        .unwrap();

    assert_eq!(response.vector.len(), 100_000);
    assert_eq!(response.dimensions, 100_000);
}

#[test]
fn text2voice_process_round_trip_keeps_audio_binary() {
    let service = Text2VoiceService::new(engine_config("tts", "standard", None)).unwrap();
    let response = service.synthesize(&voice_request()).unwrap();

    assert_eq!(response.audio, vec![1, 2, 3, 0]);
    assert_eq!(response.media_type, "audio/wav");
    assert_eq!(response.sample_rate_hz, Some(24_000));
    assert_eq!(response.metadata.model.as_deref(), Some("fixture-voice-v2"));
}

#[test]
fn text2voice_stream_preserves_chunk_order_and_completion() {
    let service = Text2VoiceService::new(engine_config("tts", "standard", None)).unwrap();
    let events = service
        .stream(&voice_request(), StreamControl::unbounded())
        .unwrap();

    assert!(matches!(
        events[0],
        Text2VoiceStreamEvent::AudioChunk { sequence: 1, .. }
    ));
    assert!(matches!(
        events[1],
        Text2VoiceStreamEvent::AudioChunk { sequence: 2, .. }
    ));
    assert_eq!(events[2], Text2VoiceStreamEvent::Complete);
}

#[test]
fn text2voice_stream_cancellation_terminates_before_completion() {
    let service = Text2VoiceService::new(engine_config("tts", "standard", None)).unwrap();
    let events = service
        .stream(&voice_request(), StreamControl::cancel_after(1))
        .unwrap();

    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        Text2VoiceStreamEvent::AudioChunk { sequence: 1, .. }
    ));
}

#[test]
fn voice2text_process_round_trip_keeps_audio_binary() {
    let service = Voice2TextService::new(engine_config("stt", "standard", None)).unwrap();
    let response = service.transcribe(&transcript_request()).unwrap();

    assert_eq!(response.transcript, "fixture transcript");
    assert_eq!(response.language.as_deref(), Some("en"));
    assert_eq!(response.confidence, Some(0.92));
    assert_eq!(response.metadata.model.as_deref(), Some("fixture-stt-v2"));
}

#[test]
fn voice2text_stream_preserves_chunk_order_and_completion() {
    let service = Voice2TextService::new(engine_config("stt", "standard", None)).unwrap();
    let events = service
        .stream(&transcript_request(), StreamControl::unbounded())
        .unwrap();

    assert!(matches!(
        events[0],
        Voice2TextStreamEvent::TranscriptChunk { sequence: 1, .. }
    ));
    assert!(matches!(
        events[1],
        Voice2TextStreamEvent::TranscriptChunk { sequence: 2, .. }
    ));
    assert_eq!(events[2], Voice2TextStreamEvent::Complete);
}

#[test]
fn malformed_protobuf_is_rejected_through_public_service() {
    let service = Text2VoiceService::new(engine_config("tts", "malformed", None)).unwrap();
    let error = service
        .synthesize(&voice_request())
        .unwrap_err()
        .to_string();

    assert!(error.contains("Protobuf envelope"));
}

#[test]
fn structured_process_failure_is_reported_through_public_service() {
    let service = Voice2TextService::new(engine_config("stt", "failure", None)).unwrap();
    let error = service
        .transcribe(&transcript_request())
        .unwrap_err()
        .to_string();

    assert!(error.contains("fixture provider failure"));
}

fn engine_config(engine_id: &str, mode: &str, dimensions: Option<usize>) -> EngineConfig {
    EngineConfig {
        engine_id: engine_id.into(),
        spawn: SpawnConfig {
            command: FIXTURE.into(),
            args: vec!["--mode".into(), mode.into()],
            timeout_ms: 1,
        },
        expected_dimensions: dimensions,
    }
}

fn vector_request() -> Text2VectorRequest {
    Text2VectorRequest {
        text: "vectorize this".into(),
        model: None,
    }
}

fn voice_request() -> Text2VoiceRequest {
    Text2VoiceRequest {
        text: "hello voice".into(),
        voice_id: Some("alloy".into()),
        model: Some("fixture-voice-v2".into()),
    }
}

fn transcript_request() -> Voice2TextRequest {
    Voice2TextRequest {
        audio: vec![0, 1, 2, 255],
        media_type: "audio/wav".into(),
        model: Some("fixture-stt-v2".into()),
    }
}
