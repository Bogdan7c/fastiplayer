use std::num::NonZeroU64;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use media_core::{TrackId, TrackKind};
use player_core::{
    MediaSummary, PlaybackResumeIntent, PlayerEvent, PlayerSnapshot, SeekAudioResumeInfo,
    SeekCommitInfo, SeekRequest, SeekTargetFramePresentation, TrackSummarySnapshot,
};
use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_present_core::VideoPresentFrameIdentity;

use super::*;

const RESTORE_TARGET: Duration = Duration::from_secs(355);

fn media_instance(raw_id: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(
        NonZeroU64::new(raw_id).expect("test media instance должен быть non-zero"),
    )
}

fn media_opened() -> PlayerEvent {
    PlayerEvent::MediaOpened(MediaSummary {
        title: None,
        source_label: "startup-test".to_owned(),
        duration: Some(Duration::from_secs(600)),
    })
}

fn video_only_snapshot(media_instance_id: MediaInstanceId) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.media_instance_id = Some(media_instance_id);
    snapshot.tracks = vec![TrackSummarySnapshot {
        id: TrackId::new(1),
        kind: TrackKind::Video,
        codec_id: "V_TEST".to_owned(),
        sample_rate: None,
        channels: None,
        duration: None,
        video: None,
        video_color_summary: None,
    }];
    snapshot
}

fn audio_only_snapshot(media_instance_id: MediaInstanceId) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.media_instance_id = Some(media_instance_id);
    snapshot.tracks = vec![TrackSummarySnapshot {
        id: TrackId::new(1),
        kind: TrackKind::Audio,
        codec_id: "A_TEST".to_owned(),
        sample_rate: Some(48_000),
        channels: Some(2),
        duration: None,
        video: None,
        video_color_summary: None,
    }];
    snapshot
}

fn frame_identity(render_generation: u64, pts: Duration) -> VideoPresentFrameIdentity {
    let decoded_frame = DecodedFrame {
        generation: 7,
        pts,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(42),
        diagnostics: VideoFrameDiagnostics::default(),
    };
    VideoPresentFrameIdentity::from_decoded_frame(render_generation, &decoded_frame)
}

fn bind_media(
    tracker: &mut StartupReadinessTracker,
    media_instance_id: MediaInstanceId,
    observed_at: Instant,
) {
    tracker.note_player_event(Some(media_instance_id), &media_opened(), observed_at);
    let mut snapshot = video_only_snapshot(media_instance_id);
    snapshot.render_generation = 11;
    tracker.reconcile_tracks(&snapshot, observed_at);
}

fn matching_restore_events(
    tracker: &mut StartupReadinessTracker,
    media_instance_id: MediaInstanceId,
    started_at: Instant,
    resume_intent: PlaybackResumeIntent,
) {
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::SeekRequested(SeekRequest::accurate(RESTORE_TARGET)),
        started_at + Duration::from_millis(2),
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::SeekTargetFramePresented(SeekTargetFramePresentation {
            target_position: RESTORE_TARGET,
            frame_pts: RESTORE_TARGET + Duration::from_millis(40),
        }),
        started_at + Duration::from_millis(3),
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::SeekCommitted(SeekCommitInfo {
            target_position: RESTORE_TARGET,
            actual_position: RESTORE_TARGET - Duration::from_secs(5),
            resume_intent,
        }),
        started_at + Duration::from_millis(4),
    );
}

#[test]
fn wrong_target_or_new_seek_terminally_aborts_startup_attempt() {
    let started_at = Instant::now();
    let media_instance_id = media_instance(1);
    let mut tracker = StartupReadinessTracker::new(started_at);
    let expectation = StartupReadinessExpectation::new(
        StartupMediaOpenKind::Restore,
        StartupTargetExpectation::Restore {
            target_position: RESTORE_TARGET,
        },
        StartupPlaybackExpectation::Playing,
        StartupAudioExpectation::Required,
    );
    tracker.begin_attempt(expectation, started_at);
    bind_media(
        &mut tracker,
        media_instance_id,
        started_at + Duration::from_millis(1),
    );

    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::SeekRequested(SeekRequest::accurate(Duration::from_secs(60))),
        started_at + Duration::from_millis(2),
    );
    assert!(!tracker.has_active_attempt());

    tracker.begin_attempt(expectation, started_at + Duration::from_millis(10));
    bind_media(
        &mut tracker,
        media_instance_id,
        started_at + Duration::from_millis(11),
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::SeekRequested(SeekRequest::accurate(RESTORE_TARGET)),
        started_at + Duration::from_millis(12),
    );
    assert!(
        tracker.has_active_attempt(),
        "repeated exact target remains safe"
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::SeekTargetFramePresented(SeekTargetFramePresentation {
            target_position: Duration::from_secs(180),
            frame_pts: Duration::from_secs(180),
        }),
        started_at + Duration::from_millis(13),
    );
    assert!(!tracker.has_active_attempt());
}

