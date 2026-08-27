//! Vertical regressions для adoption уже подготовленной initial media position.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{TryRecvError, bounded};
use media_core::{DemuxSeekResult, MediaTime, PacketKeyframe, TrackKind};

use super::prepared_demux_seek::FakePreparedDemuxSeekPort;
use super::test_support::{
    FakeDemuxer, SharedFakeVideoDecoderThread, fake_track, fake_video_packet_with_keyframe,
    install_ready_audio_runtime, seek_regression_fast_preroll_tick_config,
};
use super::*;
use crate::{
    AuthorizeInstallCommit, InstalledMediaStateRestore, InstalledMediaStateRestoreOutcome,
    InstalledPositionRestore, InstalledSubtitleRestore, InstalledTrackRestore,
    InstalledVolumeRestore, MediaInstallCompletion, MediaInstallControl,
    MediaInstallControlOutcome, MediaInstallPhase, MediaInstallReceipt, MediaInstallRequestId,
    MediaInstanceId, PlaybackIntent, PlaybackIntentRevision, PlaybackState, PlayerEvent,
    PreparedDemuxSeekPort, PreparedInitialPosition, PreparedInitialPositionError, PreparedMedia,
    StartedVideoBackend,
};

/// Prepared initial receipt должен дойти до presentation/audio commit-а без второго demux seek-а.
#[test]
fn prepared_initial_position_adopts_exact_generation_without_second_demux_seek() {
    let target_position = Duration::from_secs(355);
    let decode_anchor = Duration::from_secs(350);
    let landing_position = Duration::from_millis(355_040);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let synchronous_seek_log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut demuxer = FakeDemuxer::new(
        vec![video_track.clone(), audio_track],
        Some(Duration::from_secs(600)),
        Arc::clone(&synchronous_seek_log),
    );
    demuxer.push_packet(fake_video_packet_with_keyframe(
        video_track.id,
        decode_anchor,
        PacketKeyframe::Keyframe,
    ));
    demuxer.push_packet(fake_video_packet_with_keyframe(
        video_track.id,
        landing_position,
        PacketKeyframe::NotKeyframe,
    ));

    let prepared_seek_port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased_seek_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    let prepared_media = PreparedMedia::from_external_label("prepared-initial", Box::new(demuxer))
        .with_worker_receipted_demux_seek(erased_seek_port)
        .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
            target_position: MediaTime::from_duration(target_position),
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
            result: DemuxSeekResult {
                requested_position: MediaTime::from_duration(target_position),
                actual_position: MediaTime::from_duration(decode_anchor),
                actual_track_timestamp: None,
            },
        })
        .expect("valid prepared initial receipt должен пройти boundary validation");

    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(35_500).expect("request id должен быть non-zero"),
    );
    let (install_receipt, install_port) = MediaInstallReceipt::new(request_id);
    let mut session = PlayerSession::new();
    session.stage_prepared_media_install_compatibility(
        request_id,
        prepared_media,
        PlaybackIntent::StartPlaying,
        PlaybackIntentRevision::INITIAL,
        install_port,
    );
    assert_eq!(
        install_receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyToCommit { request_id })
    );
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = install_receipt
        .try_take_completion()
        .expect("prepared initial candidate должен установиться")
    else {
        panic!("valid prepared initial candidate не должен завершиться failure/cancellation");
    };

    assert!(session.seek_commit().is_some());
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
    assert!(prepared_seek_port.commands().is_empty());
    assert!(
        synchronous_seek_log
            .lock()
            .expect("seek log mutex")
            .is_empty()
    );

    // Stale request/instance и wrong-target restore не отбирают receipt у exact owner-а.
    let stale_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(request_id.get().saturating_add(1))
            .expect("stale request id должен быть non-zero"),
    );
    assert_eq!(
        restore_position(
            &mut session,
            stale_request_id,
            media_instance_id,
            target_position
        )
        .recv()
        .expect("stale request restore обязан получить owner outcome"),
        InstalledMediaStateRestoreOutcome::UnknownOrSupersededRequest
    );
    let stale_instance_id = MediaInstanceId::from_non_zero(
        NonZeroU64::new(media_instance_id.get().saturating_add(1))
            .expect("stale instance id должен быть non-zero"),
    );
    assert_eq!(
        restore_position(&mut session, request_id, stale_instance_id, target_position)
            .recv()
            .expect("stale restore обязан получить owner outcome"),
        InstalledMediaStateRestoreOutcome::StaleInstance
    );
    let wrong_target_outcome = restore_position(
        &mut session,
        request_id,
        media_instance_id,
        target_position + Duration::from_secs(1),
    )
    .recv()
    .expect("wrong-target restore обязан завершиться typed failure");
    assert!(matches!(
        wrong_target_outcome,
        InstalledMediaStateRestoreOutcome::Failed {
            stage: crate::InstalledMediaRestoreFailureStage::Position,
            error,
        } if error.kind == PlayerErrorKind::SeekUnavailable
    ));

    let decoder = SharedFakeVideoDecoderThread::new();
    decoder.decode_next_packet_as_frame(decode_anchor, 35_500);
    decoder.decode_next_packet_as_frame(landing_position, 35_501);
    session.set_video_backend(StartedVideoBackend::from_decoder_thread(
        "prepared-initial-test-backend",
        decoder,
    ));
    let audio_output = install_ready_audio_runtime(&mut session, 80.0, None);
    let _install_events = session.take_events();

    let restore_rx = restore_position(&mut session, request_id, media_instance_id, target_position);
    assert_eq!(restore_rx.try_recv(), Err(TryRecvError::Empty));
    let mut observed_events = session.take_events();
    assert_no_commit_before_av_readiness(&observed_events, target_position);

    for _ in 0..6 {
        let _tick = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_regression_fast_preroll_tick_config(),
        ));
        observed_events.extend(session.take_events());
        if session.seek_commit().is_none() {
            break;
        }
    }

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().current_position, target_position);
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 1);
    assert_eq!(
        restore_rx
            .recv()
            .expect("presentation+audio должны завершить exact restore"),
        InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
    );
    assert!(prepared_seek_port.commands().is_empty());
    assert!(
        synchronous_seek_log
            .lock()
            .expect("seek log mutex")
            .is_empty()
    );

    let target_frame_index = event_index(&observed_events, |event| {
        matches!(
            event,
            PlayerEvent::SeekTargetFramePresented(presentation)
                if presentation.target_position == target_position
                    && presentation.frame_pts == landing_position
        )
    });
    let generic_audio_index = event_index(&observed_events, |event| {
        matches!(event, PlayerEvent::AudioPlaybackResumed)
    });
    let seek_audio_index = event_index(&observed_events, |event| {
        matches!(event, PlayerEvent::AudioResumedAfterSeek(_))
    });
    let position_index = event_index(
        &observed_events,
        |event| matches!(event, PlayerEvent::PositionChanged(position) if *position == target_position),
    );
    let commit_index = event_index(&observed_events, |event| {
        matches!(event, PlayerEvent::SeekCommitted(_))
    });
    assert!(target_frame_index < generic_audio_index);
    assert!(generic_audio_index < seek_audio_index);
    assert!(seek_audio_index < position_index);
    assert!(position_index < commit_index);
}

