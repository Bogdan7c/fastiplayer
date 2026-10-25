//! Тесты политики Dynamic: приём события отставания и «действующий режим» файла.

use std::num::NonZeroU64;
use std::time::Duration;

use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoCodec,
    VideoDecodeRequirement,
};
use fastiplayer_config::VideoBackendPreference;
use player_core::{
    DecoderPaceEvidence, MediaInstanceId, PlaybackRate, PlayerVideoDecoderThreadConfig,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::*;
use crate::dma_buf_runtime_fallback::DmaBufRuntimeFallbackController;
use crate::video_pipeline_selector::select_video_pipeline_plan;
use player_core::PlayerRuntimeApplyError;

fn media(id: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(id).expect("test media id is non-zero"))
}

fn evidence(media_id: u64, render_generation: u64) -> DecoderPaceEvidence {
    DecoderPaceEvidence {
        evaluation_window: Duration::from_secs(4),
        expected_frames: 100.0,
        late_frames: 30,
        starved_frames: 10.0,
        missed_frame_ratio: 0.4,
        playback_rate: PlaybackRate::default(),
        media_instance_id: Some(media(media_id)),
        render_generation,
    }
}

/// Контекст «играет медиа 1 на HW, Dynamic, поколение 5, перехода ещё не было».
fn hardware_dynamic_context() -> DecoderPaceEventContext {
    DecoderPaceEventContext {
        current_media: Some(media(1)),
        current_render_generation: 5,
        current_backend_kind: Some(VideoBackendKind::HardwareZeroCopy),
        committed_preference: VideoBackendPreference::Dynamic,
        switch_already_attempted: false,
    }
}

#[test]
fn hardware_dynamic_current_media_queues_the_switch() {
    assert_eq!(
        classify_decoder_pace_event(&evidence(1, 5), &hardware_dynamic_context()),
        DecoderPaceEventDisposition::QueueSoftwareSwitch
    );
}

#[test]
fn event_from_another_media_is_ignored() {
    assert_eq!(
        classify_decoder_pace_event(&evidence(2, 5), &hardware_dynamic_context()),
        DecoderPaceEventDisposition::IgnoreForeignMedia
    );
}

#[test]
fn event_from_an_older_render_generation_is_ignored_but_a_newer_one_is_not() {
    let context = hardware_dynamic_context();
    assert_eq!(
        classify_decoder_pace_event(&evidence(1, 4), &context),
        DecoderPaceEventDisposition::IgnoreStaleGeneration
    );
    // Snapshot мог отстать от события: более новое поколение не считается устаревшим.
    assert_eq!(
        classify_decoder_pace_event(&evidence(1, 6), &context),
        DecoderPaceEventDisposition::QueueSoftwareSwitch
    );
}

/// Если и программный декод не успевает, остаёмся на нём: обратного прыжка на HW нет.
#[test]
fn event_while_software_decoding_never_switches_anything() {
    for backend in [Some(VideoBackendKind::FfmpegSoftware), None] {
        let context = DecoderPaceEventContext {
            current_backend_kind: backend,
            ..hardware_dynamic_context()
        };
        assert_eq!(
            classify_decoder_pace_event(&evidence(1, 5), &context),
            DecoderPaceEventDisposition::IgnoreNotHardwareDecoding,
            "{backend:?}"
        );
    }
}

#[test]
fn repeated_event_after_an_attempt_is_ignored() {
    let context = DecoderPaceEventContext {
        switch_already_attempted: true,
        ..hardware_dynamic_context()
    };
    assert_eq!(
        classify_decoder_pace_event(&evidence(1, 5), &context),
        DecoderPaceEventDisposition::IgnoreSwitchAlreadyAttempted
    );
}

#[test]
fn only_dynamic_preference_lets_slowness_switch_the_file() {
    for preference in [
        VideoBackendPreference::Auto,
        VideoBackendPreference::Hardware,
        VideoBackendPreference::Software,
    ] {
        let context = DecoderPaceEventContext {
            committed_preference: preference,
            ..hardware_dynamic_context()
        };
        assert_eq!(
            classify_decoder_pace_event(&evidence(1, 5), &context),
            DecoderPaceEventDisposition::IgnorePreferenceDoesNotSwitch,
            "{preference:?}"
        );
    }
}

