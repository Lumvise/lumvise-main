use lumvise_frontend_core::{
    AppLifecycle, AppSettings, AppSettingsPatch, AudioDeviceCatalog, AudioDeviceOption,
    COMPACT_WIDGET_SIZE, CanvasElement, CanvasPatch, DashboardView, DashboardVisibility,
    FrontendCore, HOST_MINIMIZE_COLLAPSE_SCRIPT, ModalityStreamKind, ModalityStreamPhase, OrbMode,
    OrbVisibility, SHELL_HEIGHT, SHELL_WIDTH, SurfaceMode, VoiceAudioChunk, VoicePlaybackChunk,
    VoicePlaybackStatus, VoiceRecordingStatus, WhiteboardSurface, WidgetBounds, WindowDisplayState,
    WindowLayout, WindowManagementCommand, WorkArea, clamp_widget_bounds,
    compact_bounds_for_collapse, settings_window_bounds, widget_bounds_for_mode,
    work_area_for_bounds, work_area_for_point,
};
use serde_json::json;

fn default_work_area() -> WorkArea {
    WorkArea::new(0, 0, 1440, 900, 1.0)
}

#[test]
fn spawn_app_creates_compact_orb_window() {
    let mut core = FrontendCore::default();
    let snapshot = core.spawn_app(default_work_area()).unwrap();

    assert_eq!(snapshot.state.app.lifecycle, AppLifecycle::Spawned);
    assert_eq!(snapshot.state.app.surface_mode, SurfaceMode::Compact);
    assert_eq!(snapshot.state.orb.mode, OrbMode::Idle);
    assert_eq!(
        snapshot.window_layout.active_bounds.width,
        COMPACT_WIDGET_SIZE
    );
}

#[test]
fn spawn_app_rejects_empty_work_area() {
    let mut core = FrontendCore::default();
    let error = core.spawn_app(WorkArea::new(0, 0, 0, 0, 1.0)).unwrap_err();

    assert!(error.to_string().contains("non-empty work area"));
    assert!(core.window_layout().is_none());
}

#[test]
fn clicking_orb_opens_dashboard_shell() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let snapshot = core.click_orb().unwrap();

    assert_eq!(
        snapshot.state.dashboard.visibility,
        DashboardVisibility::Visible
    );
    assert_eq!(snapshot.state.app.surface_mode, SurfaceMode::Shell);
    assert_eq!(snapshot.window_layout.active_bounds.width, SHELL_WIDTH);
    assert_eq!(snapshot.window_layout.active_bounds.height, SHELL_HEIGHT);
    assert_eq!(
        snapshot.window_plan.display_state,
        WindowDisplayState::DashboardShell
    );
}

#[test]
fn closing_dashboard_returns_to_idle_compact_orb() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();
    core.click_orb().unwrap();

    let snapshot = core.close_dashboard().unwrap();

    assert_eq!(
        snapshot.state.dashboard.visibility,
        DashboardVisibility::Hidden
    );
    assert_eq!(snapshot.state.orb.mode, OrbMode::Idle);
    assert_eq!(snapshot.window_layout.surface_mode, SurfaceMode::Compact);
    assert_eq!(
        snapshot.window_plan.display_state,
        WindowDisplayState::CompactOrb
    );
}

#[test]
fn frontend_status_reports_orb_whiteboard_and_window_state() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();
    core.set_whiteboard_surface(WhiteboardSurface::Shell)
        .unwrap();

    let status = core.frontend_status();

    assert_eq!(status.lifecycle, AppLifecycle::Spawned);
    assert_eq!(status.orb.visibility, OrbVisibility::Visible);
    assert_eq!(status.whiteboard.surface, WhiteboardSurface::Shell);
    assert_eq!(status.surface_mode, SurfaceMode::Shell);
    assert_eq!(
        status.window_layout.unwrap().surface_mode,
        SurfaceMode::Shell
    );
    assert_eq!(
        status.window_plan.unwrap().display_state,
        WindowDisplayState::DashboardShell
    );
}

#[test]
fn orb_visibility_endpoint_controls_hidden_state() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let hidden = core.set_orb_visibility(OrbVisibility::Hidden).unwrap();
    let visible = core.set_orb_visibility(OrbVisibility::Visible).unwrap();

    assert_eq!(hidden.state.orb.visibility, OrbVisibility::Hidden);
    assert_eq!(visible.state.orb.visibility, OrbVisibility::Visible);
}

