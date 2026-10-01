use super::*;

#[test]
fn default_english_uses_default_quality_settings() {
    let config = WhisperRsVoice2TextConfig::default_english("whisper");

    assert_eq!(config.model_path, "large-v3-turbo");
    assert_eq!(config.beam_size, 7);
    assert_eq!(config.decode, WhisperRsDecodeSettings::default_quality());
}

#[test]
fn default_quality_matches_expected_whisper_values() {
    let settings = WhisperRsDecodeSettings::default_quality();

    let prompt = settings.initial_prompt.unwrap();
    assert!(prompt.contains("literally as possible"));
    assert!(prompt.contains("commands"));
    assert_eq!(settings.temperature, 0.2);
    assert!(!settings.translate);
    assert!(settings.no_timestamps);
    assert!(!settings.detect_language);
    assert!(settings.suppress_blank);
    assert_eq!(settings.no_speech_threshold, 0.7);
    assert_eq!(settings.audio_ctx, 0);
    assert_eq!(settings.max_len, 0);
}

#[test]
fn validate_decode_settings_rejects_invalid_temperature() {
    let mut settings = WhisperRsDecodeSettings::default_quality();
    settings.temperature = 1.5;

    let error = validate_decode_settings(&settings).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("temperature between 0.0 and 1.0")
    );
}

#[test]
fn validate_decode_settings_rejects_invalid_no_speech_threshold() {
    let mut settings = WhisperRsDecodeSettings::default_quality();
    settings.no_speech_threshold = -0.1;

    let error = validate_decode_settings(&settings).unwrap_err();

    assert!(error.to_string().contains("no-speech threshold"));
}

#[test]
fn decode_wav_audio_reads_16_bit_mono_pcm() {
    let bytes = wav_bytes(1, 16, &[0, 0, 255, 127]);

    let decoded = decode_wav_audio(&bytes).unwrap();

    assert_eq!(decoded.channels, 1);
    assert_eq!(decoded.sample_rate_hz, WHISPER_SAMPLE_RATE_HZ);
    assert_eq!(decoded.samples, vec![0.0, 1.0]);
}

#[test]
fn decoded_stereo_audio_mixes_to_mono_samples() {
    let decoded = DecodedWavAudio {
        samples: vec![1.0, 0.0, 0.5, -0.5],
        channels: 2,
        sample_rate_hz: WHISPER_SAMPLE_RATE_HZ,
    };

    assert_eq!(decoded.into_whisper_samples().unwrap(), vec![0.5, 0.0]);
}

#[test]
fn decode_wav_audio_rejects_wrong_sample_rate() {
    let decoded = DecodedWavAudio {
        samples: vec![0.0],
        channels: 1,
        sample_rate_hz: 44_100,
    };

    let error = decoded.into_whisper_samples().unwrap_err();

    assert!(error.to_string().contains("16kHz"));
}

#[test]
fn decode_wav_audio_rejects_misaligned_sample_data() {
    let bytes = wav_bytes(1, 16, &[0]);

    let error = decode_wav_audio(&bytes).unwrap_err();

    assert!(error.to_string().contains("aligned to sample width"));
}

#[test]
fn validate_request_rejects_non_wav_media_type() {
    let request = Voice2TextRequest {
        audio: vec![1, 2, 3],
        media_type: "audio/webm".to_string(),
        model: None,
    };

    let error = validate_request(&request).unwrap_err();

    assert!(error.to_string().contains("WAV media type"));
}

#[test]
fn transcript_events_preserve_segment_sequence_and_text() {
    let segments = vec![json!({ "sequence": 7, "text": "hello" })];

    let events = transcript_events(&segments);

    assert_eq!(
        events,
        vec![Voice2TextStreamEvent::TranscriptChunk {
            sequence: 7,
            text: "hello".to_string(),
            is_final: true,
        }]
    );
}

fn wav_bytes(channels: u16, bits_per_sample: u16, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_wav_header(&mut bytes, channels, bits_per_sample, data.len() as u32);
    bytes.extend_from_slice(data);
    bytes
}

fn push_wav_header(bytes: &mut Vec<u8>, channels: u16, bits_per_sample: u16, data_size: u32) {
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&WHISPER_SAMPLE_RATE_HZ.to_le_bytes());
    bytes.extend_from_slice(&byte_rate(channels, bits_per_sample).to_le_bytes());
    bytes.extend_from_slice(&block_align(channels, bits_per_sample).to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
}

fn byte_rate(channels: u16, bits_per_sample: u16) -> u32 {
    WHISPER_SAMPLE_RATE_HZ * u32::from(block_align(channels, bits_per_sample))
}

fn block_align(channels: u16, bits_per_sample: u16) -> u16 {
    channels * bits_per_sample / 8
}
