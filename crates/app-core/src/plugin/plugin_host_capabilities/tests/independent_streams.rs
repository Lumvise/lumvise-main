use super::*;

struct HeldWorkspaceSpeech {
    entered: Sender<String>,
    release: Mutex<Receiver<()>>,
}

impl SpeechSynthesizer for HeldWorkspaceSpeech {
    fn warmup(&self) -> lumvise_neural_core::Result<()> {
        Ok(())
    }
    fn synthesize(
        &self,
        _: &Text2VoiceRequest,
        _: &InvocationControl,
    ) -> lumvise_neural_core::Result<Text2VoiceResponse> {
        unreachable!("the caller must use streamed speech")
    }
    fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
        events: &mut Text2VoiceStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        self.entered.send(request.text.clone()).unwrap();
        if request.text == "hold" {
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }
        StreamingSynthesizer.stream_with_events(request, control, events)
    }
}

fn speak(services: &PluginHostServices, session: &str, playback: &str, text: &str) {
    services
        .invoke(
            "builtin.assistant",
            TEXT_TO_SPEECH,
            json!({
                "session_id": session, "playback_id": playback, "text": text, "close": true
            }),
        )
        .unwrap();
}

#[test]
fn floating_synthesis_starts_while_workspace_speech_is_still_running() {
    let services = services();
    let (entered, entries) = mpsc::channel();
    let (release, held) = mpsc::channel();
    services.install_speech_synthesizer(Arc::new(HeldWorkspaceSpeech {
        entered,
        release: Mutex::new(held),
    }));
    speak(&services, "workspace:voice", "workspace-turn", "hold");
    assert_eq!(
        entries.recv_timeout(Duration::from_secs(1)).unwrap(),
        "hold"
    );
    speak(&services, "floating", "floating-turn", "Assistant response");
    let parallel = entries.recv_timeout(Duration::from_millis(250));
    release.send(()).unwrap();
    assert_eq!(parallel.unwrap(), "Assistant response");
}

#[test]
fn workspace_playback_does_not_replace_dashboard_state_or_drop_its_completion() {
    let services = services();
    services.install_speech_synthesizer(Arc::new(StreamingSynthesizer));
    speak(&services, "floating", "floating-turn", "Assistant response");
    speak(
        &services,
        "workspace:voice",
        "workspace-turn",
        "Workspace response",
    );
    assert_eq!(
        services
            .frontend
            .lock()
            .unwrap()
            .voice_playback_status()
            .active_playback_id
            .as_deref(),
        Some("floating-turn")
    );
    for (playback, session) in [
        ("floating-turn", "floating"),
        ("workspace-turn", "workspace:voice"),
    ] {
        let owner = services
            .report_voice_playback_status(playback, VoicePlaybackStatus::Completed)
            .unwrap();
        assert_eq!(
            owner.map(|(_, session, _)| session).as_deref(),
            Some(session)
        );
    }
}

#[test]
fn workspace_segments_and_cancellation_leave_floating_playback_intact() {
    let services = services();
    services.install_speech_synthesizer(Arc::new(StreamingSynthesizer));
    speak(&services, "floating", "floating-turn", "Assistant response");
    for index in 0..2 {
        let response = services.invoke("builtin.assistant", TEXT_TO_SPEECH, json!({
            "session_id": "workspace:voice", "playback_id": "workspace-turn", "text": "Segment", "close": false
        })).unwrap();
        assert_eq!(response["segment_index"], index);
    }
    services
        .invoke(
            "builtin.assistant",
            TEXT_TO_SPEECH,
            json!({
                "session_id": "workspace:voice", "playback_id": "workspace-turn", "cancel": true
            }),
        )
        .unwrap();
    assert!(
        services
            .report_voice_playback_status("workspace-turn", VoicePlaybackStatus::Completed)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        services
            .report_voice_playback_status("floating-turn", VoicePlaybackStatus::Completed)
            .unwrap()
            .unwrap()
            .1,
        "floating"
    );
}

#[test]
fn four_held_workspace_llm_requests_leave_an_independent_floating_slot() {
    let (entered, entries) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let providers =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(HeldLaneProvider {
            entered,
            release: Mutex::new(held),
        })])
        .unwrap()
        .with_completion_concurrency("lane-test", 4);
    let services = PluginHostServices::new(FrontendCore::default(), providers, test_persistence());
    let jobs = (0..4)
        .map(|index| {
            let services = Arc::clone(&services);
            std::thread::spawn(move || {
                let mut request = lane_request("hold");
                request["conversation_id"] = json!(format!("workspace:{index}"));
                services.invoke("builtin.assistant", NEURAL_LLM, request)
            })
        })
        .collect::<Vec<_>>();
    for _ in 0..4 {
        expect_lane_entry(&entries, "hold");
    }
    let floating_services = Arc::clone(&services);
    let floating = std::thread::spawn(move || {
        floating_services.invoke("builtin.assistant", NEURAL_LLM, lane_request("floating"))
    });
    let parallel = entries.recv_timeout(Duration::from_millis(250));
    for _ in 0..4 {
        release.send(()).unwrap();
    }
    for job in jobs {
        job.join().unwrap().unwrap();
    }
    floating.join().unwrap().unwrap();
    assert_eq!(parallel.unwrap(), "floating");
}
