//! Functional regressions для install-scoped video backend policy.
//!
//! Тесты проходят strong staged install целиком и не подменяют terminal seek commit
//! прямым вызовом: successful route обязан реально довести packet до decoder-а,
//! принять decoded frame текущего generation и представить его scheduler-ом.

use super::super::test_support::{
    decoded_frame_for_current_seek_generation, fake_video_packet, seek_admission_tick_config,
};
use super::*;
use crate::{MediaInstallVideoBackendConstraint, PlayerTickContext};
use video_frame_contract::VideoFrameContract;

/// Возвращает canonical FFmpeg software backend ID без concrete decoder dependency.
fn ffmpeg_software_backend_id() -> DecodeBackendId {
    DecodeBackendId::new("ffmpeg-sw").expect("canonical FFmpeg backend id должен быть допустим")
}

/// Строит capability snapshot, где одинаковый VP9 stream сначала предлагает VA-API,
/// а затем FFmpeg software. Такой порядок воспроизводит исходную production-регрессию.
fn vaapi_then_ffmpeg_vp9_capabilities() -> capability_core::SystemCapabilities {
    let mut capabilities = capabilities_with_vp9_profile0();
    let base_output = capabilities
        .playable_video_outputs
        .first()
        .cloned()
        .expect("VP9 fixture должен содержать playable output");
    let base_backend = capabilities
        .video_backends
        .first()
        .cloned()
        .expect("VP9 fixture должен содержать backend capabilities");

    let vaapi_backend_id = DecodeBackendId::vaapi();
    let mut vaapi_output = base_output.clone();
    vaapi_output.backend = vaapi_backend_id.clone();

    let ffmpeg_backend_id = ffmpeg_software_backend_id();
    let mut ffmpeg_output = base_output;
    ffmpeg_output.backend = ffmpeg_backend_id.clone();
    ffmpeg_output.frame_contract = VideoFrameContract::host_yuv420_planar8();

    let mut vaapi_backend = base_backend.clone();
    vaapi_backend.backend_id = vaapi_backend_id;
    vaapi_backend.display_name = "Test VA-API".to_owned();
    vaapi_backend.raw_supported_outputs = vec![vaapi_output.clone()];

    let mut ffmpeg_backend = base_backend;
    ffmpeg_backend.backend_id = ffmpeg_backend_id;
    ffmpeg_backend.display_name = "Test FFmpeg software".to_owned();
    ffmpeg_backend.raw_supported_outputs = vec![ffmpeg_output.clone()];

    capabilities.video_backends = vec![vaapi_backend, ffmpeg_backend];
    capabilities.playable_video_outputs = vec![vaapi_output, ffmpeg_output];
    capabilities
}

/// Оставляет в snapshot-е только первый VA-API output для negative policy route.
fn vaapi_only_vp9_capabilities() -> capability_core::SystemCapabilities {
    let mut capabilities = vaapi_then_ffmpeg_vp9_capabilities();
    capabilities.video_backends.truncate(1);
    capabilities.playable_video_outputs.truncate(1);
    capabilities
}

/// Создаёт seekable VP9 candidate с единственным target packet после demux seek-а.
fn prepared_vp9_media_with_target_packet(
    target_position: Duration,
    seek_log: Arc<Mutex<Vec<Duration>>>,
) -> PreparedMedia {
    let video_track = staged_vp9_track(1);
    let mut demuxer = FakeDemuxer::new(vec![video_track], Some(Duration::from_secs(42)), seek_log);
    demuxer.push_event(DemuxReadEvent::Packet(fake_video_packet(
        crate::TrackId::new(1),
        target_position,
    )));
    PreparedMedia::from_external_label("software-policy-vp9", Box::new(demuxer))
}

/// Software policy обязана выбрать FFmpeg даже при первом playable VA-API output-е,
/// а startup restore должен завершиться только после реально представленного кадра.
#[test]
fn software_policy_staged_install_and_startup_restore_reach_presented_frame() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(vaapi_then_ffmpeg_vp9_capabilities());

    let target_position = Duration::from_secs(7);
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let decoder = SharedFakeVideoDecoderThread::new();
    let ffmpeg_backend_id = ffmpeg_software_backend_id();
    let (resource_port, resource_state) =
        FakeResourcePort::available_for_backend(decoder.clone(), ffmpeg_backend_id.clone());
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(1_901).expect("test request id должен быть non-zero"),
    );
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    session.stage_prepared_media_install(
        request_id,
        prepared_vp9_media_with_target_packet(target_position, Arc::clone(&seek_log)),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        MediaInstallVideoResourcePort::new(
            MediaInstallVideoBackendConstraint::RequireBackend(ffmpeg_backend_id.clone()),
            resource_port,
        ),
    );

    assert!(
        receipt.try_take_ready_to_commit().is_some(),
        "software candidate должен достичь ReadyToCommit"
    );
    {
        let state = resource_state
            .lock()
            .expect("fake resource state lock должен оставаться доступен");
        assert_eq!(state.request_count, 1);
        let [selection] = state.requested_selections.as_slice() else {
            panic!("player должен сделать ровно один exact backend request");
        };
        assert_eq!(selection.expected_backend_id(), ffmpeg_backend_id.as_str());
        assert_eq!(
            selection.frame_contract(),
            VideoFrameContract::host_yuv420_planar8()
        );
        assert!(matches!(
            state.statuses.as_slice(),
            [DetachedVideoBackendCandidateStatus::StreamConfigured {
                backend_id,
                ..
            }] if backend_id == ffmpeg_backend_id.as_str()
        ));
    }

    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let completion = receipt
        .try_take_completion()
        .expect("authorization должна синхронно опубликовать Installed");
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = completion
    else {
        panic!("успешный software candidate должен завершиться Installed");
    };

    let (restore_tx, restore_rx) = crossbeam_channel::bounded(1);
    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::SeekTo(target_position),
        },
        restore_tx,
    );
    assert_eq!(
        restore_rx.try_recv(),
        Err(crossbeam_channel::TryRecvError::Empty),
        "restore нельзя подтверждать до decoder landing"
    );

    let packet_tick = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            max_video_packets_sent_per_tick: 1,
            ..seek_admission_tick_config(2, 4)
        },
    ));
    assert_eq!(packet_tick.video_frames_presented, 0);
    assert_eq!(decoder.sent_packets().len(), 1);

    let mut decoded_frame =
        decoded_frame_for_current_seek_generation(&session, target_position, 1_901);
    decoded_frame.frame_contract = VideoFrameContract::host_yuv420_planar8();
    decoder.push_decoded_frame(decoded_frame);
    let present_tick = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));

    assert_eq!(present_tick.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(target_position)
    );
    assert_eq!(
        restore_rx
            .try_recv()
            .expect("представленный target frame должен завершить restore"),
        InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
    );
    assert_eq!(session.snapshot().current_position, target_position);
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert!(!session.snapshot().timeline.scrubbing);
    assert!(session.seek_commit().is_none());
    assert_eq!(
        *seek_log
            .lock()
            .expect("seek log lock должен оставаться доступен"),
        vec![target_position]
    );
}

