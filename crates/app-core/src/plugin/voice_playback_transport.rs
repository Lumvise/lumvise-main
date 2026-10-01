//! Bounded producer/subscriber for streamed voice-playback audio, owned by
//! App Core. `PluginHostServices::text_to_speech` (the producer) and the
//! desktop `subscribe_voice_playback` Tauri command (the one live
//! subscriber) are the only two callers; this type carries no domain
//! knowledge of either and is desktop-agnostic. Events published before a
//! subscriber has attached — headless/test hosts, or simply the startup
//! window before the renderer's long-lived subscription installs — are
//! retained in a bounded pending FIFO (sharing `subscribe`'s capacity
//! bound) and flushed, in order, to the first subscriber that attaches.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};

/// One ordered playback-transport event. Field shapes mirror
/// `lumvise_frontend_core::desktop::bridge::DesktopVoicePlaybackEvent`
/// one-to-one; the desktop bridge only ever renames this straight across.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VoicePlaybackTransportEvent {
    Opened {
        playback_id: String,
        media_type: String,
        sample_rate_hz: Option<u32>,
    },
    AudioChunk {
        playback_id: String,
        sequence: u64,
        audio: Vec<u8>,
    },
    Closed {
        playback_id: String,
    },
    Cancelled {
        playback_id: String,
    },
}

const TRANSPORT_QUEUE_CAPACITY: usize = 64;

#[derive(Debug, thiserror::Error)]
pub(crate) enum VoicePlaybackTransportError {
    #[error("voice playback transport is saturated")]
    Saturated,
}

/// Guarded transport state: the current live subscriber, if any, plus a
/// bounded FIFO of events published while no subscriber was attached (at
/// startup, headless, or between a dropped subscriber and its
/// replacement). Both share the single `TRANSPORT_QUEUE_CAPACITY` bound.
#[derive(Default)]
struct TransportState {
    sender: Option<SyncSender<VoicePlaybackTransportEvent>>,
    pending: VecDeque<VoicePlaybackTransportEvent>,
}

impl TransportState {
    /// Retains `event` in the pending FIFO, bounded by
    /// `TRANSPORT_QUEUE_CAPACITY` just like the live channel.
    fn retain_pending(
        &mut self,
        event: VoicePlaybackTransportEvent,
    ) -> Result<(), VoicePlaybackTransportError> {
        if self.pending.len() >= TRANSPORT_QUEUE_CAPACITY {
            return Err(VoicePlaybackTransportError::Saturated);
        }
        self.pending.push_back(event);
        Ok(())
    }
}

/// Single-subscriber, bounded event bus from synthesis producers to the one
/// live renderer subscription. A fresh `subscribe()` call replaces whatever
/// subscriber was previously installed — only one renderer is ever
/// attached, matching `stream_synthesize_speech`'s existing shape. Events
/// published before any subscriber has attached are buffered in a bounded
/// pending FIFO and flushed, in order, to the first subscriber.
pub(crate) struct VoicePlaybackTransport {
    state: Mutex<TransportState>,
}

impl VoicePlaybackTransport {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(TransportState::default()),
        }
    }

    /// Installs a fresh bounded subscription, flushes every event retained
    /// since the last subscriber (or since startup) to it in order, and
    /// returns its receiving end.
    pub(crate) fn subscribe(&self) -> Receiver<VoicePlaybackTransportEvent> {
        let (sender, receiver) = mpsc::sync_channel(TRANSPORT_QUEUE_CAPACITY);
        if let Ok(mut state) = self.state.lock() {
            for event in state.pending.drain(..) {
                // The fresh channel shares `TRANSPORT_QUEUE_CAPACITY` with
                // the pending FIFO it is draining, so this can never
                // observe `Full`; the receiver is still owned locally, so
                // it can never observe `Disconnected` either.
                let _ = sender.try_send(event);
            }
            state.sender = Some(sender);
        }
        receiver
    }

    /// Publishes one event. With a live subscriber attached, forwards
    /// immediately and errors rather than growing the queue if that
    /// channel is saturated (Open Risk #1 in the voice pipeline plan:
    /// bound the queue, drop-with-error rather than growing). With no
    /// subscriber currently attached (headless hosts, unit tests invoking
    /// the capability directly, or the startup window before the renderer
    /// subscribes), retains the event in the same-bounded pending FIFO
    /// instead of dropping it, erroring only once that FIFO is also full.
    pub(crate) fn publish(
        &self,
        event: VoicePlaybackTransportEvent,
    ) -> Result<(), VoicePlaybackTransportError> {
        let Ok(mut state) = self.state.lock() else {
            return Ok(());
        };
        match state.sender.take() {
            Some(sender) => match sender.try_send(event) {
                Ok(()) => {
                    state.sender = Some(sender);
                    Ok(())
                }
                Err(TrySendError::Full(_)) => {
                    state.sender = Some(sender);
                    Err(VoicePlaybackTransportError::Saturated)
                }
                Err(TrySendError::Disconnected(event)) => state.retain_pending(event),
            },
            None => state.retain_pending(event),
        }
    }
}

