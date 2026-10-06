//! Сессия 03: причина отказа player-а доходит до terminal-исхода coordinator-а.
//!
//! Раньше `PlayerRejected`/`PlayerFailed` несли только `request_id`, а `MediaInstallFailure`
//! выбрасывался. Эти тесты ломаются, если причину снова потерять или подменить.

use std::time::Instant;

use player_core::{
    FrameCounters, MediaInstallFailure, MediaInstallFailureStage, PlayerError, PlayerErrorKind,
    PlayerSnapshot, PlayerWorker, PlayerWorkerConfig,
};

use super::*;
use crate::local_open_message::local_open_failure_message;
use crate::media_open::PlayerInstallFailureReason;

/// Все ожидания реального worker-а ограничены, чтобы регрессия не подвесила CI.
const REAL_WORKER_DEADLINE: Duration = Duration::from_secs(5);

fn start_intent() -> MediaOpenInstallIntent {
    MediaOpenInstallIntent {
        intent: player_core::PlaybackIntent::StartPaused,
        revision: player_core::PlaybackIntentRevision::INITIAL,
    }
}

/// Запускает request с уже подготовленным media и возвращает его ID.
fn start_prepared_request(
    coordinator: &mut MediaOpenCoordinator,
    client_number: u64,
    prepared: PreparedMediaOpen,
) -> MediaOpenRequestId {
    match coordinator
        .start_prepared(
            client(client_number),
            prepared,
            SafeMediaLabel::from_service_safe_label("player-failure.test"),
        )
        .expect("prepared request accepted")
    {
        MediaOpenStartOutcome::Accepted { request_id } => request_id,
        MediaOpenStartOutcome::Coalesced { .. } => panic!("unexpected coalesce"),
    }
}

fn player_failure(stage: MediaInstallFailureStage, kind: PlayerErrorKind) -> MediaInstallFailure {
    MediaInstallFailure::new(stage, PlayerError::new(kind, "технические детали для лога"))
}

/// Кладёт terminal player-а в fake receipt, как это сделал бы настоящий worker.
fn publish_player_completion(
    player_state: &Arc<Mutex<FakePlayerState>>,
    completion: MediaInstallCompletion,
) {
    player_state
        .lock()
        .expect("player state")
        .install_slots
        .lock()
        .expect("install slots")
        .completion = Some(completion);
}

#[test]
fn staging_failure_terminal_carries_classified_player_reason() {
    let mut coordinator = coordinator();
    let request_id = start_prepared_request(&mut coordinator, 301, fake_prepared());
    let player_state = attach_fake_player(&mut coordinator, None, Vec::new());
    let player_request_id = coordinator
        .stage_at_player(
            request_id,
            start_intent(),
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");

    publish_player_completion(
        &player_state,
        MediaInstallCompletion::Failed {
            request_id: player_request_id,
            failure: player_failure(
                MediaInstallFailureStage::CandidateVideoResourceAcquisition,
                PlayerErrorKind::RequiredVideoBackendUnavailable,
            ),
        },
    );

    assert_eq!(
        coordinator.wait_for_progress(request_id),
        Ok(MediaOpenPhase::Failed)
    );
    assert!(matches!(
        coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::PlayerFailed {
            reason: PlayerInstallFailureReason::NoSuitableVideoDecoder,
            ..
        }))
    ));
}

/// Прежняя семантика сохранена: любой terminal до Ready завершает request как
/// `PlayerFailed`. Неожиданная отмена — внутренняя ошибка, а не «кодек не поддерживается».
#[test]
fn unexpected_cancelled_completion_during_staging_is_internal_player_failure() {
    let mut coordinator = coordinator();
    let request_id = start_prepared_request(&mut coordinator, 302, fake_prepared());
    let player_state = attach_fake_player(&mut coordinator, None, Vec::new());
    let player_request_id = coordinator
        .stage_at_player(
            request_id,
            start_intent(),
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");

    publish_player_completion(
        &player_state,
        MediaInstallCompletion::Cancelled {
            request_id: player_request_id,
            cause: MediaInstallCancellationCause::Superseded,
        },
    );

    assert_eq!(
        coordinator.wait_for_progress(request_id),
        Ok(MediaOpenPhase::Failed)
    );
    assert!(matches!(
        coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::PlayerFailed {
            reason: PlayerInstallFailureReason::InternalError,
            ..
        }))
    ));
}