/// Железо тянет H.264 и аппаратно, и программно.
fn capabilities_with_hardware_and_software_h264() -> SystemCapabilities {
    let format = SupportedVideoDecodeFormat {
        codec: VideoCodec::H264,
        profile: codec_core::VideoProfile::H264(codec_core::H264Profile::Main),
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        max_width: Some(1920),
        max_height: Some(1080),
        max_fps: Some(60.0),
        hdr_input: false,
    };
    let hardware = SupportedVideoOutput {
        backend: DecodeBackendId::vaapi(),
        decode_format: format.clone(),
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
    };
    let software = SupportedVideoOutput {
        backend: DecodeBackendId::new(video_ffmpeg::FFMPEG_SOFTWARE_BACKEND_ID)
            .expect("valid FFmpeg backend id"),
        decode_format: format,
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    };
    let playable_video_outputs = vec![hardware, software];
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id: DecodeBackendId::vaapi(),
            display_name: "Test VA-API".to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: playable_video_outputs.clone(),
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs,
    }
}

/// Итоговый backend, который выберет приложение для H.264 при действующем режиме.
fn planned_backend_kind(
    effective: VideoBackendPreference,
    capabilities: &SystemCapabilities,
) -> VideoBackendKind {
    let requirement = VideoDecodeRequirement::new(VideoCodec::H264);
    select_video_pipeline_plan(
        effective,
        Some(capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        Some(&requirement),
    )
    .expect("H.264 is playable in the test capabilities")
    .backend_kind()
}

/// Сквозная цепочка политики: файл 1 начинает с HW, после перехода переподбор даёт SW, а
/// следующий файл (сброс на `MediaOpenRequested`) снова получает HW-план.
#[test]
fn switched_file_stays_on_software_and_the_next_file_returns_to_hardware() {
    let capabilities = capabilities_with_hardware_and_software_h264();
    let committed = VideoBackendPreference::Dynamic;
    let mut controller = DmaBufRuntimeFallbackController::default();

    // Старт файла: действующий режим = committed, план — аппаратный.
    assert_eq!(
        planned_backend_kind(controller.effective_preference(committed), &capabilities),
        VideoBackendKind::HardwareZeroCopy
    );

    // Успешный переход: все читатели preference теперь видят Software.
    controller.note_software_switch_installed(committed);
    assert_eq!(
        planned_backend_kind(controller.effective_preference(committed), &capabilities),
        VideoBackendKind::FfmpegSoftware
    );

    // Следующий файл: сброс защёлки и override.
    controller.reset_for_new_media();
    assert_eq!(
        planned_backend_kind(controller.effective_preference(committed), &capabilities),
        VideoBackendKind::HardwareZeroCopy
    );
    assert!(!controller.has_attempted_switch());
}

#[test]
fn text_for_the_user_is_the_agreed_english_sentence() {
    assert_eq!(
        DYNAMIC_SWITCH_TO_SOFTWARE_MESSAGE,
        "Hardware decoding couldn't keep up, so this video switched to software decoding."
    );
}

/// Охранный тест проводки: у `AppState` нет headless-обвязки, поэтому читателей preference
/// и точки приёма/фиксации перехода защищаем по исходникам. Если читатель снова возьмёт
/// committed preference напрямую, файл после HW->SW перехода может вернуться на HW.
#[test]
fn wiring_readers_use_the_effective_mode_and_the_event_reaches_the_policy() {
    let backend_source = include_str!("../video_backend.rs");
    let reselection = section(
        backend_source,
        "pub(crate) fn apply_video_backend_reselection",
        "pub(crate) fn note_dma_buf_fallback_trigger",
    );
    assert!(reselection.contains("self.effective_video_backend_preference()"));
    assert!(!reselection.contains("committed_config_snapshot.video_backend_preference()"));

    let renderer_check = section(
        backend_source,
        "pub(crate) fn validate_renderer_candidate_capabilities",
        "pub(crate) fn release_renderer_bound_visual_state",
    );
    assert!(renderer_check.contains("self.effective_video_backend_preference()"));

    let apply = section(
        backend_source,
        "pub(crate) fn apply_pending_dma_buf_runtime_fallback",
        "pub(super) fn reject_pending_video_backend",
    );
    assert!(apply.contains("note_software_switch_installed(committed_preference)"));
    assert!(apply.contains("announce_dynamic_switch_to_software"));

    let settings_adapter = include_str!("../../frame_prepare/settings_runtime_adapter.rs");
    assert!(settings_adapter.contains("self.app_state.effective_video_backend_preference()"));

    let worker_events = include_str!("../../frame_prepare.rs");
    assert!(worker_events.contains(
        "PlayerEvent::VideoDecoderFallingBehind(evidence) => {\n                        app_state.note_video_decoder_falling_behind(evidence);"
    ));
}

/// Участок исходника между двумя маркерами.
fn section<'source>(source: &'source str, start: &str, end: &str) -> &'source str {
    let from = source.find(start).expect("start marker exists");
    let tail = &source[from..];
    let to = tail.find(end).expect("end marker exists");
    &tail[..to]
}

