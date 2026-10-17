//! Регрессии backlog #21: same-lineage reopen (смена web-версии звука или качества) у хвоста
//! HLS и в первых кадрах сегмента.
//!
//! Каждый тест проходит настоящий production путь player-а: staged same-lineage install,
//! worker-owned demux seek receipt, authorization, app restore (`InstalledMediaStateRestore`),
//! packet admission в decoder и scheduler presentation. Ручной final seek commit запрещён:
//! `Applied` приходит только от реально представленного кадра новой generation.

use super::super::test_support::{
    decoded_frame_for_current_seek_generation, fake_video_packet, seek_admission_tick_config,
};
use super::*;
use crate::PlayerTickContext;

/// Duration candidate-а: десять HLS-сегментов по 2 с, как в живом прогоне D6.
const CANDIDATE_DURATION: Duration = Duration::from_secs(20);

/// PTS единственного RAP последнего сегмента (DTS 18 с): B-кадры H.264 сдвигают его
/// на reorder delay.
const LAST_SEGMENT_RAP_PTS: Duration = Duration::from_millis(18_080);

/// Что app просит восстановить для video после reopen.
#[derive(Debug, Clone, Copy)]
enum RestoredVideoTrack {
    /// App не трогает video selection.
    KeepDefault,
    /// App повторно выбирает ту же дорожку, что уже выбрал staged install (production путь).
    SameTrackAgain,
}

/// В каком состоянии старый instance встречает переключение.
#[derive(Debug, Clone, Copy)]
enum OldPlayback {
    /// Старый playback играет.
    Playing,
    /// Demux старого instance-а уже дошёл до EOF: играет хвост (`Draining`), и app
    /// зеркалит ему D52 intent `StartPlaying` во время подготовки reopen-а.
    DrainingTailWithStartPlayingIntent,
}

/// Наблюдаемый итог одного reopen-а.
struct ReopenObservation {
    /// Session после landing-а.
    session: PlayerSession,
    /// Detached decoder нового instance-а.
    decoder: SharedFakeVideoDecoderThread,
    /// Итог restore receipt-а app-а.
    restore_outcome: Result<InstalledMediaStateRestoreOutcome, crossbeam_channel::TryRecvError>,
    /// Instance, установленный authorization-ом.
    media_instance_id: MediaInstanceId,
}

/// Сценарий: какое media, куда переключаемся и что вернул worker.
struct ReopenScenario {
    /// Позиция старого playback-а в момент переключения.
    old_position: Duration,
    /// Ожидаемый target reopen-а (позиция старого playback-а, не дальше конца media).
    target_position: Duration,
    /// `actual_position` worker receipt-а (PTS найденного RAP).
    receipted_anchor: Duration,
    /// Кадр, который decoder вернёт после post-seek packet-а.
    decoded_frame_pts: Duration,
    /// Selection, который app восстанавливает после install-а.
    restored_video_track: RestoredVideoTrack,
    /// Состояние старого instance-а.
    old_playback: OldPlayback,
}

/// VP9 HDR track без profile/bit depth в container-е: точный P010 contract даёт только
/// packet preflight. Так staged config заведомо богаче config-а «из одной дорожки» —
/// ровно как у HLS H.264 (container не знает размеров, preflight знает).
fn hdr_track_resolved_only_by_packet_preflight() -> media_core::TrackInfo {
    let mut video_track = fake_track(1, TrackKind::Video);
    let mut video_metadata = VideoTrackMetadata::empty();
    video_metadata.coded_width = Some(3_840);
    video_metadata.coded_height = Some(2_160);
    video_metadata.color = Some(bt2020_pq_limited());
    video_track.video = Some(video_metadata);
    video_track
}

/// Candidate, который до seek-а отдаёт preflight-у keyframe позиции 0, а после
/// worker seek-а — RAP последнего сегмента и EOF (хвост без следующего keyframe-а).
fn tail_candidate(post_seek_anchor: Duration) -> PreparedMedia {
    let mut demuxer = FakeDemuxer::new(
        vec![hdr_track_resolved_only_by_packet_preflight()],
        Some(CANDIDATE_DURATION),
        Arc::new(Mutex::new(Vec::new())),
    );
    // Этот keyframe читает только staged preflight на стартовой позиции candidate-а.
    demuxer.push_event(DemuxReadEvent::Packet(Packet::new_unbounded(
        crate::TrackId::new(1),
        TrackKind::Video,
        Duration::ZERO,
        None,
        true,
        vp9_profile2_10bit_bt2020_keyframe(3_840, 2_160),
    )));
    // После seek-а demuxer стоит на RAP последнего сегмента; затем EOF.
    demuxer.push_packet(fake_video_packet(crate::TrackId::new(1), post_seek_anchor));
    PreparedMedia::from_external_label("hls-tail-reopen", Box::new(demuxer))
}