/// Parses `rate=<n>` out of a PCM media-type string
/// (`"audio/pcm;rate=24000;format=s16le"`), used only to fill the
/// informational `Opened.sample_rate_hz` field.
pub(crate) fn sample_rate_from_media_type(media_type: &str) -> Option<u32> {
    media_type
        .split(';')
        .find_map(|part| part.trim().strip_prefix("rate="))
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_before_subscribe_is_retained_and_delivered_in_order() {
        let transport = VoicePlaybackTransport::new();
        transport
            .publish(VoicePlaybackTransportEvent::Opened {
                playback_id: "p".into(),
                media_type: "audio/pcm;rate=24000".into(),
                sample_rate_hz: Some(24_000),
            })
            .unwrap();
        transport
            .publish(VoicePlaybackTransportEvent::AudioChunk {
                playback_id: "p".into(),
                sequence: 0,
                audio: vec![1, 2, 3],
            })
            .unwrap();
        transport
            .publish(VoicePlaybackTransportEvent::Closed {
                playback_id: "p".into(),
            })
            .unwrap();

        let receiver = transport.subscribe();
        assert_eq!(
            receiver.recv().unwrap(),
            VoicePlaybackTransportEvent::Opened {
                playback_id: "p".into(),
                media_type: "audio/pcm;rate=24000".into(),
                sample_rate_hz: Some(24_000),
            }
        );
        assert_eq!(
            receiver.recv().unwrap(),
            VoicePlaybackTransportEvent::AudioChunk {
                playback_id: "p".into(),
                sequence: 0,
                audio: vec![1, 2, 3],
            }
        );
        assert_eq!(
            receiver.recv().unwrap(),
            VoicePlaybackTransportEvent::Closed {
                playback_id: "p".into(),
            }
        );
    }

    #[test]
    fn publish_before_subscribe_saturates_at_capacity() {
        let transport = VoicePlaybackTransport::new();
        for sequence in 0..TRANSPORT_QUEUE_CAPACITY as u64 {
            transport
                .publish(VoicePlaybackTransportEvent::AudioChunk {
                    playback_id: "p".into(),
                    sequence,
                    audio: vec![0],
                })
                .unwrap();
        }
        let overflow = transport.publish(VoicePlaybackTransportEvent::AudioChunk {
            playback_id: "p".into(),
            sequence: TRANSPORT_QUEUE_CAPACITY as u64,
            audio: vec![0],
        });
        assert!(matches!(
            overflow,
            Err(VoicePlaybackTransportError::Saturated)
        ));

        // The buffered events are unaffected by the failed overflow
        // attempt and are still flushed to the first subscriber in order.
        let receiver = transport.subscribe();
        for sequence in 0..TRANSPORT_QUEUE_CAPACITY as u64 {
            assert_eq!(
                receiver.recv().unwrap(),
                VoicePlaybackTransportEvent::AudioChunk {
                    playback_id: "p".into(),
                    sequence,
                    audio: vec![0],
                }
            );
        }
    }

    #[test]
    fn publish_forwards_to_the_current_subscriber() {
        let transport = VoicePlaybackTransport::new();
        let receiver = transport.subscribe();
        transport
            .publish(VoicePlaybackTransportEvent::AudioChunk {
                playback_id: "p".into(),
                sequence: 0,
                audio: vec![1, 2, 3],
            })
            .unwrap();
        assert_eq!(
            receiver.recv().unwrap(),
            VoicePlaybackTransportEvent::AudioChunk {
                playback_id: "p".into(),
                sequence: 0,
                audio: vec![1, 2, 3],
            }
        );
    }

    #[test]
    fn publish_errors_when_the_subscriber_is_saturated() {
        let transport = VoicePlaybackTransport::new();
        let _receiver = transport.subscribe();
        for sequence in 0..TRANSPORT_QUEUE_CAPACITY as u64 {
            transport
                .publish(VoicePlaybackTransportEvent::AudioChunk {
                    playback_id: "p".into(),
                    sequence,
                    audio: vec![0],
                })
                .unwrap();
        }
        let overflow = transport.publish(VoicePlaybackTransportEvent::AudioChunk {
            playback_id: "p".into(),
            sequence: TRANSPORT_QUEUE_CAPACITY as u64,
            audio: vec![0],
        });
        assert!(overflow.is_err());
    }

    #[test]
    fn sample_rate_parses_from_pcm_media_type() {
        assert_eq!(
            sample_rate_from_media_type("audio/pcm;rate=24000;format=s16le"),
            Some(24_000)
        );
        assert_eq!(sample_rate_from_media_type("audio/wav"), None);
    }
}
