//! Вертикальные player-session regressions для opt-in post-target landing policy.
//!
//! Эта policy предназначена только для source owner-а, который явно выбрал ближайший
//! доказанный post-target RAP. Обычные Accurate/worker-receipted media продолжают
//! использовать прежний decode-before-target contract.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use media_core::{DemuxSeekResult, MediaTime, PacketKeyframe, TrackKind};

use super::prepared_demux_seek::FakePreparedDemuxSeekPort;
use super::test_support::{
    SeekRegressionHarness, fake_audio_packet, fake_track, fake_video_packet_with_keyframe,
    install_ready_audio_runtime, scripted_seek_demuxer,
};
use super::*;
use crate::{
    PlaybackState, PlayerEvent, PlayerTickConfig, PlayerTickContext,
    PreparedDemuxSeekLandingPolicy, PreparedDemuxSeekMode, PreparedDemuxSeekOutcome,
    PreparedDemuxSeekPort,
};

/// HLS opt-in receipt обязан коммитить фактически показанный post-target frame, а не лгать target-ом.
#[test]
fn hls_post_target_receipt_commits_only_after_presented_frame_and_audio_resume() {
    // Public target остаётся request identity, а actual — доказанный первым RAP следующего segment-а.
    let requested_target = Duration::from_secs(55);
    let authoritative_actual = Duration::from_secs(60);
    // Этот кадр уже позже public target, но раньше доказанного actual и потому всё ещё запрещён.
    let frame_before_authoritative_actual = Duration::from_millis(57_500);
    let presented_landing = Duration::from_millis(60_040);

    // Muxed A/V topology заставляет пройти не только video gate, но и настоящий audio resume gate.
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let packets = vec![
        fake_audio_packet(
            audio_track.id,
            authoritative_actual,
            Duration::from_millis(40),
        ),
        fake_video_packet_with_keyframe(
            video_track.id,
            presented_landing,
            PacketKeyframe::Keyframe,
        ),
    ];
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        requested_target,
        authoritative_actual,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
    // Test-support media имеет короткий default timeline; scenario проверяет реальные 55 -> 60 секунд.
    harness
        .session
        .set_snapshot_duration(Some(Duration::from_secs(120)));

    // Worker port и policy включаются явно: generic/legacy media эту семантику не наследуют.
    let prepared_seek_port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased_seek_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased_seek_port,
            landing_policy: PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget,
        });

    // Packet path возвращает допустимый landing; запрещённый frame ниже добавляется отдельно после receipt-а.
    harness
        .decoder
        .decode_next_packet_as_frame(presented_landing, 60_001);
    let audio_output = install_ready_audio_runtime(&mut harness.session, 80.0, None);

    // Seek начинается из Playing, чтобы regression включала pause/clear и последующий audio play.
    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .expect("pre-seek Play должен пройти session boundary");
    let _events_before_seek = harness.session.take_events();
    harness.start_final_seek(MediaTime::from_duration(requested_target));

    // До worker receipt-а запрещены commit state, UI position и скрытый synchronous demux seek.
    let commands = prepared_seek_port.commands();
    let [(request_id, request)] = commands.as_slice() else {
        panic!("HLS public seek должен создать ровно один authoritative worker request");
    };
    assert_eq!(request.timestamp, requested_target);
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.snapshot().current_position, Duration::ZERO);
    assert!(harness.seek_requests().is_empty());

    // Receipt сохраняет requested target для correlation, но объявляет отдельный authoritative actual.
    prepared_seek_port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(requested_target),
            actual_position: MediaTime::from_duration(authoritative_actual),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();
    // Frame текущей generation выше public target, но ниже actual: presentation обязана его подавить.
    harness.push_decoded_frame(frame_before_authoritative_actual, 55_001, 0);

    // Сам receipt ещё не является user-visible success: кадр не представлен и audio не возобновлено.
    let active_commit = harness
        .session
        .seek_commit()
        .expect("authoritative receipt должен открыть existing seek commit lifecycle");
    assert_eq!(
        active_commit.target_position.as_duration(),
        requested_target
    );
    assert_eq!(
        active_commit.actual_position.as_duration(),
        authoritative_actual
    );
    assert_eq!(harness.session.snapshot().current_position, Duration::ZERO);
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 1);
    let mut observed_events = harness.session.take_events();
    assert_no_successful_position_commit(&observed_events, requested_target, presented_landing);

    // Production tick path должен декодировать, скрыть 57.5 s и представить ровно landing frame 60.04 s.
    let mut presented_positions = Vec::new();
    for _ in 0..8 {
        let tick_result = harness.tick_once_fast_preroll();
        if tick_result.video_frames_presented > 0 {
            let presented_position = harness
                .session
                .pipeline
                .present_video_frame()
                .expect("presentation counter требует текущий frame")
                .pts;
            presented_positions.push(presented_position);
        }
        observed_events.extend(harness.session.take_events());
        if harness.session.seek_commit().is_none() {
            break;
        }
    }

    // Ни receipt, ни pre-actual decoded frame не имеют права открыть UI/audio gates.
    assert_eq!(presented_positions, vec![presented_landing]);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(presented_landing)
    );
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.playback_state(), PlaybackState::Playing);
    assert_eq!(
        harness.session.snapshot().current_position,
        presented_landing,
        "UI/snapshot обязаны коммитить фактически presented landing, а не requested 55 s"
    );
    assert_eq!(
        harness.session.pipeline.media_clock_base(),
        presented_landing,
        "post-seek A/V clock должен начинаться от честного landing frame"
    );
    assert_eq!(audio_output.pause_count.load(Ordering::Relaxed), 1);
    assert_eq!(audio_output.clear_count.load(Ordering::Relaxed), 1);
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 2);
    assert!(harness.seek_requests().is_empty());

    // Lifecycle ordering не позволяет PositionChanged/SeekCommitted обогнать video и audio readiness.
    let target_frame_index = event_index(&observed_events, |event| {
        matches!(
            event,
            PlayerEvent::SeekTargetFramePresented(presentation)
                if presentation.target_position == requested_target
                    && presentation.frame_pts == presented_landing
        )
    });
    let audio_resume_index = event_index(&observed_events, |event| {
        matches!(
            event,
            PlayerEvent::AudioResumedAfterSeek(info)
                if info.target_position == requested_target
                    && info.playback_position == presented_landing
        )
    });
    let position_commit_index = event_index(
        &observed_events,
        |event| matches!(event, PlayerEvent::PositionChanged(position) if *position == presented_landing),
    );
    let seek_commit_index = event_index(&observed_events, |event| {
        matches!(
            event,
            PlayerEvent::SeekCommitted(commit)
                if commit.target_position == requested_target
                    && commit.actual_position == authoritative_actual
        )
    });
    assert!(target_frame_index < audio_resume_index);
    assert!(audio_resume_index < position_commit_index);
    assert!(position_commit_index < seek_commit_index);

    // После честного commit-а timeline продолжает двигаться от audio clock, а не прыгает назад к 55 s.
    let committed_position = harness.session.snapshot().current_position;
    audio_output.clock.record_played(48_000 * 2 / 10);
    let _tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..PlayerTickConfig::default()
        },
    ));
    assert!(harness.session.snapshot().current_position > committed_position);
}