#[test]
fn paused_restore_completes_on_target_surface_and_audio_output_without_resume() {
    let started_at = Instant::now();
    let media_instance_id = media_instance(2);
    let mut tracker = StartupReadinessTracker::new(started_at);
    tracker.begin_attempt(
        StartupReadinessExpectation::new(
            StartupMediaOpenKind::Restore,
            StartupTargetExpectation::Restore {
                target_position: RESTORE_TARGET,
            },
            StartupPlaybackExpectation::Paused,
            StartupAudioExpectation::Required,
        ),
        started_at,
    );
    bind_media(
        &mut tracker,
        media_instance_id,
        started_at + Duration::from_millis(1),
    );
    matching_restore_events(
        &mut tracker,
        media_instance_id,
        started_at,
        PlaybackResumeIntent::Pause,
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::AudioOutputReady,
        started_at + Duration::from_millis(5),
    );
    tracker.note_surface_frame_presented(
        Some(media_instance_id),
        frame_identity(11, RESTORE_TARGET + Duration::from_millis(40)),
        11,
        started_at + Duration::from_millis(6),
    );

    assert!(!tracker.has_active_attempt());
}

#[test]
fn abort_prevents_late_media_and_surface_events_from_rebinding() {
    let started_at = Instant::now();
    let media_instance_id = media_instance(3);
    let mut tracker = StartupReadinessTracker::new(started_at);
    tracker.begin_attempt(
        StartupReadinessExpectation::new(
            StartupMediaOpenKind::Cli,
            StartupTargetExpectation::Beginning,
            StartupPlaybackExpectation::Playing,
            StartupAudioExpectation::NotPresent,
        ),
        started_at,
    );
    tracker.abort_attempt(
        StartupReadinessAbortReason::PreparationFailed,
        started_at + Duration::from_millis(1),
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &media_opened(),
        started_at + Duration::from_millis(2),
    );
    tracker.note_surface_frame_presented(
        Some(media_instance_id),
        frame_identity(11, Duration::ZERO),
        11,
        started_at + Duration::from_millis(3),
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::AudioPlaybackResumed,
        started_at + Duration::from_millis(4),
    );

    assert!(!tracker.has_active_attempt());
}

#[test]
fn unknown_audio_does_not_infer_absence_from_video_only_snapshot() {
    let started_at = Instant::now();
    let media_instance_id = media_instance(4);
    let mut tracker = StartupReadinessTracker::new(started_at);
    tracker.begin_attempt(
        StartupReadinessExpectation::new(
            StartupMediaOpenKind::Cli,
            StartupTargetExpectation::Beginning,
            StartupPlaybackExpectation::Paused,
            StartupAudioExpectation::Unknown,
        ),
        started_at,
    );
    bind_media(
        &mut tracker,
        media_instance_id,
        started_at + Duration::from_millis(1),
    );
    tracker.note_surface_frame_presented(
        Some(media_instance_id),
        frame_identity(11, Duration::ZERO),
        11,
        started_at + Duration::from_millis(2),
    );

    let attempt = tracker
        .active_attempt
        .as_ref()
        .expect("Unknown audio не должен публиковать readiness");
    assert_eq!(attempt.expectation.audio, StartupAudioExpectation::Unknown);

    tracker.note_prepared_consumer_proof(
        StartupPreparedConsumerProof {
            audio: StartupAudioProof::NotPresent,
            video: StartupVideoProof::Required,
        },
        started_at + Duration::from_millis(3),
    );
    assert!(
        !tracker.has_active_attempt(),
        "только authoritative NotPresent proof закрывает audio-less gate"
    );
}