/// Paused initial adoption доходит до target presentation, но ни разу не запускает audio.
#[test]
fn prepared_initial_position_start_paused_commits_without_audio_resume_or_second_seek() {
    let target_position = Duration::from_secs(355);
    let decode_anchor = Duration::from_secs(350);
    let landing_position = Duration::from_millis(355_040);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let synchronous_seek_log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut demuxer = FakeDemuxer::new(
        vec![video_track.clone(), audio_track],
        Some(Duration::from_secs(600)),
        Arc::clone(&synchronous_seek_log),
    );
    demuxer.push_packet(fake_video_packet_with_keyframe(
        video_track.id,
        decode_anchor,
        PacketKeyframe::Keyframe,
    ));
    demuxer.push_packet(fake_video_packet_with_keyframe(
        video_track.id,
        landing_position,
        PacketKeyframe::NotKeyframe,
    ));

    let prepared_seek_port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased_seek_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    let prepared_media =
        PreparedMedia::from_external_label("prepared-initial-paused", Box::new(demuxer))
            .with_worker_receipted_demux_seek(erased_seek_port)
            .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
                target_position: MediaTime::from_duration(target_position),
                landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
                result: DemuxSeekResult {
                    requested_position: MediaTime::from_duration(target_position),
                    actual_position: MediaTime::from_duration(decode_anchor),
                    actual_track_timestamp: None,
                },
            })
            .expect("valid paused initial receipt должен пройти boundary validation");

    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(35_501).expect("request id должен быть non-zero"),
    );
    let (install_receipt, install_port) = MediaInstallReceipt::new(request_id);
    let mut session = PlayerSession::new();
    session.stage_prepared_media_install_compatibility(
        request_id,
        prepared_media,
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
    );
    assert_eq!(
        install_receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyToCommit { request_id })
    );
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = install_receipt
        .try_take_completion()
        .expect("paused prepared initial candidate должен установиться")
    else {
        panic!("valid paused candidate не должен завершаться failure/cancellation");
    };

    assert_eq!(session.playback_state(), PlaybackState::Seeking);
    assert!(prepared_seek_port.commands().is_empty());
    let decoder = SharedFakeVideoDecoderThread::new();
    decoder.decode_next_packet_as_frame(decode_anchor, 35_502);
    decoder.decode_next_packet_as_frame(landing_position, 35_503);
    session.set_video_backend(StartedVideoBackend::from_decoder_thread(
        "prepared-initial-paused-test-backend",
        decoder,
    ));
    let audio_output = install_ready_audio_runtime(&mut session, 80.0, None);
    let mut observed_events = session.take_events();

    let restore_rx = restore_position(&mut session, request_id, media_instance_id, target_position);
    assert_eq!(restore_rx.try_recv(), Err(TryRecvError::Empty));
    observed_events.extend(session.take_events());
    assert_no_commit_before_av_readiness(&observed_events, target_position);

    let mut presented_frame_count = 0;
    for _ in 0..6 {
        let tick = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_regression_fast_preroll_tick_config(),
        ));
        presented_frame_count += tick.video_frames_presented;
        observed_events.extend(session.take_events());
        if session.seek_commit().is_none() {
            break;
        }
    }

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().current_position, target_position);
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(presented_frame_count, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(landing_position)
    );
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 0);
    assert_eq!(
        restore_rx
            .recv()
            .expect("target presentation должна завершить paused exact restore"),
        InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
    );
    assert!(prepared_seek_port.commands().is_empty());
    assert!(
        synchronous_seek_log
            .lock()
            .expect("seek log mutex")
            .is_empty()
    );

    let target_frame_index = event_index(&observed_events, |event| {
        matches!(
            event,
            PlayerEvent::SeekTargetFramePresented(presentation)
                if presentation.target_position == target_position
                    && presentation.frame_pts == landing_position
        )
    });
    let position_index = event_index(
        &observed_events,
        |event| matches!(event, PlayerEvent::PositionChanged(position) if *position == target_position),
    );
    let commit_index = event_index(&observed_events, |event| {
        matches!(
            event,
            PlayerEvent::SeekCommitted(commit)
                if commit.target_position == target_position
                    && commit.resume_intent == PlaybackResumeIntent::Pause
        )
    });
    assert!(target_frame_index < position_index);
    assert!(position_index < commit_index);
    assert!(!observed_events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::AudioPlaybackResumed | PlayerEvent::AudioResumedAfterSeek(_)
        )
    }));
}