/// Tick config, который отправляет в decoder по одному packet-у за tick.
fn one_packet_tick_config() -> PlayerTickConfig {
    PlayerTickConfig {
        max_demux_packets_per_tick: 1,
        max_video_packets_sent_per_tick: 1,
        ..seek_admission_tick_config(2, 4)
    }
}

/// Проводит reopen через production staged path до presentation-а целевого кадра.
fn run_reopen(scenario: &ReopenScenario, request_value: u64) -> ReopenObservation {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
    let old_instance_id = install_old_media(&mut session, PlaybackState::Playing);
    // Старый instance — та же media (тот же lineage), что и candidate.
    session.set_snapshot_duration(Some(CANDIDATE_DURATION));
    set_old_position(&mut session, scenario.old_position);

    let staged_seek_port = Arc::new(FakeStagedSeekPort::default());
    let candidate = tail_candidate(LAST_SEGMENT_RAP_PTS)
        .with_worker_receipted_demux_seek(staged_seek_port.clone());
    let decoder = SharedFakeVideoDecoderThread::new();
    let (resource_port, _) = FakeResourcePort::available(decoder.clone());
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(request_value).expect("test request id must be non-zero"),
    );
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
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

    session.prepare_staged_media_position(PrepareMediaInstallPosition { request_id });
    if let OldPlayback::DrainingTailWithStartPlayingIntent = scenario.old_playback {
        session.enter_eof_drain();
        assert!(session.apply_playback_intent_to_exact_installed_instance(
            old_instance_id,
            PlaybackIntent::StartPlaying,
        ));
        assert_eq!(
            session.playback_state(),
            PlaybackState::Draining,
            "the intent must not restart the old playback from the beginning"
        );
        assert_eq!(session.snapshot().current_position, scenario.old_position);
    }
    let staged_seek_commands = staged_seek_port
        .commands
        .lock()
        .expect("staged command lock")
        .clone();
    let [(staged_seek_request_id, staged_seek_request)] = staged_seek_commands.as_slice() else {
        panic!("position preparation must send exactly one staged demux seek");
    };
    assert_eq!(staged_seek_request.timestamp, scenario.target_position);
    staged_seek_port.complete(
        *staged_seek_request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(scenario.target_position),
            actual_position: MediaTime::from_duration(scenario.receipted_anchor),
            actual_track_timestamp: None,
        }),
    );
    session.service_staged_position_preparation();
    assert_eq!(
        receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyToCommit { request_id }),
        "worker receipt must authorize the reopen instead of failing position preparation: {:?}",
        receipt.try_take_completion()
    );
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
        .expect("authorization must publish Installed")
    else {
        panic!("successful same-lineage candidate must complete Installed");
    };

    // App сразу после Installed восстанавливает дорожки и усыновляет prepared position.
    let video_track = match scenario.restored_video_track {
        RestoredVideoTrack::KeepDefault => InstalledTrackRestore::KeepDefault,
        RestoredVideoTrack::SameTrackAgain => InstalledTrackRestore::Select(crate::TrackId::new(1)),
    };
    let (restore_tx, restore_rx) = crossbeam_channel::bounded(1);
    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::AdoptPreparedSameLineagePosition,
        },
        restore_tx,
    );

    // Post-seek packet уходит в decoder обычным installed admission.
    let packet_tick = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        one_packet_tick_config(),
    ));
    assert_eq!(packet_tick.video_frames_presented, 0);
    // Decoder декодирует GOP вперёд и отдаёт кадр, покрывающий target, в том contract-е,
    // под который его на самом деле сконфигурировали последним.
    let mut decoded_frame = decoded_frame_for_current_seek_generation(
        &session,
        scenario.decoded_frame_pts,
        request_value,
    );
    decoded_frame.frame_contract = decoder
        .stream_config()
        .expect("staged install must configure the detached decoder")
        .frame_contract;
    decoder.push_decoded_frame(decoded_frame);
    let present_tick = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));
    assert_eq!(present_tick.video_frames_presented, 1);

    ReopenObservation {
        session,
        decoder,
        restore_outcome: restore_rx.try_recv(),
        media_instance_id,
    }
}

/// Проверяет общий успешный итог reopen-а: кадр представлен, позиция сохранена, seek закрыт.
fn assert_reopen_landed(observation: &ReopenObservation, scenario: &ReopenScenario) {
    assert_eq!(
        observation.restore_outcome,
        Ok(InstalledMediaStateRestoreOutcome::Applied {
            media_instance_id: observation.media_instance_id,
        }),
        "the presented target frame must complete the app restore receipt"
    );
    assert_eq!(
        observation
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(scenario.decoded_frame_pts)
    );
    assert_eq!(
        observation.session.snapshot().current_position,
        scenario.target_position,
        "reopen must keep the old playback position"
    );
    assert_eq!(observation.session.playback_state(), PlaybackState::Paused);
    assert!(observation.session.seek_commit().is_none());
}