#[test]
fn orb_mode_endpoint_controls_non_countdown_status() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();
    core.start_countdown(3).unwrap();

    let snapshot = core.set_orb_mode(OrbMode::Session).unwrap();

    assert_eq!(snapshot.state.orb.mode, OrbMode::Session);
    assert_eq!(snapshot.state.orb.countdown_digit, None);
}

#[test]
fn orb_mode_endpoint_rejects_countdown_without_digit() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let error = core.set_orb_mode(OrbMode::Countdown).unwrap_err();

    assert!(error.to_string().contains("start_countdown"));
}

#[test]
fn whiteboard_surface_endpoint_controls_shell_fullscreen_and_hidden() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let shell = core
        .set_whiteboard_surface(WhiteboardSurface::Shell)
        .unwrap();
    let fullscreen = core
        .set_whiteboard_surface(WhiteboardSurface::Fullscreen)
        .unwrap();
    let hidden = core
        .set_whiteboard_surface(WhiteboardSurface::Hidden)
        .unwrap();

    assert_eq!(shell.window_layout.surface_mode, SurfaceMode::Shell);
    assert_eq!(
        fullscreen.window_layout.surface_mode,
        SurfaceMode::Fullscreen
    );
    assert_eq!(hidden.window_layout.surface_mode, SurfaceMode::Compact);
    assert_eq!(
        hidden.state.dashboard.visibility,
        DashboardVisibility::Hidden
    );
}

#[test]
fn voice_recording_endpoint_streams_real_audio_bytes_end_to_end() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let started = core.trigger_voice_recording("rec-1", "audio/wav").unwrap();
    let first = core.stream_voice_audio(voice_chunk("rec-1", &[82, 73, 70, 70], false));
    let final_chunk = core.stream_voice_audio(voice_chunk("rec-1", &[87, 65, 86, 69], true));

    assert_eq!(
        started.state.dashboard.active_view,
        DashboardView::VoiceRecording
    );
    assert_eq!(
        started.state.streams.speech.phase,
        ModalityStreamPhase::Capturing
    );
    assert_eq!(first.unwrap().state.voice_recording.audio_bytes, 4);
    let snapshot = final_chunk.unwrap();
    let recording = core.current_voice_recording().unwrap();
    assert_eq!(
        snapshot.state.voice_recording.status,
        VoiceRecordingStatus::Completed
    );
    assert_eq!(
        snapshot.state.streams.speech.phase,
        ModalityStreamPhase::Ready
    );
    assert_eq!(core.voice_recording_status().audio_bytes, 8);
    assert_eq!(recording.audio, vec![82, 73, 70, 70, 87, 65, 86, 69]);
}

#[test]
fn voice_recording_endpoint_rejects_mismatched_stream_id() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();
    core.trigger_voice_recording("rec-1", "audio/wav").unwrap();

    let error = core
        .stream_voice_audio(voice_chunk("rec-other", &[1, 2, 3], false))
        .unwrap_err();

    assert!(error.to_string().contains("active voice recording id"));
}

#[test]
fn disabled_voice_recording_endpoint_is_rejected() {
    let settings = AppSettings {
        voice_recording_enabled: false,
        ..Default::default()
    };
    let mut core = FrontendCore::new(settings);
    core.spawn_app(default_work_area()).unwrap();

    let error = core
        .trigger_voice_recording("rec-1", "audio/wav")
        .unwrap_err();

    assert!(error.to_string().contains("enabled voice recording"));
}

#[test]
fn voice_playback_endpoint_controls_real_audio_item_end_to_end() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let opened = core.open_voice_playback("play-1").unwrap();
    let appended = core
        .append_voice_playback_chunk(playback_chunk("play-1"))
        .unwrap();
    let playing = core
        .set_voice_playback_status("play-1", VoicePlaybackStatus::Playing)
        .unwrap();
    let completed = core
        .set_voice_playback_status("play-1", VoicePlaybackStatus::Completed)
        .unwrap();

    assert_eq!(
        opened.state.voice_playback.active_playback_id.as_deref(),
        Some("play-1")
    );
    assert_eq!(
        opened.state.dashboard.active_view,
        DashboardView::VoicePlayback
    );
    assert_eq!(
        appended
            .state
            .voice_playback
            .segments
            .back()
            .unwrap()
            .status,
        VoicePlaybackStatus::Queued
    );
    assert_eq!(playing.state.orb.mode, OrbMode::Activity);
    assert_eq!(completed.state.orb.mode, OrbMode::Idle);
    assert_eq!(
        core.current_voice_playback_segment().unwrap().segment_index,
        0
    );
    assert_eq!(
        core.voice_playback_status().segments.back().unwrap().status,
        VoicePlaybackStatus::Completed
    );
}

