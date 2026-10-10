//! UX сессия 18: медленный файл на настоящем player-е.
//!
//! Первый тест воспроизводит причину подвисания окна: блокирующее ожидание прогресса держит
//! вызывающий поток, пока player думает над файлом. Второй — то, на что опирается неблокирующий
//! путь Open: `drain`/`snapshot` возвращаются сразу, а отказ по таймауту предпросмотра доходит
//! до пользователя понятным текстом, и старое media остаётся на месте.

use std::time::Instant;

use player_core::{PlayerTickConfig, PlayerWorker, PlayerWorkerConfig};

use super::player_failure_tests::{
    REAL_WORKER_DEADLINE, start_intent, start_prepared_request, wait_for_worker_snapshot,
};
use super::*;
use crate::local_open_message::local_open_failure_message;
use crate::media_open::PlayerInstallFailureReason;

/// Уменьшенный `staged_video_preflight_timeout` (в production — 15 с).
const TEST_PREFLIGHT_TIMEOUT: Duration = Duration::from_millis(400);
/// Один неблокирующий шаг обязан укладываться в эту границу.
const NONBLOCKING_STEP_BUDGET: Duration = Duration::from_millis(50);

/// Видео, данные которого «ещё не пришли»: медленный диск/NAS глазами demuxer-а.
struct NeverReadyVideoDemuxer {
    tracks: Vec<media_core::TrackInfo>,
}

impl NeverReadyVideoDemuxer {
    fn new() -> Self {
        Self {
            tracks: vec![media_core::TrackInfo {
                id: media_core::TrackId::new(1),
                kind: media_core::TrackKind::Video,
                codec_id: "V_MPEG4/ISO/AVC".to_owned(),
                codec_private: None,
                time_base: None,
                duration: None,
                sample_rate: None,
                channels: None,
                video: Some(media_core::VideoTrackMetadata::empty()),
            }],
        }
    }
}

impl Demuxer for NeverReadyVideoDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        let retry_hint = media_core::DemuxRetryHint::new(Duration::from_millis(20))
            .map_err(|_| anyhow::anyhow!("retry hint вне допустимого диапазона"))?;
        Ok(media_core::DemuxReadEvent::TemporarilyUnavailable(
            retry_hint,
        ))
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        panic!("staged preflight медленного файла не делает seek")
    }
}

/// Настоящий worker с уже играющим старым media и коротким таймаутом предпросмотра.
fn worker_with_old_media() -> (PlayerWorker, Option<player_core::MediaInstanceId>) {
    let mut worker = PlayerWorker::spawn(PlayerWorkerConfig {
        tick_config: PlayerTickConfig {
            staged_video_preflight_timeout: TEST_PREFLIGHT_TIMEOUT,
            ..PlayerTickConfig::default()
        },
        ..PlayerWorkerConfig::default()
    })
    .expect("player worker стартует");
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
    (worker, old_snapshot.media_instance_id)
}

/// Отдаёт медленный файл настоящему player-у и возвращает request в фазе PlayerStaging.
fn stage_slow_file(
    coordinator: &mut MediaOpenCoordinator,
    worker: &PlayerWorker,
    client_number: u64,
) -> MediaOpenRequestId {
    let slow_file = PreparedMediaOpen {
        prepared_media: player_core::PreparedMedia::from_external_label(
            "slow-media",
            Box::new(NeverReadyVideoDemuxer::new()),
        ),
        ..fake_prepared()
    };
    let request_id = start_prepared_request(coordinator, client_number, slow_file);
    coordinator.attach_fake_player(Arc::new(worker.command_sender()));
    coordinator
        .stage_at_player(
            request_id,
            start_intent(),
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage доставлен настоящему worker-у");
    assert_eq!(
        coordinator.snapshot().expect("current request").phase,
        MediaOpenPhase::PlayerStaging
    );
    request_id
}

/// Причина подвисания окна (часть A сессии 18): блокирующий шаг держит поток весь preflight.
#[test]
fn blocking_progress_wait_holds_caller_for_whole_slow_preflight() {
    let (worker, _old_media) = worker_with_old_media();
    let mut coordinator = coordinator();
    let request_id = stage_slow_file(&mut coordinator, &worker, 1801);

    let wait_started = Instant::now();
    let phase = coordinator
        .wait_for_progress(request_id)
        .expect("player отвечает таймаутом, а не пропадает");
    let blocked_for = wait_started.elapsed();

    assert_eq!(phase, MediaOpenPhase::Failed);
    assert!(
        blocked_for >= TEST_PREFLIGHT_TIMEOUT.mul_f32(0.9),
        "блокирующий шаг вернулся раньше player-а: {blocked_for:?}"
    );
}

/// На это опирается Open: каждый шаг быстрый, итог — понятный текст, старое media на месте.
#[test]
fn nonblocking_steps_report_slow_file_timeout_and_keep_old_media() {
    let (mut worker, old_media_instance_id) = worker_with_old_media();
    let mut coordinator = coordinator();
    let request_id = stage_slow_file(&mut coordinator, &worker, 1802);

    let staging_started = Instant::now();
    let mut steps_while_player_was_busy = 0_u32;
    loop {
        let step_started = Instant::now();
        coordinator.drain();
        let phase = coordinator.snapshot().expect("current request").phase;
        assert!(
            step_started.elapsed() < NONBLOCKING_STEP_BUDGET,
            "неблокирующий шаг ждал player-а"
        );
        if phase == MediaOpenPhase::Failed {
            break;
        }
        assert_eq!(phase, MediaOpenPhase::PlayerStaging);
        steps_while_player_was_busy += 1;
        assert!(
            staging_started.elapsed() < REAL_WORKER_DEADLINE,
            "player так и не ответил таймаутом предпросмотра"
        );
        thread::sleep(Duration::from_millis(10));
    }
    // Пока player думал, вызывающий поток успел сделать много шагов — то есть окно бы жило.
    assert!(staging_started.elapsed() >= TEST_PREFLIGHT_TIMEOUT.mul_f32(0.9));
    assert!(steps_while_player_was_busy >= 10);

    let Ok(Some(MediaOpenTerminalOutcome::PlayerFailed { reason, .. })) =
        coordinator.take_terminal(request_id)
    else {
        panic!("таймаут предпросмотра обязан завершиться PlayerFailed");
    };
    assert_eq!(reason, PlayerInstallFailureReason::PreparationTimedOut);
    assert_eq!(
        local_open_failure_message(std::path::Path::new("/nas/movies/clip.mkv"), reason),
        "Не удалось открыть «clip.mkv»: файл читается слишком долго"
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
