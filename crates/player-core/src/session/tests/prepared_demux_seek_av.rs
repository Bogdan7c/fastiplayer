//! Vertical worker-receipted seek regressions до video presentation и audio resume.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use media_core::{DemuxSeekResult, MediaTime, PacketKeyframe, TrackKind};

use super::prepared_demux_seek::{FakePreparedDemuxSeekPort, receipted_video_session};
use super::test_support::{
    SeekRegressionHarness, fake_track, fake_video_packet_with_keyframe,
    install_ready_audio_runtime, scripted_seek_demuxer,
};
use super::tracing_capture::install_tracing_capture;
use crate::{
    PlaybackState, PlayerCommand, PlayerEvent, PlayerTickConfig, PlayerTickContext,
    PreparedDemuxSeekMode, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, SeekMode, SeekRequest,
    SeekTarget,
};

#[test]
fn worker_receipted_av_seek_preserves_exact_topology_and_commits_after_audio_play() {
    let (captured_tracing, _tracing_guard) = install_tracing_capture();
    let target_position = Duration::from_secs(8);
    let actual_position = Duration::from_secs(5);
    let landing_position = Duration::from_millis(8_040);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let tracks = vec![video_track.clone(), audio_track.clone()];
    let packets = vec![
        fake_video_packet_with_keyframe(video_track.id, actual_position, PacketKeyframe::Keyframe),
        fake_video_packet_with_keyframe(
            video_track.id,
            landing_position,
            PacketKeyframe::NotKeyframe,
        ),
    ];
    let demuxer = scripted_seek_demuxer(tracks.clone(), target_position, actual_position, packets);
    let mut harness = SeekRegressionHarness::new(tracks.clone(), demuxer);
    let audio_output = install_ready_audio_runtime(&mut harness.session, 80.0, None);
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    harness
        .decoder
        .decode_next_packet_as_frame(actual_position, 921);
    harness
        .decoder
        .decode_next_packet_as_frame(landing_position, 922);

    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .expect("A/V regression должна стартовать в playing intent");
    let _events_before_seek = harness.session.take_events();
    harness.start_final_seek(MediaTime::from_duration(target_position));
    let artificial_public_accepted_at = Instant::now() - Duration::from_millis(1_500);
    harness
        .session
        .prepared_demux_seek
        .set_pending_public_accepted_at_for_tests(artificial_public_accepted_at);
    let commands = port.commands();
    let [(request_id, _request)] = commands.as_slice() else {
        panic!("A/V seek должен создать один worker request");
    };
    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(target_position),
            actual_position: MediaTime::from_duration(actual_position),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();

    let seek_generation = harness
        .session
        .seek_commit()
        .expect("receipt должна открыть final seek")
        .generation;
    let seek_commit = harness
        .session
        .seek_commit()
        .expect("receipt должна сохранить оба monotonic origin-а");
    assert_eq!(
        seek_commit.public_accepted_at, artificial_public_accepted_at,
        "public origin обязан пережить worker round-trip"
    );
    assert!(
        seek_commit
            .started_at
            .saturating_duration_since(seek_commit.public_accepted_at)
            >= Duration::from_millis(1_500),
        "receipt origin не должен заменять artificial public origin"
    );
    let timeout_config = PlayerTickConfig {
        seek_commit_timeout: Duration::from_secs(1),
        ..PlayerTickConfig::default()
    };
    harness.session.finish_seek_commit_if_ready(
        seek_commit.started_at + timeout_config.seek_commit_timeout - Duration::from_millis(1),
        &timeout_config,
    );
    assert!(
        harness.session.seek_commit().is_some(),
        "public age > timeout не должен преждевременно запускать receipt timeout"
    );
    let pause_count_before_update = audio_output.pause_count.load(Ordering::Relaxed);
    let clear_count_before_update = audio_output.clear_count.load(Ordering::Relaxed);
    harness
        .session
        .handle_demux_track_list_update(media_core::DemuxTrackListUpdate::new(
            tracks,
            Some(Duration::from_secs(30)),
        ));

    assert_eq!(harness.session.pipeline.seek_generation(), seek_generation);
    assert!(harness.session.pipeline.has_audio_decoder());
    assert!(harness.session.pipeline.has_audio_output());
    assert_eq!(
        audio_output.pause_count.load(Ordering::Relaxed),
        pause_count_before_update
    );
    assert_eq!(
        audio_output.clear_count.load(Ordering::Relaxed),
        clear_count_before_update
    );

    for _ in 0..6 {
        harness.tick_once_fast_preroll();
        if harness.session.seek_commit().is_none() {
            break;
        }
    }

    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(landing_position),
        "decode anchor до target не должен стать presented frame"
    );
    assert_eq!(
        audio_output.play_count.load(Ordering::Relaxed),
        2,
        "audio play вызывается при исходном Play и перед seek commit"
    );
    assert_eq!(
        harness.session.snapshot().playback_state,
        PlaybackState::Playing
    );
    assert_eq!(harness.session.snapshot().current_position, target_position);

    let events = harness.session.take_events();
    let target_frame_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                PlayerEvent::SeekTargetFramePresented(presentation)
                    if presentation.target_position == target_position
                        && presentation.frame_pts == landing_position
            )
        })
        .expect("target frame event должен дойти до player presentation");
    let audio_resume_index = events
        .iter()
        .position(|event| matches!(event, PlayerEvent::AudioResumedAfterSeek(_)))
        .expect("audio play acceptance должен быть видимым");
    let position_index = events
        .iter()
        .position(|event| {
            matches!(event, PlayerEvent::PositionChanged(position) if *position == target_position)
        })
        .expect("position публикуется после audio play");
    let commit_index = events
        .iter()
        .position(|event| matches!(event, PlayerEvent::SeekCommitted(_)))
        .expect("A/V seek должен завершиться commit-ом");
    let playing_index = events
        .iter()
        .rposition(|event| {
            matches!(
                event,
                PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
            )
        })
        .expect("Playing публикуется после commit-а");
    assert!(target_frame_index < audio_resume_index);
    assert!(audio_resume_index < position_index);
    assert!(position_index < commit_index);
    assert!(commit_index < playing_index);

    let committed_position = harness.session.snapshot().current_position;
    audio_output.clock.record_played(48_000 * 2 / 10);
    let _position_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..PlayerTickConfig::default()
        },
    ));
    assert!(
        harness.session.snapshot().current_position > committed_position,
        "после A/V commit позиция должна продолжить движение по audio clock"
    );

    let trace = captured_tracing.contents();
    let demux_marker = trace
        .find("Demux seek transaction accepted")
        .expect("authoritative demux accept обязан публиковаться");
    let decoded_marker = trace
        .find("First post-seek decoded frame observed")
        .expect("target decode обязан публиковаться до presentation");
    let presented_marker = trace
        .find("First post-seek presented frame observed")
        .expect("functional seek обязан публиковать target presentation marker");
    let audio_marker = trace
        .find("Audio play accepted before final seek commit")
        .expect("selected audio обязан публиковать accepted play marker");
    let commit_marker = trace
        .find("Final seek commit завершён")
        .expect("commit marker обязан следовать после presentation/audio");
    let progress_marker = trace
        .find("Post-seek position progress observed")
        .expect("первый положительный clock delta обязан дать one-shot marker");
    assert!(demux_marker < decoded_marker);
    assert!(decoded_marker < presented_marker);
    assert!(presented_marker < audio_marker);
    assert!(audio_marker < commit_marker);
    assert!(commit_marker < progress_marker);
    assert!(trace.contains(&format!("generation={seek_generation}")));
    assert!(trace.contains("presented_pre_target_frames=0"));
    assert!(trace.contains("available_audio_track_count=1"));
    assert!(trace.contains("audio_ready=true"));
    let decoded_event = trace
        .lines()
        .find(|line| line.contains("First post-seek decoded frame observed"))
        .expect("decoded acceptance event обязан существовать");
    assert!(
        decoded_event.starts_with("level=INFO "),
        "decoded acceptance proof обязан переживать production INFO filter: {decoded_event}"
    );
    let public_to_presented_ms = marker_u128_field(
        &trace,
        "First post-seek presented frame observed",
        "public_to_presented_ms",
    );
    let receipt_to_presented_ms = marker_u128_field(
        &trace,
        "First post-seek presented frame observed",
        "receipt_to_presented_ms",
    );
    let public_to_audio_ms = marker_u128_field(
        &trace,
        "Audio play accepted before final seek commit",
        "public_to_audio_ms",
    );
    let receipt_to_audio_ms = marker_u128_field(
        &trace,
        "Audio play accepted before final seek commit",
        "receipt_to_audio_ms",
    );
    assert!(public_to_presented_ms >= 1_500);
    assert!(public_to_audio_ms >= 1_500);
    assert!(receipt_to_presented_ms < public_to_presented_ms);
    assert!(receipt_to_audio_ms < public_to_audio_ms);
    assert!(trace.contains("public_to_commit_ms="));
    assert!(trace.contains("receipt_to_commit_ms="));
    assert!(trace.contains("public_to_progress_ms="));
    assert!(trace.contains("receipt_to_progress_ms="));
    assert!(
        marker_u128_field(
            &trace,
            "Post-seek position progress observed",
            "progress_delta_us",
        ) > 0,
        "progress marker обязан сохранять положительный sub-ms delta без округления в ноль"
    );
    assert_eq!(
        trace
            .matches("Post-seek position progress observed")
            .count(),
        1,
        "progress telemetry не должна спамить на каждом tick"
    );
}