/// Required FFmpeg policy не имеет права молча выбрать единственный VA-API output
/// и не должна пересекать Ready barrier либо менять старый Playing instance.
#[test]
fn software_policy_never_requests_hardware_when_only_vaapi_matches() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(vaapi_only_vp9_capabilities());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Playing);

    let ffmpeg_backend_id = ffmpeg_software_backend_id();
    let decoder = SharedFakeVideoDecoderThread::new();
    let (resource_port, resource_state) =
        FakeResourcePort::available_for_backend(decoder, ffmpeg_backend_id.clone());
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(1_902).expect("test request id должен быть non-zero"),
    );
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    session.stage_prepared_media_install(
        request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        MediaInstallVideoResourcePort::new(
            MediaInstallVideoBackendConstraint::RequireBackend(ffmpeg_backend_id.clone()),
            resource_port,
        ),
    );

    assert!(receipt.try_take_ready_to_commit().is_none());
    let completion = receipt
        .try_take_completion()
        .expect("required backend miss должен terminal-завершить install");
    let MediaInstallCompletion::Failed { failure, .. } = completion else {
        panic!("required backend miss должен вернуть typed Failed terminal");
    };
    assert_eq!(
        failure.stage,
        MediaInstallFailureStage::VideoStreamConfiguration
    );
    assert_eq!(
        failure.error.kind,
        PlayerErrorKind::RequiredVideoBackendUnavailable
    );
    assert!(
        failure.error.message.contains(ffmpeg_backend_id.as_str()),
        "diagnostic должен назвать exact required backend"
    );
    let state = resource_state
        .lock()
        .expect("fake resource state lock должен оставаться доступен");
    assert_eq!(state.request_count, 0);
    assert!(state.requested_selections.is_empty());
    assert!(state.statuses.is_empty());
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(session.playback_state(), PlaybackState::Playing);
}

/// Required FFmpeg resource failure должен сохранять software-policy diagnostic,
/// не делать fallback на VA-API и не менять lifecycle уже играющего instance-а.
#[test]
fn software_policy_resource_unavailable_reports_required_backend_and_preserves_old_instance() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(vaapi_then_ffmpeg_vp9_capabilities());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Playing);

    let ffmpeg_backend_id = ffmpeg_software_backend_id();
    let resource_failure_reason = format!(
        "required software backend `{}` resource is unavailable",
        ffmpeg_backend_id.as_str()
    );
    let expected_error_message =
        format!("candidate backend unavailable: {resource_failure_reason}");
    let (resource_port, resource_state) =
        FakeResourcePort::unavailable(DetachedVideoBackendResourceError::Unavailable {
            reason: resource_failure_reason,
        });
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(1_903).expect("test request id должен быть non-zero"),
    );
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    session.stage_prepared_media_install(
        request_id,
        prepared_media(&[TrackKind::Video]),
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        MediaInstallVideoResourcePort::new(
            MediaInstallVideoBackendConstraint::RequireBackend(ffmpeg_backend_id.clone()),
            resource_port,
        ),
    );

    assert!(receipt.try_take_ready_to_commit().is_none());
    let completion = receipt
        .try_take_completion()
        .expect("software resource failure должен terminal-завершить install");
    let MediaInstallCompletion::Failed { failure, .. } = completion else {
        panic!("software resource failure должен вернуть typed Failed terminal");
    };
    assert_eq!(
        failure.stage,
        MediaInstallFailureStage::CandidateVideoResourceAcquisition
    );
    assert_eq!(
        failure.error.kind,
        PlayerErrorKind::RequiredVideoBackendUnavailable
    );
    assert_eq!(failure.error.message, expected_error_message);

    let state = resource_state
        .lock()
        .expect("fake resource state lock должен оставаться доступен");
    assert_eq!(state.request_count, 1);
    let [selection] = state.requested_selections.as_slice() else {
        panic!("player должен сделать ровно один exact software backend request");
    };
    assert_eq!(selection.expected_backend_id(), ffmpeg_backend_id.as_str());
    assert_eq!(
        selection.frame_contract(),
        VideoFrameContract::host_yuv420_planar8()
    );
    assert!(state.statuses.is_empty());
    drop(state);

    assert!(!session.has_staged_media_install());
    assert_eq!(session.runtime_reconfigure_boundary_activity(), None);
    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(session.playback_state(), PlaybackState::Playing);
}