/// Отказ на authorization (`AuthorizationRejectedBeforeCommit`) тоже сохраняет причину.
#[test]
fn authorization_rejected_before_commit_carries_player_reason() {
    let mut coordinator = coordinator();
    let request_id = start_prepared_request(&mut coordinator, 303, fake_prepared());
    let authorization_state = Arc::new(Mutex::new(FakeControlState::Pending));
    let player_state = attach_fake_player(
        &mut coordinator,
        None,
        vec![Arc::clone(&authorization_state)],
    );
    let player_request_id = coordinator
        .stage_at_player(
            request_id,
            start_intent(),
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");
    player_state
        .lock()
        .expect("player state")
        .install_slots
        .lock()
        .expect("install slots")
        .ready = Some(MediaInstallPhase::ReadyToCommit {
        request_id: player_request_id,
    });
    assert_eq!(
        coordinator.wait_for_progress(request_id),
        Ok(MediaOpenPhase::ReadyToCommit)
    );
    assert_eq!(
        coordinator.authorize_ready(request_id),
        Ok(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner)
    );

    publish_player_completion(
        &player_state,
        MediaInstallCompletion::Failed {
            request_id: player_request_id,
            failure: player_failure(
                MediaInstallFailureStage::PositionPreparation,
                PlayerErrorKind::UnsupportedAudioCodec,
            ),
        },
    );
    *authorization_state.lock().expect("authorization state") =
        FakeControlState::Outcome(MediaInstallControlOutcome::AuthorizationRejectedBeforeCommit);

    assert_eq!(
        coordinator.wait_for_progress(request_id),
        Ok(MediaOpenPhase::Failed)
    );
    assert!(matches!(
        coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::PlayerFailed {
            reason: PlayerInstallFailureReason::UnsupportedAudioFormat,
            ..
        }))
    ));
}

/// Port, который отказывает в доставке команды установки (очередь/поток player-а).
struct RejectingStagePlayerPort {
    rejection: PlayerDispatchRejection,
}

impl MediaOpenPlayerPort for RejectingStagePlayerPort {
    fn stage(
        &self,
        _request_id: MediaInstallRequestId,
        _prepared_media: player_core::PreparedMedia,
        _intent: MediaOpenInstallIntent,
        _video_resource_port: MediaInstallVideoResourcePort,
        _position_preparation: MediaOpenPositionPreparation,
    ) -> Result<Box<dyn InstallReceiptPort>, PlayerDispatchRejection> {
        Err(self.rejection)
    }

    fn prepare_position(
        &self,
        _request_id: MediaInstallRequestId,
    ) -> Result<(), PlayerDispatchRejection> {
        panic!("отклонённый stage не может дойти до position preparation")
    }

