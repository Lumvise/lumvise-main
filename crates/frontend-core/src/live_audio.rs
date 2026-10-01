//! Desktop live-audio input protocol v1. Public `LiveAudioInput` carries scoped
//! PCM/control frames; remote-provider protocols remain in Neural Core.
use prost::Message;

/// Protobuf v1 microphone/control frame. Example: decode a raw Tauri request body.
#[derive(Clone, PartialEq, Message)]
pub struct LiveAudioInput {
    #[prost(string, tag = "1")]
    pub session_id: String,
    #[prost(uint64, tag = "2")]
    pub session_epoch: u64,
    #[prost(bytes = "vec", tag = "3")]
    pub pcm: Vec<u8>,
    /// 0: PCM16 mono 24kHz, 1: pause, 2: resume, 3: interrupt, 4: playback stopped, 5: utterance ended.
    #[prost(uint32, tag = "4")]
    pub control: u32,
    #[prost(uint64, tag = "5")]
    pub played_ms: u64,
    #[prost(string, tag = "6")]
    pub playback_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_microphone_frame_round_trips_with_renderer_fixture() {
        let frame = LiveAudioInput {
            session_id: "s".into(),
            session_epoch: 1,
            pcm: vec![0, 128, 255, 127],
            control: 0,
            played_ms: 0,
            playback_id: String::new(),
        };
        let bytes = vec![10, 1, 115, 16, 1, 26, 4, 0, 128, 255, 127];
        assert_eq!(frame.encode_to_vec(), bytes);
        assert_eq!(LiveAudioInput::decode(bytes.as_slice()).unwrap(), frame);
    }
    #[test]
    fn playback_interruption_round_trips_with_renderer_fixture() {
        let frame = LiveAudioInput {
            session_id: "s".into(),
            session_epoch: 1,
            control: 4,
            played_ms: 237,
            playback_id: "p".into(),
            ..Default::default()
        };
        let bytes = vec![10, 1, 115, 16, 1, 32, 4, 40, 237, 1, 50, 1, 112];
        assert_eq!(frame.encode_to_vec(), bytes);
        assert_eq!(LiveAudioInput::decode(bytes.as_slice()).unwrap(), frame);
    }
    #[test]
    fn truncated_frame_is_rejected() {
        assert!(LiveAudioInput::decode(&[26, 4, 0][..]).is_err());
    }
}