/// Живой D6: переключение в первые ~80 мс сегмента. Worker нашёл RAP с DTS до target-а,
/// но его PTS (18.08 с) позже target-а (18.04 с) из-за B-кадров. Обычный seek такой
/// receipt принимает; reopen обязан тоже, а не отказывать «anchor lies after target».
#[test]
fn reopen_accepts_reordered_rap_whose_pts_lies_after_target() {
    let scenario = ReopenScenario {
        old_position: Duration::from_millis(18_040),
        target_position: Duration::from_millis(18_040),
        receipted_anchor: LAST_SEGMENT_RAP_PTS,
        decoded_frame_pts: LAST_SEGMENT_RAP_PTS,
        restored_video_track: RestoredVideoTrack::KeepDefault,
        old_playback: OldPlayback::Playing,
    };

    let observation = run_reopen(&scenario, 2_101);

    assert_reopen_landed(&observation, &scenario);
}

/// Живой D6: переключение в хвосте последнего сегмента (после последнего keyframe-а) и
/// прямо перед EOF. App повторно выбирает ту же video-дорожку; это не должно
/// переконфигурировать decoder (настоящий decoder при этом теряет GOP и ждёт следующего
/// keyframe-а, которого в хвосте нет — seek висел в `WaitingForVideoTargetFrame`).
#[test]
fn reopen_in_last_segment_tail_keeps_decoder_stream_and_lands_target_frame() {
    for (request_value, old_position, target_position, old_playback) in [
        // Середина хвоста после единственного RAP последнего сегмента.
        (
            2_102,
            Duration::from_millis(19_000),
            Duration::from_millis(19_000),
            OldPlayback::Playing,
        ),
        // Последний кадр перед EOF, старый playback уже доигрывает хвост.
        (
            2_103,
            Duration::from_millis(19_960),
            Duration::from_millis(19_960),
            OldPlayback::DrainingTailWithStartPlayingIntent,
        ),
        // Часы хвоста уже экстраполировали за конец (живой D6: 20.0107 с у 20 с ролика):
        // reopen встаёт на конец media, а не отказывает «candidate is shorter than target».
        (
            2_104,
            Duration::from_micros(20_010_666),
            CANDIDATE_DURATION,
            OldPlayback::DrainingTailWithStartPlayingIntent,
        ),
    ] {
        let scenario = ReopenScenario {
            old_position,
            target_position,
            receipted_anchor: LAST_SEGMENT_RAP_PTS,
            decoded_frame_pts: target_position,
            restored_video_track: RestoredVideoTrack::SameTrackAgain,
            old_playback,
        };

        let observation = run_reopen(&scenario, request_value);

        assert_reopen_landed(&observation, &scenario);
        // Decoder сконфигурирован один раз staged preflight-ом (P010) и не переконфигурирован
        // менее точным config-ом из container metadata.
        let configured_layouts = observation
            .decoder
            .configured_streams()
            .iter()
            .map(|config| config.frame_contract.pixel_layout)
            .collect::<Vec<_>>();
        assert_eq!(
            configured_layouts,
            vec![video_frame_contract::VideoFramePixelLayout::P010],
            "redundant same-track restore must not reconfigure the decoder ({target_position:?})"
        );
        assert_eq!(
            observation
                .session
                .pipeline
                .active_video_frame_contract()
                .map(|contract| contract.pixel_layout),
            Some(video_frame_contract::VideoFramePixelLayout::P010)
        );
        // В decoder ушёл только post-seek RAP: preflight packet позиции 0 устарел после seek-а.
        let sent_packet_pts = observation
            .decoder
            .sent_packets()
            .iter()
            .map(|packet| packet.pts)
            .collect::<Vec<_>>();
        assert_eq!(sent_packet_pts, vec![LAST_SEGMENT_RAP_PTS]);
    }
}

/// Пользовательский Play во время drain-а доигранного файла по-прежнему повторяет файл:
/// no-op касается только D52 intent-а «играть», а не публичной команды.
#[test]
fn public_play_in_started_playback_drain_still_replays_from_start() {
    let mut session = PlayerSession::new();
    install_old_media(&mut session, PlaybackState::Playing);
    set_old_position(&mut session, Duration::from_millis(19_960));
    session.enter_eof_drain();

    session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play during drain must be accepted");

    let replay_seek = session
        .seek_runtime
        .active_commit()
        .expect("Play after playthrough starts a replay seek");
    assert_eq!(replay_seek.target_position, MediaTime::ZERO);
}