#[test]
fn voice_playback_endpoint_rejects_mismatched_status_id() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();
    core.open_voice_playback("play-1").unwrap();
    core.append_voice_playback_chunk(playback_chunk("play-1"))
        .unwrap();

    let error = core
        .set_voice_playback_status("play-other", VoicePlaybackStatus::Playing)
        .unwrap_err();

    assert!(error.to_string().contains("active voice playback id"));
}

#[test]
fn dashboard_view_opens_requested_surface() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let snapshot = core
        .open_dashboard_view(DashboardView::GraphViewer)
        .unwrap();

    assert_eq!(
        snapshot.state.dashboard.active_view,
        DashboardView::GraphViewer
    );
    assert_eq!(
        snapshot.state.dashboard.visibility,
        DashboardVisibility::Visible
    );
}

#[test]
fn disabled_graph_view_is_rejected() {
    let settings = AppSettings {
        graph_view_enabled: false,
        ..Default::default()
    };
    let mut core = FrontendCore::new(settings);
    core.spawn_app(default_work_area()).unwrap();

    let error = core
        .open_dashboard_view(DashboardView::GraphViewer)
        .unwrap_err();

    assert!(error.to_string().contains("enabled graph view"));
    assert_eq!(
        core.state().dashboard.active_view,
        DashboardView::CanvasDashboard
    );
}

#[test]
fn countdown_digit_changes_orb_state() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let snapshot = core.start_countdown(4).unwrap();

    assert_eq!(snapshot.state.orb.mode, OrbMode::Countdown);
    assert_eq!(snapshot.state.orb.countdown_digit, Some(4));
}

#[test]
fn invalid_countdown_digit_reports_expected_shape() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let error = core.start_countdown(0).unwrap_err();

    assert!(error.to_string().contains("digit 1..=9"));
}

#[test]
fn modality_stream_failure_requires_error_text() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();
    let error = core
        .set_modality_stream(
            ModalityStreamKind::Speech,
            ModalityStreamPhase::Failed,
            None,
        )
        .unwrap_err();

    assert!(error.to_string().contains("non-empty stream failure"));
}

#[test]
fn modality_stream_updates_requested_channel() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();

    let snapshot = core
        .set_modality_stream(
            ModalityStreamKind::DesktopBroadcasts,
            ModalityStreamPhase::Capturing,
            None,
        )
        .unwrap();

    assert_eq!(
        snapshot.state.streams.desktop_broadcasts.phase,
        ModalityStreamPhase::Capturing
    );
}

#[test]
fn context_menu_patches_return_renderer_json() {
    let mut core = FrontendCore::default();

    let input = core.apply_app_settings_patch(&AppSettingsPatch::InputDevice(Some("mic-a".into())));
    let output =
        core.apply_app_settings_patch(&AppSettingsPatch::OutputDevice(Some("speaker-a".into())));
    let graph = core.apply_app_settings_patch(&AppSettingsPatch::GraphViewEnabled(false));
    let recording = core.apply_app_settings_patch(&AppSettingsPatch::VoiceRecordingEnabled(false));
    let broadcasts =
        core.apply_app_settings_patch(&AppSettingsPatch::DesktopBroadcastsEnabled(false));
    let view = core.apply_app_settings_patch(&AppSettingsPatch::DefaultDashboardView(
        DashboardView::DesktopBroadcasts,
    ));

    assert_eq!(input["inputDeviceId"], "mic-a");
    assert_eq!(output["outputDeviceId"], "speaker-a");
    assert_eq!(graph["graphViewEnabled"], false);
    assert_eq!(recording["voiceRecordingEnabled"], false);
    assert_eq!(broadcasts["desktopBroadcastsEnabled"], false);
    assert_eq!(view["defaultDashboardView"], "desktop_broadcasts");
}

#[test]
fn quick_menu_contains_only_audio_controls() {
    let core = FrontendCore::default();
    let devices = AudioDeviceCatalog {
        inputs: vec![AudioDeviceOption {
            id: "mic-a".to_string(),
            label: "Studio Mic".to_string(),
        }],
        outputs: vec![AudioDeviceOption {
            id: "speaker-a".to_string(),
            label: "Studio Speakers".to_string(),
        }],
    };
    let menu = core.app_settings().menu_model(&devices);
    let labels = menu
        .sections
        .iter()
        .map(|section| section.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["Microphone Input", "Speaker Output"]);
    assert!(menu.sections.iter().all(|section| {
        section.items.iter().all(|item| {
            !item.id.starts_with("engine:")
                && !item.id.starts_with("model:")
                && !item.id.starts_with("vector:")
        })
    }));
    for item_id in ["plugin-view:example.workspace:main", "settings:open"] {
        assert_eq!(
            core.app_settings().patch_for_menu_item(&devices, item_id),
            None
        );
    }
}