/// Anchor после target-а нельзя даже поместить в `PreparedMedia` contract.
#[test]
fn prepared_initial_position_rejects_anchor_after_target() {
    let target = MediaTime::from_duration(Duration::from_secs(10));
    let actual = MediaTime::from_duration(Duration::from_secs(11));
    let demuxer = FakeDemuxer::new(
        Vec::new(),
        Some(Duration::from_secs(20)),
        Arc::new(std::sync::Mutex::new(Vec::new())),
    );
    let error = PreparedMedia::from_external_label("invalid-prepared-initial", Box::new(demuxer))
        .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
            target_position: target,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
            result: DemuxSeekResult {
                requested_position: target,
                actual_position: actual,
                actual_track_timestamp: None,
            },
        })
        .err()
        .expect("anchor after target должен быть отклонён");
    assert_eq!(
        error,
        PreparedInitialPositionError::ActualPositionAfterTarget {
            target_position: target,
            actual_position: actual,
        }
    );
}

/// Opt-in post-target contract симметрично не принимает старый pre-target decoder anchor.
#[test]
fn prepared_post_target_position_rejects_anchor_before_target() {
    let target = MediaTime::from_duration(Duration::from_secs(10));
    let actual = MediaTime::from_duration(Duration::from_secs(9));
    let demuxer = FakeDemuxer::new(
        Vec::new(),
        Some(Duration::from_secs(20)),
        Arc::new(std::sync::Mutex::new(Vec::new())),
    );
    let error =
        PreparedMedia::from_external_label("invalid-post-target-initial", Box::new(demuxer))
            .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
                target_position: target,
                landing_policy: crate::PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget,
                result: DemuxSeekResult {
                    requested_position: target,
                    actual_position: actual,
                    actual_track_timestamp: None,
                },
            })
            .err()
            .expect("anchor before target должен быть отклонён post-target contract-ом");
    assert_eq!(
        error,
        PreparedInitialPositionError::ActualPositionBeforeTarget {
            target_position: target,
            actual_position: actual,
        }
    );
}