#[test]
fn playing_audio_only_completes_without_inventing_surface_presentation() {
    let started_at = Instant::now();
    let media_instance_id = media_instance(41);
    let mut tracker = StartupReadinessTracker::new(started_at);
    tracker.begin_attempt(
        StartupReadinessExpectation::new(
            StartupMediaOpenKind::Cli,
            StartupTargetExpectation::Beginning,
            StartupPlaybackExpectation::Playing,
            StartupAudioExpectation::Unknown,
        ),
        started_at,
    );
    tracker.note_prepared_consumer_proof(
        StartupPreparedConsumerProof {
            audio: StartupAudioProof::Required,
            video: StartupVideoProof::NotPresent,
        },
        started_at + Duration::from_millis(1),
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &media_opened(),
        started_at + Duration::from_millis(2),
    );
    tracker.reconcile_tracks(
        &audio_only_snapshot(media_instance_id),
        started_at + Duration::from_millis(2),
    );
    tracker.note_audio_playback_resumed(
        Some(media_instance_id),
        started_at + Duration::from_millis(3),
    );

    assert!(
        !tracker.has_active_attempt(),
        "authoritative audio-only topology не должна ждать несуществующий video surface"
    );
}

#[test]
fn playing_restore_requires_exact_target_surface_and_audio_resume() {
    let started_at = Instant::now();
    let media_instance_id = media_instance(5);
    let mut tracker = StartupReadinessTracker::new(started_at);
    tracker.begin_attempt(
        StartupReadinessExpectation::new(
            StartupMediaOpenKind::Restore,
            StartupTargetExpectation::Restore {
                target_position: RESTORE_TARGET,
            },
            StartupPlaybackExpectation::Playing,
            StartupAudioExpectation::Required,
        ),
        started_at,
    );
    bind_media(
        &mut tracker,
        media_instance_id,
        started_at + Duration::from_millis(1),
    );
    matching_restore_events(
        &mut tracker,
        media_instance_id,
        started_at,
        PlaybackResumeIntent::Play,
    );
    tracker.note_surface_frame_presented(
        Some(media_instance_id),
        frame_identity(11, RESTORE_TARGET - Duration::from_millis(1)),
        11,
        started_at + Duration::from_millis(5),
    );
    assert!(
        tracker
            .active_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.surface_presented_at.is_none()),
        "pre-target surface не должен закрывать video gate"
    );
    tracker.note_surface_frame_presented(
        Some(media_instance_id),
        frame_identity(11, RESTORE_TARGET + Duration::from_millis(50)),
        11,
        started_at + Duration::from_millis(6),
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::AudioOutputReady,
        started_at + Duration::from_millis(7),
    );
    assert!(tracker.has_active_attempt());

    // Correlation остаётся на requested target, даже когда native HLS честно начинает с post-target RAP.
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::AudioResumedAfterSeek(SeekAudioResumeInfo {
            target_position: RESTORE_TARGET,
            playback_position: RESTORE_TARGET + Duration::from_secs(5),
        }),
        started_at + Duration::from_millis(7),
    );
    assert!(tracker.has_active_attempt());

    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::AudioPlaybackResumed,
        started_at + Duration::from_millis(8),
    );
    assert!(!tracker.has_active_attempt());
}

#[test]
fn paused_restore_aborts_if_seek_audio_resumes() {
    let started_at = Instant::now();
    let media_instance_id = media_instance(6);
    let mut tracker = StartupReadinessTracker::new(started_at);
    tracker.begin_attempt(
        StartupReadinessExpectation::new(
            StartupMediaOpenKind::Restore,
            StartupTargetExpectation::Restore {
                target_position: RESTORE_TARGET,
            },
            StartupPlaybackExpectation::Paused,
            StartupAudioExpectation::Required,
        ),
        started_at,
    );
    bind_media(
        &mut tracker,
        media_instance_id,
        started_at + Duration::from_millis(1),
    );
    tracker.note_player_event(
        Some(media_instance_id),
        &PlayerEvent::AudioResumedAfterSeek(SeekAudioResumeInfo {
            target_position: RESTORE_TARGET,
            playback_position: RESTORE_TARGET,
        }),
        started_at + Duration::from_millis(2),
    );

    assert!(!tracker.has_active_attempt());
}