/// Rapid forward -> backward supersede не должен представить landing устаревшего HLS request-а.
#[test]
fn hls_post_target_supersede_presents_only_latest_backward_landing() {
    let superseded_target = Duration::from_secs(55);
    let superseded_actual = Duration::from_secs(60);
    let latest_target = Duration::from_secs(15);
    let latest_actual = Duration::from_secs(20);
    let latest_landing = Duration::from_millis(20_040);
    let video_track = fake_track(1, TrackKind::Video);
    let packets = vec![fake_video_packet_with_keyframe(
        video_track.id,
        latest_landing,
        PacketKeyframe::Keyframe,
    )];
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        latest_target,
        latest_actual,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .session
        .set_snapshot_duration(Some(Duration::from_secs(120)));

    let prepared_seek_port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased_seek_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased_seek_port,
            landing_policy: PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget,
        });
    harness
        .decoder
        .decode_next_packet_as_frame(latest_landing, 20_001);

    // Второй seek идёт назад и supersede-ит первый до получения его terminal receipt-а.
    harness.start_final_seek(MediaTime::from_duration(superseded_target));
    harness.start_final_seek(MediaTime::from_duration(latest_target));
    let commands = prepared_seek_port.commands();
    assert_eq!(commands.len(), 2);

    // Поздний success старого request-а должен быть полностью stale для player generation-а.
    prepared_seek_port.complete(
        commands[0].0,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(superseded_target),
            actual_position: MediaTime::from_duration(superseded_actual),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();
    assert!(harness.session.seek_commit().is_none());
    assert!(harness.session.pipeline.present_video_frame().is_none());

    // Только latest backward target получает commit lifecycle и может дойти до presentation.
    prepared_seek_port.complete(
        commands[1].0,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(latest_target),
            actual_position: MediaTime::from_duration(latest_actual),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();
    for _ in 0..6 {
        let _tick_result = harness.tick_once_fast_preroll();
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
        Some(latest_landing)
    );
    assert_eq!(harness.session.snapshot().current_position, latest_landing);
    assert!(harness.session.take_events().into_iter().all(|event| {
        !matches!(
            event,
            PlayerEvent::SeekTargetFramePresented(presentation)
                if presentation.target_position == superseded_target
        )
    }));
}

/// Проверяет отсутствие fake UI success до target presentation и audio resume.
fn assert_no_successful_position_commit(
    events: &[PlayerEvent],
    requested_target: Duration,
    presented_landing: Duration,
) {
    assert!(!events.iter().any(|event| {
        matches!(event, PlayerEvent::PositionChanged(position) if *position == requested_target)
            || matches!(event, PlayerEvent::PositionChanged(position) if *position == presented_landing)
            || matches!(event, PlayerEvent::SeekCommitted(_))
            || matches!(event, PlayerEvent::AudioResumedAfterSeek(_))
    }));
}

/// Возвращает index обязательного lifecycle event-а с полной event-диагностикой при падении.
fn event_index(events: &[PlayerEvent], predicate: impl Fn(&PlayerEvent) -> bool) -> usize {
    events
        .iter()
        .position(predicate)
        .unwrap_or_else(|| panic!("ожидаемый lifecycle event отсутствует: {events:#?}"))
}