/// Читает одно числовое structured field из exact acceptance marker-а.
fn marker_u128_field(trace: &str, marker: &str, field_name: &str) -> u128 {
    let marker_line = trace
        .lines()
        .find(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("marker `{marker}` отсутствует в captured tracing"));
    let field_prefix = format!("{field_name}=");
    marker_line
        .split_whitespace()
        .find_map(|field| field.strip_prefix(&field_prefix))
        .unwrap_or_else(|| panic!("field `{field_name}` отсутствует в marker `{marker}`"))
        .parse()
        .unwrap_or_else(|_| panic!("field `{field_name}` не является u128"))
}

/// Keyframe-before может законно показать container landing до public target, но acceptance
/// telemetry не имеет права назвать такой кадр target/post-target либо выдумать нулевой counter.
#[test]
fn presented_pre_target_violation_never_publishes_zero_target_proof() {
    let (captured_tracing, _tracing_guard) = install_tracing_capture();
    let target_position = Duration::from_secs(8);
    let actual_position = Duration::from_secs(5);
    let video_track = fake_track(1, TrackKind::Video);
    let tracks = vec![video_track.clone()];
    let packets = vec![fake_video_packet_with_keyframe(
        video_track.id,
        actual_position,
        PacketKeyframe::Keyframe,
    )];
    let demuxer = scripted_seek_demuxer(tracks.clone(), target_position, actual_position, packets);
    let mut harness = SeekRegressionHarness::new(tracks, demuxer);
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    harness
        .decoder
        .decode_next_packet_as_frame(actual_position, 923);

    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .expect("video-only regression должна стартовать в playing intent");
    let _events_before_seek = harness.session.take_events();
    harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_duration(target_position)),
            mode: SeekMode::KeyframeBefore,
        }))
        .expect("public keyframe-before seek должен пройти session boundary");
    let commands = port.commands();
    let [(request_id, _request)] = commands.as_slice() else {
        panic!("keyframe-before seek должен создать один worker request");
    };
    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(target_position),
            actual_position: MediaTime::from_duration(actual_position),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();

    for _ in 0..4 {
        harness.tick_once_fast_preroll();
        if harness.session.seek_commit().is_none() {
            break;
        }
    }

    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual_position),
        "fixture должен действительно представить pre-target landing"
    );
    let trace = captured_tracing.contents();
    assert!(trace.contains("Final seek commit завершён"));
    assert!(trace.contains("presented_pre_target_frames=1"));
    assert!(!trace.contains("First post-seek presented frame observed"));
    assert!(!trace.contains("presented_pre_target_frames=0"));
}

#[test]
fn worker_receipted_changed_topology_keeps_full_generation_reset() {
    let (mut session, port, _synchronous_seek_log) = receipted_video_session();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .expect("worker seek command");
    let commands = port.commands();
    let [(request_id, _request)] = commands.as_slice() else {
        panic!("worker seek должен создать один request");
    };
    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(8),
            actual_position: MediaTime::from_secs(5),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();
    let generation_before_update = session.pipeline.seek_generation();

    session.handle_demux_track_list_update(media_core::DemuxTrackListUpdate::new(
        vec![fake_track(3, TrackKind::Video)],
        Some(Duration::from_secs(30)),
    ));

    assert_ne!(session.pipeline.seek_generation(), generation_before_update);
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(crate::TrackId::new(3))
    );
}