    fn authorize(
        &self,
        _request_id: MediaInstallRequestId,
    ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection> {
        panic!("отклонённый stage не может дойти до authorization")
    }

    fn cancel(
        &self,
        _request_id: MediaInstallRequestId,
        _cause: MediaInstallCancellationCause,
    ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection> {
        panic!("отклонённый stage не требует cancel")
    }

    fn update_intent(
        &self,
        _update: PlaybackIntentUpdate,
    ) -> Result<PlaybackIntentUpdateReceipt, PlayerDispatchRejection> {
        panic!("отклонённый stage не обновляет intent")
    }
}

#[test]
fn undelivered_stage_command_is_rejected_with_player_not_responding() {
    let mut coordinator = coordinator();
    let request_id = start_prepared_request(&mut coordinator, 304, fake_prepared());
    coordinator.attach_fake_player(Arc::new(RejectingStagePlayerPort {
        rejection: PlayerDispatchRejection::Disconnected,
    }));

    // Вызывающий код по-прежнему видит точный transport-отказ...
    assert_eq!(
        coordinator.stage_at_player(
            request_id,
            start_intent(),
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        ),
        Err(MediaOpenCommandError::PlayerDispatch(
            PlayerDispatchRejection::Disconnected
        ))
    );
    // ...а terminal несёт пользовательскую причину и остаётся отдельным вариантом Rejected.
    assert!(matches!(
        coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::PlayerRejected {
            reason: PlayerInstallFailureReason::PlayerNotResponding,
            ..
        }))
    ));
}

/// Видеодорожка с кодеком, которого нет в capability model плеера.
struct UnknownVideoCodecDemuxer {
    tracks: Vec<media_core::TrackInfo>,
}

impl UnknownVideoCodecDemuxer {
    fn new() -> Self {
        Self {
            tracks: vec![media_core::TrackInfo {
                id: media_core::TrackId::new(1),
                kind: media_core::TrackKind::Video,
                codec_id: "V_FASTIPLAYER_UNKNOWN_CODEC".to_owned(),
                codec_private: None,
                time_base: None,
                duration: None,
                sample_rate: None,
                channels: None,
                video: None,
            }],
        }
    }
}

impl Demuxer for UnknownVideoCodecDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        panic!("staged preflight неизвестного кодека не делает seek")
    }
}