#[test]
fn window_bounds_cover_compact_shell_and_fullscreen() {
    let area = default_work_area();

    let compact = widget_bounds_for_mode(area, SurfaceMode::Compact);
    let shell = widget_bounds_for_mode(area, SurfaceMode::Shell);
    let fullscreen = widget_bounds_for_mode(area, SurfaceMode::Fullscreen);

    assert_eq!(compact.width, COMPACT_WIDGET_SIZE);
    assert_eq!(shell.width, SHELL_WIDTH);
    assert_eq!(fullscreen.width, area.width);
}
#[test]
fn settings_window_uses_logical_target_at_unit_scale() {
    let area = WorkArea::new(100, 40, 1440, 900, 1.0);
    let bounds = settings_window_bounds(area);

    assert_eq!(
        bounds,
        WidgetBounds {
            x: 420,
            y: 190,
            width: 800,
            height: 600,
        }
    );
}

#[test]
fn settings_window_scales_and_centers_with_offset_work_area() {
    let area = WorkArea::new(-800, 120, 1600, 1200, 1.5);
    let bounds = settings_window_bounds(area);

    assert_eq!(
        bounds,
        WidgetBounds {
            x: -600,
            y: 270,
            width: 1200,
            height: 900,
        }
    );
}

#[test]
fn settings_window_clamps_to_small_work_area() {
    let area = WorkArea::new(-320, 80, 700, 500, 2.0);
    let bounds = settings_window_bounds(area);

    assert_eq!(
        bounds,
        WidgetBounds {
            x: -320,
            y: 80,
            width: 700,
            height: 500,
        }
    );
}

#[test]
fn moved_widget_bounds_are_clamped_to_work_area() {
    let area = WorkArea::new(0, 0, 300, 200, 1.0);
    let bounds = WidgetBounds {
        x: 250,
        y: 190,
        width: 100,
        height: 100,
    };

    let moved = bounds.moved_by(100.0, 100.0, area);

    assert_eq!(moved.x, 200);
    assert_eq!(moved.y, 100);
    assert_eq!(clamp_widget_bounds(bounds, area), moved);
}

#[test]
fn compact_collapse_bounds_follow_current_screen_when_remembered_orb_is_elsewhere() {
    let first_screen = WorkArea::new(0, 0, 1440, 900, 1.0);
    let second_screen = WorkArea::new(1440, 0, 1440, 900, 1.0);
    let remembered = widget_bounds_for_mode(first_screen, SurfaceMode::Compact);

    let collapsed = compact_bounds_for_collapse(second_screen, Some(remembered));

    assert_ne!(collapsed, remembered);
    assert_eq!(
        collapsed,
        widget_bounds_for_mode(second_screen, SurfaceMode::Compact)
    );
}

#[test]
fn compact_collapse_bounds_keep_remembered_orb_on_current_screen() {
    let screen = WorkArea::new(1440, 0, 1440, 900, 1.0);
    let remembered = WidgetBounds {
        x: 1600,
        y: 120,
        width: COMPACT_WIDGET_SIZE,
        height: COMPACT_WIDGET_SIZE,
    };

    let collapsed = compact_bounds_for_collapse(screen, Some(remembered));

    assert_eq!(collapsed, remembered);
}

#[test]
fn work_area_for_bounds_tracks_screen_containing_window_center() {
    let first_screen = WorkArea::new(0, 0, 1440, 900, 1.0);
    let second_screen = WorkArea::new(1440, 0, 1440, 900, 1.0);
    let bounds = WidgetBounds {
        x: 1600,
        y: 100,
        width: COMPACT_WIDGET_SIZE,
        height: COMPACT_WIDGET_SIZE,
    };

    let tracked = work_area_for_bounds(bounds, [first_screen, second_screen], first_screen);

    assert_eq!(tracked, second_screen);
}

#[test]
fn work_area_for_point_tracks_same_and_other_screen_clicks() {
    let first_screen = WorkArea::new(0, 0, 1440, 900, 1.0);
    let second_screen = WorkArea::new(1440, 0, 1440, 900, 1.0);

    let same_screen = work_area_for_point(1600, 400, [first_screen, second_screen], first_screen);
    let other_screen = work_area_for_point(400, 400, [first_screen, second_screen], second_screen);

    assert_eq!(same_screen, second_screen);
    assert_eq!(other_screen, first_screen);
}