/// Отказ перехода Dynamic с заданной причиной неудачи пересборки конвейера.
fn rejected_switch_failing_with(
    fallback_failure: VideoPipelineRebuildError,
) -> DmaBufRuntimeFallbackFailure {
    DmaBufRuntimeFallbackFailure {
        error: DmaBufRuntimeFallbackError::SoftwarePipelineFailed {
            trigger: DmaBufFallbackTrigger::HardwareDecoderFallingBehind(evidence(1, 5)),
            fallback_failure,
        },
        player_error: None,
    }
}

#[test]
fn failure_before_the_software_backend_is_installed_keeps_hardware_video_playing() {
    let failures_before_install = [
        VideoPipelineRebuildError::Preparation("prepare failed".to_owned()),
        VideoPipelineRebuildError::BackendStartup {
            plan_label: "software",
            message: "no ffmpeg".to_owned(),
        },
        VideoPipelineRebuildError::Worker(PlayerRuntimeApplyError::Backpressure),
    ];
    for failure in failures_before_install {
        let description = failure.to_string();
        assert_eq!(
            classify_rejected_switch(&rejected_switch_failing_with(failure)),
            RejectedSwitchImpact::VideoKeepsPlaying,
            "{description}"
        );
    }
}

#[test]
fn policy_refusals_never_report_a_broken_video() {
    let refusal = DmaBufRuntimeFallbackFailure {
        error: DmaBufRuntimeFallbackError::FallbackLoopPrevented {
            trigger: DmaBufFallbackTrigger::HardwareDecoderFallingBehind(evidence(1, 5)),
            render_generation: 5,
        },
        player_error: None,
    };

    assert_eq!(
        classify_rejected_switch(&refusal),
        RejectedSwitchImpact::VideoKeepsPlaying
    );
}

#[test]
fn failure_after_the_software_backend_is_installed_reports_a_broken_video() {
    let impact = classify_rejected_switch(&rejected_switch_failing_with(
        VideoPipelineRebuildError::ImportInstallAfterBackendSwap("import rejected".to_owned()),
    ));

    let RejectedSwitchImpact::VideoBroken(player_error) = impact else {
        panic!("a failure after the install must be reported, got {impact:?}");
    };
    assert_eq!(
        player_error.kind,
        player_core::PlayerRenderErrorKind::VideoBackendSwitchFailed
    );
    // Пользователь видит внутреннюю ошибку плеера, а не «видеокарта перестала отвечать».
    assert_eq!(
        player_error.to_player_error().kind,
        player_core::PlayerErrorKind::RuntimeError
    );
    assert!(
        player_error
            .message
            .starts_with("Switching to software decoding failed:"),
        "{}",
        player_error.message
    );
    assert!(player_error.message.contains("import rejected"));
}

#[test]
fn worker_rollback_failure_reports_a_broken_video() {
    let impact = classify_rejected_switch(&rejected_switch_failing_with(
        VideoPipelineRebuildError::Worker(PlayerRuntimeApplyError::ApplyAndRollbackFailed {
            apply_error: "apply".to_owned(),
            rollback_error: "rollback".to_owned(),
        }),
    ));

    assert!(matches!(impact, RejectedSwitchImpact::VideoBroken(_)));
}
