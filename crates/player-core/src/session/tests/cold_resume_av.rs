//! Вертикальная regression-проверка cold resume до video presentation и audio play.

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
    PlaybackIntent, PlaybackIntentRevision, PlaybackState, PlayerEvent, PlayerTickConfig,
    PlayerTickContext, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, PreparedMedia,
    StartedVideoBackend,
};

/// Cold resume обязан завершиться только после правильного кадра и реально принятого audio play.
#[test]
fn staged_cold_resume_commits_nonzero_position_after_target_frame_and_audio_resume() {
    // Позиции разделяют saved target, decode-safe anchor и первый допустимый presented frame.
    let saved_position = Duration::from_secs(355);
    let decode_anchor = Duration::from_secs(350);
    let landing_position = Duration::from_millis(355_040);
    let media_duration = Duration::from_secs(600);

    // Muxed A/V topology моделирует тот же обязательный путь, что и HLS VOD.
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let tracks = vec![video_track.clone(), audio_track];
    let synchronous_seek_log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut demuxer = FakeDemuxer::new(
        tracks.clone(),
        Some(media_duration),
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

    // Worker-owned port остаётся единственным authoritative источником seek receipt-а.
    let prepared_seek_port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased_seek_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    let prepared_media = PreparedMedia::from_external_label("cold-resume-av", Box::new(demuxer))
        .with_worker_receipted_demux_seek(erased_seek_port);

    // Cold open проходит настоящий staged install protocol с известной request identity.
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(3_551).expect("cold resume request id должен быть non-zero"),
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
        .expect("authorized cold open должен завершиться Installed")
    else {
        panic!("успешный cold open не должен завершаться failure/cancellation");
    };

    // Compatibility install получает concrete decoder/output после atomic media switch.
    let decoder = SharedFakeVideoDecoderThread::new();
    decoder.decode_next_packet_as_frame(decode_anchor, 3_550);
    decoder.decode_next_packet_as_frame(landing_position, 3_551);
    session.set_video_backend(StartedVideoBackend::from_decoder_thread(
        "cold-resume-test-backend",
        decoder.clone(),
    ));
    let audio_output = install_ready_audio_runtime(&mut session, 80.0, None);
    assert!(session.pipeline.has_audio_decoder());
    assert!(session.pipeline.has_audio_output());
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    let _install_events = session.take_events();

    // Restore принимает сохранённую ненулевую позицию через installed-media boundary.
    let (restore_tx, restore_rx) = bounded(1);
    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::SeekTo(saved_position),
        },
        restore_tx,
    );
    assert_eq!(restore_rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(session.snapshot().current_position, Duration::ZERO);
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 0);

    // До worker receipt-а запрещены fake UI commit, Playing и скрытый synchronous fallback seek.
    let commands = prepared_seek_port.commands();
    let [(prepared_seek_request_id, prepared_seek_request)] = commands.as_slice() else {
        panic!("cold resume должен отправить ровно один worker seek request");
    };
    assert_eq!(prepared_seek_request.timestamp, saved_position);
    let mut observed_events = session.take_events();
    assert_no_user_visible_resume_commit(&observed_events, saved_position);

    // Authoritative receipt сообщает decode-safe anchor, но сам по себе ещё ничего не commit-ит.
    prepared_seek_port.complete(
        *prepared_seek_request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(saved_position),
            actual_position: MediaTime::from_duration(decode_anchor),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();
    observed_events.extend(session.take_events());
    assert_no_user_visible_resume_commit(&observed_events, saved_position);
    assert_eq!(restore_rx.try_recv(), Err(TryRecvError::Empty));

    // Обычный tick path декодирует anchor, подавляет его presentation и доходит до target frame.
    let mut presented_frame_count = 0;
    for _ in 0..6 {
        let tick = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_regression_fast_preroll_tick_config(),
        ));
        presented_frame_count += tick.video_frames_presented;
        if let Some(presented_frame) = session.pipeline.present_video_frame() {
            assert!(
                presented_frame.pts >= saved_position,
                "decode anchor/pre-target frame не имеет права становиться видимым"
            );
        }
        observed_events.extend(session.take_events());
        if session.seek_commit().is_none() {
            break;
        }
    }

    // Final state доказывает frame, audio acceptance, restore receipt и отсутствие zero fallback-а.
    assert!(
        session.seek_commit().is_none(),
        "cold resume не закрыл seek: diagnostics={:#?}, sent_packets={:#?}, events={:#?}",
        session
            .active_seek_diagnostics(Instant::now(), &seek_regression_fast_preroll_tick_config()),
        decoder.sent_packets(),
        observed_events
    );
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(landing_position)
    );
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 1);
    assert_eq!(
        presented_frame_count, 1,
        "cold resume должен представить только landing frame, а не decode anchor"
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(session.snapshot().current_position, saved_position);
    assert!(
        synchronous_seek_log
            .lock()
            .expect("synchronous seek log должен оставаться доступен")
            .is_empty(),
        "worker-receipted restore не должен скрыто вызывать synchronous demux seek"
    );
    assert_eq!(
        restore_rx
            .recv()
            .expect("target presentation и audio resume должны завершить restore"),
        InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
    );

    // User-visible ordering не позволяет UI объявить успех до video+audio readiness.
    let target_frame_index = event_index(&observed_events, |event| {
        matches!(
            event,
            PlayerEvent::SeekTargetFramePresented(presentation)
                if presentation.target_position == saved_position
                    && presentation.frame_pts == landing_position
        )
    });
    let audio_resume_index = event_index(&observed_events, |event| {
        matches!(event, PlayerEvent::AudioResumedAfterSeek(_))
    });
    let position_index = event_index(
        &observed_events,
        |event| matches!(event, PlayerEvent::PositionChanged(position) if *position == saved_position),
    );
    let seek_commit_index = event_index(&observed_events, |event| {
        matches!(event, PlayerEvent::SeekCommitted(_))
    });
    let playing_index = event_index(&observed_events, |event| {
        matches!(
            event,
            PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
        )
    });
    assert!(target_frame_index < audio_resume_index);
    assert!(audio_resume_index < position_index);
    assert!(position_index < seek_commit_index);
    assert!(seek_commit_index < playing_index);

    // После commit timeline продолжает двигаться по выбранному audio output clock.
    let committed_position = session.snapshot().current_position;
    audio_output.clock.record_played(48_000 * 2 / 10);
    let _tick = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..PlayerTickConfig::default()
        },
    ));
    assert!(session.snapshot().current_position > committed_position);
}

/// До presentation/audio barrier-а в потоке не должно быть ни одного успешного UI commit event-а.
fn assert_no_user_visible_resume_commit(events: &[PlayerEvent], saved_position: Duration) {
    assert!(!events.iter().any(|event| {
        matches!(event, PlayerEvent::PositionChanged(position) if *position == saved_position)
            || matches!(event, PlayerEvent::SeekCommitted(_))
            || matches!(
                event,
                PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
            )
    }));
}

/// Возвращает exact event index и сохраняет полезную диагностику при нарушении lifecycle.
fn event_index(events: &[PlayerEvent], predicate: impl Fn(&PlayerEvent) -> bool) -> usize {
    events
        .iter()
        .position(predicate)
        .unwrap_or_else(|| panic!("ожидаемый lifecycle event отсутствует: {events:#?}"))
}
