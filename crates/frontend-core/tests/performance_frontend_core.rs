use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use lumvise_frontend_core::{
    FrontendCore, OrbMode, VoicePlaybackChunk, VoicePlaybackStatus, WorkArea,
};

const OPERATIONS: usize = 32;
const WORKERS: usize = 4;

#[test]
#[ignore = "raw release performance evidence; run explicitly"]
fn frontend_core_snapshot_samples_single_and_parallel() {
    assert_eq!(OPERATIONS % WORKERS, 0);

    let mut serial_core = seeded_core();
    let serial_started = Instant::now();
    let mut serial_samples = Vec::with_capacity(OPERATIONS);
    for operation in 0..OPERATIONS {
        let sample_started = Instant::now();
        let status = if operation % 2 == 0 {
            VoicePlaybackStatus::Playing
        } else {
            VoicePlaybackStatus::Queued
        };
        let snapshot = serial_core
            .set_voice_playback_status("benchmark-playback", status)
            .expect("serial snapshot operation succeeds");
        assert_eq!(snapshot.state.voice_playback.segments.len(), 1);
        assert_eq!(snapshot.state.orb.mode, orb_mode_for(status));
        serial_samples.push(sample_started.elapsed().as_nanos());
    }
    let serial_total = serial_started.elapsed().as_nanos();
    assert_eq!(serial_samples.len(), OPERATIONS);
    println!(
        "{}",
        serde_json::json!({
            "scenario": "single",
            "buffered_audio_bytes": 0,
            "workers": 1,
            "operations": OPERATIONS,
            "total_elapsed_ns": serial_total,
            "per_operation_ns": serial_total / OPERATIONS as u128,
            "samples": serial_samples,
        })
    );

    let parallel_core = Arc::new(Mutex::new(seeded_core()));
    let parallel_started = Instant::now();
    let handles = (0..WORKERS)
        .map(|worker| {
            let core = Arc::clone(&parallel_core);
            thread::spawn(move || {
                let operations = OPERATIONS / WORKERS;
                let mut samples = Vec::with_capacity(operations);
                for operation in 0..operations {
                    let sample_started = Instant::now();
                    let status = if (worker + operation) % 2 == 0 {
                        VoicePlaybackStatus::Playing
                    } else {
                        VoicePlaybackStatus::Queued
                    };
                    let snapshot = core
                        .lock()
                        .expect("desktop-style frontend core mutex is not poisoned")
                        .set_voice_playback_status("benchmark-playback", status)
                        .expect("parallel snapshot operation succeeds");
                    assert_eq!(snapshot.state.voice_playback.segments.len(), 1);
                    assert_eq!(snapshot.state.orb.mode, orb_mode_for(status));
                    samples.push(sample_started.elapsed().as_nanos());
                }
                samples
            })
        })
        .collect::<Vec<_>>();
    let parallel_samples = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("parallel worker succeeds"))
        .collect::<Vec<_>>();
    let parallel_total = parallel_started.elapsed().as_nanos();
    assert_eq!(parallel_samples.len(), OPERATIONS);
    println!(
        "{}",
        serde_json::json!({
            "scenario": "parallel",
            "buffered_audio_bytes": 0,
            "workers": WORKERS,
            "operations": OPERATIONS,
            "total_elapsed_ns": parallel_total,
            "per_operation_ns": parallel_total / OPERATIONS as u128,
            "samples": parallel_samples,
        })
    );
}

fn seeded_core() -> FrontendCore {
    let mut core = FrontendCore::default();
    core.spawn_app(WorkArea::new(0, 0, 1440, 900, 1.0))
        .expect("benchmark app spawn succeeds");
    core.open_voice_playback("benchmark-playback")
        .expect("benchmark playback opens");
    core.append_voice_playback_chunk(VoicePlaybackChunk {
        playback_id: "benchmark-playback".to_string(),
        media_type: "audio/pcm;rate=24000;format=s16le".to_string(),
    })
    .expect("benchmark playback segment seed succeeds");
    core
}

fn orb_mode_for(status: VoicePlaybackStatus) -> OrbMode {
    match status {
        VoicePlaybackStatus::Queued => OrbMode::Session,
        VoicePlaybackStatus::Playing => OrbMode::Activity,
        VoicePlaybackStatus::Completed | VoicePlaybackStatus::Cancelled => OrbMode::Idle,
        VoicePlaybackStatus::Failed => OrbMode::Error,
    }
}
