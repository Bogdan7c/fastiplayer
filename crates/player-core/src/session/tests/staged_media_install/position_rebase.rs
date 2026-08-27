//! Functional regression для request-owned position receipt после exact `TracksChanged`.
//!
//! Тест проходит staged same-lineage install, worker-owned demux seek, decoder reset,
//! packet admission и scheduler presentation. Ручной вызов final seek commit запрещён:
//! `Applied` должен прийти только от реально представленного кадра новой generation.

use super::super::test_support::{
    decoded_frame_for_current_seek_generation, fake_video_packet, seek_admission_tick_config,
};
use super::*;
use crate::PlayerTickContext;

/// Создаёт video candidate, который после staged commit публикует `TracksChanged`,
/// а затем единственный target packet для настоящего decoder landing.
fn video_candidate_with_post_commit_track_update(
    target_position: Duration,
    demux_seek_log: Arc<Mutex<Vec<Duration>>>,
) -> PreparedMedia {
    // Один и тот же exact track сохраняет media identity, меняя только decoder generation.
    let video_track = staged_vp9_track(1);
    // Fake demuxer хранит отдельный log обычных seek-вызовов installed runtime-а.
    let mut demuxer = FakeDemuxer::new(
        vec![video_track.clone()],
        Some(Duration::from_secs(40)),
        demux_seek_log,
    );
    // Exact first post-receipt `TracksChanged` подтверждает topology без второго reset-а.
    demuxer.push_event(DemuxReadEvent::TracksChanged(
        media_core::DemuxTrackListUpdate::new(vec![video_track], Some(Duration::from_secs(40))),
    ));
    // Следующий tick передаёт target packet decoder-у уже в новой generation.
    demuxer.push_event(DemuxReadEvent::Packet(fake_video_packet(
        crate::TrackId::new(1),
        target_position,
    )));
    // Candidate остаётся detached до strong staged authorization barrier-а.
    PreparedMedia::from_external_label("position-rebase-video", Box::new(demuxer))
}

/// `TracksChanged` между staged commit и adoption должен переносить receipt на новую
/// generation, не выполнять второй demux seek и завершаться только presentation commit-ом.
#[test]
fn staged_track_rebase_before_adoption_reaches_presented_frame_without_second_demux_seek() {
    // Session получает реальный playable VP9 output для staged video preflight-а.
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    // Старый instance остаётся владельцем позиции до destructive authorization barrier-а.
    let old_instance_id = install_old_media(&mut session, PlaybackState::Playing);
    let target_position = Duration::from_secs(12);
    let prepared_anchor = Duration::from_secs(8);
    set_old_position(&mut session, target_position);

    // Worker port является единственным владельцем staged demux seek request-а.
    let staged_seek_port = Arc::new(FakeStagedSeekPort::default());
    let demux_seek_log = Arc::new(Mutex::new(Vec::new()));
    let candidate =
        video_candidate_with_post_commit_track_update(target_position, Arc::clone(&demux_seek_log))
            .with_worker_receipted_demux_seek(staged_seek_port.clone());
    // Detached decoder позволит доказать packet -> decoded frame -> presentation route.
    let decoder = SharedFakeVideoDecoderThread::new();
    let (resource_port, _) = FakeResourcePort::available(decoder.clone());
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(1_902).expect("test request id должен быть non-zero"),
    );
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);

    // Same-lineage install обязан сначала запросить player-owned position preparation.
    session.stage_same_lineage_prepared_media_install(
        request_id,
        candidate,
        PlaybackIntent::StartPaused,
        PlaybackIntentRevision::INITIAL,
        install_port,
        MediaInstallVideoResourcePort::any_playable(resource_port),
        old_instance_id,
    );
    assert_eq!(
        receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyForPositionPreparation { request_id })
    );

    // Worker получает ровно один request и возвращает decode-safe anchor до target-а.
    session.prepare_staged_media_position(PrepareMediaInstallPosition { request_id });
    let staged_seek_commands = staged_seek_port
        .commands
        .lock()
        .expect("staged command lock должен оставаться доступен")
        .clone();
    let [(staged_seek_request_id, staged_seek_request)] = staged_seek_commands.as_slice() else {
        panic!("position preparation должна отправить ровно один staged demux seek");
    };
    assert_eq!(staged_seek_request.timestamp, target_position);
    staged_seek_port.complete(
        *staged_seek_request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(target_position),
            actual_position: MediaTime::from_duration(prepared_anchor),
            actual_track_timestamp: None,
        }),
    );
    session.service_staged_position_preparation();
    assert_eq!(
        receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyToCommit { request_id })
    );

    // Authorization атомарно устанавливает candidate и запускает adopted decoder landing.
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = receipt
        .try_take_completion()
        .expect("authorization должна опубликовать Installed")
    else {
        panic!("успешный same-lineage candidate должен завершиться Installed");
    };
    let generation_before_track_update = session
        .seek_runtime
        .active_commit()
        .expect("adopted staged position должна ждать decoder landing")
        .generation;

    // Первый installed tick читает exact demux event без повторной смены generation.
    let track_update_tick = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            // Нулевой send budget изолирует rebase до последующего adoption receipt-а.
            max_video_packets_sent_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));
    assert_eq!(track_update_tick.video_frames_presented, 0);
    assert!(decoder.sent_packets().is_empty());
    let generation_after_track_update = session
        .seek_runtime
        .active_commit()
        .expect("TracksChanged должен сохранить active adopted seek")
        .generation;
    assert_eq!(
        generation_after_track_update,
        generation_before_track_update
    );

    // App забирает staged outcome после topology confirmation и ждёт presentation.
    let (restore_tx, restore_rx) = crossbeam_channel::bounded(1);
    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::AdoptPreparedSameLineagePosition,
        },
        restore_tx,
    );
    assert_eq!(
        restore_rx.try_recv(),
        Err(crossbeam_channel::TryRecvError::Empty),
        "receipt нельзя завершать до presentation commit-а текущей generation"
    );

    // Второй tick проводит target packet через обычный installed decoder admission.
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

    // Decoded frame получает current generation и только scheduler завершает seek.
    decoder.push_decoded_frame(decoded_frame_for_current_seek_generation(
        &session,
        target_position,
        1_902,
    ));
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
            .expect("представленный target frame должен завершить rebased receipt"),
        InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
    );
    assert_eq!(session.snapshot().current_position, target_position);
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert!(session.seek_commit().is_none());

    // Ни adoption, ни TracksChanged не имеют права повторять worker/demux seek.
    assert_eq!(
        staged_seek_port
            .commands
            .lock()
            .expect("staged command lock должен оставаться доступен")
            .len(),
        1
    );
    assert!(
        demux_seek_log
            .lock()
            .expect("demux seek log lock должен оставаться доступен")
            .is_empty()
    );
}