#[test]
fn core_rebases_window_layout_to_current_screen_before_shape_change() {
    let first_screen = WorkArea::new(0, 0, 1440, 900, 1.0);
    let second_screen = WorkArea::new(1440, 0, 1440, 900, 1.0);
    let mut core = FrontendCore::default();

    core.spawn_app(first_screen).unwrap();
    core.set_window_work_area(second_screen).unwrap();
    let snapshot = core.click_orb().unwrap();

    assert_eq!(snapshot.window_layout.work_area, second_screen);
    assert_eq!(
        snapshot.window_plan.bounds,
        widget_bounds_for_mode(second_screen, SurfaceMode::Shell)
    );
}

#[test]
fn window_layout_switches_surface_mode() {
    let layout = WindowLayout::compact(default_work_area()).with_surface_mode(SurfaceMode::Shell);

    assert_eq!(layout.surface_mode, SurfaceMode::Shell);
    assert_eq!(layout.active_bounds.width, SHELL_WIDTH);
}

#[test]
fn window_management_plan_expands_dashboard_and_keeps_compact_orb_small() {
    let compact = WindowLayout::compact(default_work_area()).management_plan();
    let fullscreen = WindowLayout::compact(default_work_area())
        .with_surface_mode(SurfaceMode::Fullscreen)
        .management_plan();

    assert_eq!(compact.display_state, WindowDisplayState::CompactOrb);
    assert_eq!(compact.bounds.width, COMPACT_WIDGET_SIZE);
    assert!(compact.commands.iter().any(|command| matches!(
        command,
        WindowManagementCommand::RememberCompactBounds { .. }
    )));
    assert_eq!(
        fullscreen.display_state,
        WindowDisplayState::DashboardFullscreen
    );
    assert_eq!(fullscreen.bounds.width, default_work_area().width);
    assert!(
        fullscreen
            .commands
            .iter()
            .any(|command| matches!(command, WindowManagementCommand::SetBounds { .. }))
    );
    assert!(HOST_MINIMIZE_COLLAPSE_SCRIPT.contains("setExpanded?.(false)"));
}

#[test]
fn modality_streams_are_exposed_as_component_interface() {
    let mut core = FrontendCore::default();
    core.spawn_app(default_work_area()).unwrap();
    core.set_modality_stream(
        ModalityStreamKind::Screenshots,
        ModalityStreamPhase::Capturing,
        None,
    )
    .unwrap();

    let streams = core.modality_streams();

    assert_eq!(streams.streams.len(), 4);
    assert!(streams.streams.iter().any(|stream| {
        stream.kind == ModalityStreamKind::Screenshots
            && stream.phase == ModalityStreamPhase::Capturing
    }));
    assert!(streams.streams.iter().any(|stream| {
        stream.kind == ModalityStreamKind::ScreenFrameBroadcasts
            && stream.phase == ModalityStreamPhase::Ready
    }));
}

#[test]
fn canvas_patch_preserves_content_fields() {
    let mut core = FrontendCore::default();
    let snapshot = core
        .update_canvas(
            "main",
            CanvasPatch {
                canvas_id: "main".to_string(),
                elements: vec![CanvasElement {
                    element_id: "assistant-card".to_string(),
                    element_kind: "note".to_string(),
                    content: json!({ "text": "Updated by assistant plugin", "x": 24 }),
                }],
            },
        )
        .unwrap();

    assert_eq!(snapshot.elements[0].content["id"], json!("assistant-card"));
    assert_eq!(snapshot.elements[0].content["type"], json!("note"));
    assert_eq!(
        snapshot.elements[0].content["text"],
        json!("Updated by assistant plugin")
    );
    assert_eq!(snapshot.elements[0].content["x"], json!(24));
}

fn voice_chunk(recording_id: &str, bytes: &[u8], final_chunk: bool) -> VoiceAudioChunk {
    VoiceAudioChunk {
        recording_id: recording_id.to_string(),
        media_type: "audio/wav".to_string(),
        bytes: bytes.to_vec(),
        final_chunk,
    }
}

fn playback_chunk(playback_id: &str) -> VoicePlaybackChunk {
    VoicePlaybackChunk {
        playback_id: playback_id.to_string(),
        media_type: "audio/pcm;rate=24000;format=s16le".to_string(),
    }
}