/// Ждёт состояние настоящего worker-а с ограничением по времени.
fn wait_for_worker_snapshot(
    worker: &mut PlayerWorker,
    predicate: impl Fn(&PlayerSnapshot) -> bool,
) -> PlayerSnapshot {
    let deadline = Instant::now() + REAL_WORKER_DEADLINE;
    loop {
        let snapshot = worker.latest_snapshot(FrameCounters::default());
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "player snapshot не достиг ожидаемого состояния"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// Реальный путь: настоящий player worker отказывает в установке файла с неизвестным
/// видеокодеком. Пользователь получает «формат видео не поддерживается» с именем файла,
/// а ранее установленное media остаётся на месте (отказ до commit barrier-а).
#[test]
fn real_player_rejects_unknown_video_codec_with_readable_reason_and_keeps_old_media() {
    let mut worker =
        PlayerWorker::spawn(PlayerWorkerConfig::default()).expect("player worker стартует");
    let old_install = worker
        .load_prepared_media(
            player_core::PreparedMedia::from_external_label("old-media", Box::new(FakeDemuxer)),
            false,
        )
        .expect("старое media принято worker-ом");
    let old_snapshot = wait_for_worker_snapshot(&mut worker, |snapshot| {
        snapshot.source_label.as_deref() == Some("old-media")
            && snapshot.media_instance_id.is_some()
    });
    let old_media_instance_id = old_snapshot.media_instance_id;
    drop(old_install);

    let mut coordinator = coordinator();
    let unsupported = PreparedMediaOpen {
        prepared_media: player_core::PreparedMedia::from_external_label(
            "unsupported-media",
            Box::new(UnknownVideoCodecDemuxer::new()),
        ),
        ..fake_prepared()
    };
    let request_id = start_prepared_request(&mut coordinator, 305, unsupported);
    coordinator.attach_fake_player(Arc::new(worker.command_sender()));
    coordinator
        .stage_at_player(
            request_id,
            start_intent(),
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage доставлен настоящему worker-у");

    let deadline = Instant::now() + REAL_WORKER_DEADLINE;
    while coordinator.snapshot().expect("current request").phase != MediaOpenPhase::Failed {
        assert!(
            Instant::now() < deadline,
            "настоящий worker не завершил staging отказом"
        );
        coordinator.drain();
        thread::sleep(Duration::from_millis(1));
    }
    let Ok(Some(MediaOpenTerminalOutcome::PlayerFailed { reason, .. })) =
        coordinator.take_terminal(request_id)
    else {
        panic!("неподдерживаемый кодек обязан завершиться PlayerFailed");
    };

    assert_eq!(reason, PlayerInstallFailureReason::UnsupportedVideoFormat);
    assert_eq!(
        local_open_failure_message(std::path::Path::new("/videos/clip.mkv"), reason),
        "Не удалось открыть «clip.mkv»: формат видео не поддерживается"
    );
    let snapshot_after_failure =
        wait_for_worker_snapshot(&mut worker, |snapshot| snapshot.source_label.is_some());
    assert_eq!(
        snapshot_after_failure.source_label.as_deref(),
        Some("old-media")
    );
    assert_eq!(
        snapshot_after_failure.media_instance_id,
        old_media_instance_id
    );
}

/// UX сессия 08: web-ссылка не подготовилась (сервер ответил 404) — coordinator отдаёт
/// типизированную причину, а настоящий player продолжает держать прежнее media.
#[test]
fn web_preparation_failure_reports_reason_and_keeps_old_media_playing() {
    use crate::media_open::preparation::web_failure_tests::{LoopbackServer, ServerAnswer};

    let mut worker =
        PlayerWorker::spawn(PlayerWorkerConfig::default()).expect("player worker стартует");
    let old_install = worker
        .load_prepared_media(
            player_core::PreparedMedia::from_external_label("old-media", Box::new(FakeDemuxer)),
            false,
        )
        .expect("старое media принято worker-ом");
    let old_snapshot = wait_for_worker_snapshot(&mut worker, |snapshot| {
        snapshot.source_label.as_deref() == Some("old-media")
            && snapshot.media_instance_id.is_some()
    });
    drop(old_install);

    let server = LoopbackServer::spawn(ServerAnswer::Status {
        status_line: "404 Not Found",
        extra_headers: "",
    });
    let locator = media_source_open::direct_progressive_open::classify_direct_media_url(
        &server.secret_media_url(),
    )
    .expect("loopback mp4 ссылка — direct media");
    let request = crate::media_open::MediaOpenSourceRequest::Web(
        crate::media_open::WebMediaOpenRequest::direct(
            locator,
            fastiplayer_config::NetworkConfig::default(),
            fastiplayer_config::PlayerDemuxConfig::default(),
        ),
    );
    let mut coordinator = coordinator();
    coordinator.attach_fake_player(Arc::new(worker.command_sender()));
    coordinator
        .start_fake(
            client(306),
            SafeMediaLabel::from_service_safe_label("direct mp4 (127.0.0.1)"),
            move || {
                let cancellation = crate::media_open::executor::PreparationCancellation::new();
                crate::media_open::preparation::prepare_source(request, &cancellation)
            },
        )
        .expect("request принят");
    let request_id = coordinator.snapshot().expect("текущий request").request_id;

    let deadline = Instant::now() + REAL_WORKER_DEADLINE;
    let kind = loop {
        coordinator.drain();
        if let Ok(Some(MediaOpenTerminalOutcome::PreparationFailed { kind, .. })) =
            coordinator.take_terminal(request_id)
        {
            break kind;
        }
        assert!(
            Instant::now() < deadline,
            "подготовка не завершилась отказом"
        );
        thread::sleep(Duration::from_millis(1));
    };

    assert_eq!(
        kind,
        MediaPreparationFailureKind::DirectOpen(crate::media_open::WebOpenFailureReason::NotFound)
    );
    let snapshot_after_failure =
        wait_for_worker_snapshot(&mut worker, |snapshot| snapshot.source_label.is_some());
    assert_eq!(
        snapshot_after_failure.source_label.as_deref(),
        Some("old-media")
    );
    assert_eq!(
        snapshot_after_failure.media_instance_id,
        old_snapshot.media_instance_id
    );
}