/// Receipt другого request target-а нельзя перелабелить под prepared target.
#[test]
fn prepared_initial_position_rejects_requested_target_mismatch() {
    let target = MediaTime::from_duration(Duration::from_secs(10));
    let requested = MediaTime::from_duration(Duration::from_secs(9));
    let demuxer = FakeDemuxer::new(
        Vec::new(),
        Some(Duration::from_secs(20)),
        Arc::new(std::sync::Mutex::new(Vec::new())),
    );
    let error =
        PreparedMedia::from_external_label("mismatched-prepared-initial", Box::new(demuxer))
            .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
                target_position: target,
                landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
                result: DemuxSeekResult {
                    requested_position: requested,
                    actual_position: requested,
                    actual_track_timestamp: None,
                },
            })
            .err()
            .expect("receipt другого target-а должен быть отклонён");
    assert_eq!(
        error,
        PreparedInitialPositionError::RequestedPositionMismatch {
            target_position: target,
            requested_position: requested,
        }
    );
}

/// Superseding seek generation запрещает позднему restore усыновить старый receipt.
#[test]
fn prepared_initial_position_rejects_stale_seek_generation() {
    let target_position = Duration::from_secs(5);
    let demuxer = FakeDemuxer::new(
        Vec::new(),
        Some(Duration::from_secs(20)),
        Arc::new(std::sync::Mutex::new(Vec::new())),
    );
    let prepared_media = PreparedMedia::from_external_label("stale-generation", Box::new(demuxer))
        .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
            target_position: MediaTime::from_duration(target_position),
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
            result: DemuxSeekResult {
                requested_position: MediaTime::from_duration(target_position),
                actual_position: MediaTime::from_duration(target_position),
                actual_track_timestamp: None,
            },
        })
        .expect("exact target/anchor должен быть valid");
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(5_005).expect("request id должен быть non-zero"),
    );
    let (install_receipt, install_port) = MediaInstallReceipt::new(request_id);
    let mut session = PlayerSession::new();
    session.stage_prepared_media_install_compatibility(
        request_id,
        prepared_media,
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
    );
    assert_eq!(
        install_receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyToCommit { request_id })
    );
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = install_receipt
        .try_take_completion()
        .expect("candidate должен установиться до generation check")
    else {
        panic!("candidate не должен завершаться failure/cancellation");
    };

    let prepared_generation = session
        .seek_commit()
        .expect("prepared position должна начать decoder landing")
        .generation;
    let superseding_generation = session.pipeline.begin_seek_generation();
    assert_ne!(superseding_generation, prepared_generation);

    let outcome = restore_position(&mut session, request_id, media_instance_id, target_position)
        .recv()
        .expect("stale generation должна завершиться typed failure");
    assert!(matches!(
        outcome,
        InstalledMediaStateRestoreOutcome::Failed {
            stage: crate::InstalledMediaRestoreFailureStage::Position,
            error,
        } if error.kind == PlayerErrorKind::SeekUnavailable
            && error.message.contains("generation is stale")
    ));
}

/// Запускает exact-instance restore и возвращает его request-owned outcome receiver.
fn restore_position(
    session: &mut PlayerSession,
    request_id: MediaInstallRequestId,
    media_instance_id: MediaInstanceId,
    expected_target: Duration,
) -> crossbeam_channel::Receiver<InstalledMediaStateRestoreOutcome> {
    let (restore_tx, restore_rx) = bounded(1);
    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::AdoptPreparedInitialPosition { expected_target },
        },
        restore_tx,
    );
    restore_rx
}

/// До surface/audio gates player не имеет права публиковать user-visible success.
fn assert_no_commit_before_av_readiness(events: &[PlayerEvent], target_position: Duration) {
    assert!(!events.iter().any(|event| {
        matches!(event, PlayerEvent::PositionChanged(position) if *position == target_position)
            || matches!(event, PlayerEvent::SeekCommitted(_))
            || matches!(
                event,
                PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
            )
    }));
}

/// Возвращает exact index marker-а для проверки строгого lifecycle ordering.
fn event_index(events: &[PlayerEvent], predicate: impl Fn(&PlayerEvent) -> bool) -> usize {
    events
        .iter()
        .position(predicate)
        .unwrap_or_else(|| panic!("ожидаемый lifecycle event отсутствует: {events:#?}"))
}
