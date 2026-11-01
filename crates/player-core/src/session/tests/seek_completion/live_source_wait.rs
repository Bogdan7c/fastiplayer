//! Срок перемотки и живой поставщик данных (решения 150–151,
//! `seek_completion::live_source_wait`).
//!
//! Тесты идут настоящими тиками сессии в реальном времени с коротким сроком перемотки:
//! отметку «источник ждёт живого поставщика» ставит само чтение демуксера, поэтому
//! подменять время нельзя — иначе отметка и проверка срока жили бы на разных часах.

use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use media_core::{DemuxDataWait, DemuxRetryHint, LiveProducerProgress};

/// Короткий срок перемотки: тест проходит за доли секунды, а ожидание «дольше срока»
/// остаётся настоящим.
const SHORT_COMMIT_TIMEOUT: Duration = Duration::from_millis(60);

/// Короткий предел молчания живого поставщика (по умолчанию 60 с).
const SHORT_STALL_TIMEOUT: Duration = Duration::from_millis(150);

/// Сколько тест ждёт перемотку: в несколько раз дольше обоих сроков.
const WAIT_LONGER_THAN_TIMEOUT: Duration = Duration::from_millis(400);

/// Что источник отвечает на «чего ждёшь».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProducerAnswer {
    /// Обычный источник: не уточняет.
    Unattributed,
    /// Живой поставщик на связи, но ничего нового не несёт (счётчик стоит).
    LiveSilent,
    /// Живой поставщик несёт байты (счётчик растёт с каждым чтением).
    LiveTrickling,
}

/// Цель перемотки.
const SEEK_TARGET: Duration = Duration::from_secs(5);

/// Что источник отдаёт на чтение во время перемотки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceSupply {
    /// Данных нет: каждое чтение — «данных пока нет».
    Dry,
    /// Данные есть: каждое чтение — новый пакет видео после цели (плеер сам их не
    /// успевает переварить — декодера нет).
    Plentiful,
}

/// Источник с заданным ответом «чего жду» и заданной подачей.
struct WaitAwareDemuxer {
    tracks: Vec<TrackInfo>,
    answer: ProducerAnswer,
    supply: SourceSupply,
    next_packet_pts: Duration,
    /// Сколько «принёс» поставщик (растёт только у `LiveTrickling`).
    delivered_bytes: AtomicU64,
}

impl WaitAwareDemuxer {
    fn new(tracks: Vec<TrackInfo>, answer: ProducerAnswer, supply: SourceSupply) -> Self {
        Self {
            tracks,
            answer,
            supply,
            next_packet_pts: SEEK_TARGET,
            delivered_bytes: AtomicU64::new(0),
        }
    }
}

impl Demuxer for WaitAwareDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn data_wait(&self) -> DemuxDataWait {
        let progress = LiveProducerProgress::from_delivered_bytes(
            self.delivered_bytes.load(Ordering::Relaxed),
        );
        match self.answer {
            ProducerAnswer::Unattributed => DemuxDataWait::Unattributed,
            ProducerAnswer::LiveSilent | ProducerAnswer::LiveTrickling => {
                DemuxDataWait::LiveProducer { progress }
            }
        }
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if self.answer == ProducerAnswer::LiveTrickling {
            // Страница принесла ещё кусочек ожидаемого сегмента (сегмент не готов).
            self.delivered_bytes.fetch_add(512, Ordering::Relaxed);
        }
        match self.supply {
            SourceSupply::Dry => Ok(DemuxReadEvent::TemporarilyUnavailable(DemuxRetryHint::new(
                DemuxRetryHint::MIN_RETRY_AFTER,
            )?)),
            SourceSupply::Plentiful => {
                let packet = fake_video_packet(TrackId::new(1), self.next_packet_pts);
                self.next_packet_pts += Duration::from_millis(16);
                Ok(DemuxReadEvent::Packet(packet))
            }
        }
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
        Ok(media_core::DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }

    /// Любой режим перемотки садится ровно на цель (как поток браузера вне полки).
    fn seek_with_request(
        &mut self,
        request: media_core::DemuxSeekRequest,
    ) -> anyhow::Result<media_core::DemuxSeekResult> {
        self.seek(request.timestamp)
    }
}

/// Сессия с видео, играющая, с начатой перемоткой к [`SEEK_TARGET`].
fn seeking_session(answer: ProducerAnswer, supply: SourceSupply) -> PlayerSession {
    let mut session = PlayerSession::new();
    let tracks = vec![fake_track(1, TrackKind::Video)];
    session.pipeline.install_opened_media(
        Box::new(WaitAwareDemuxer::new(tracks.clone(), answer, supply)),
        None,
        None,
        tracks.clone(),
    );
    session
        .select_default_video_track(&tracks, "the fixture has a video track")
        .expect("video track selected");
    session.snapshot.media_instance_id = Some(crate::MediaInstanceId::new_unique());
    session.set_snapshot_duration(Some(Duration::from_secs(60)));
    session.apply_demux_seekability(media_core::DemuxSeekability::Seekable);
    session.set_playback_state(PlaybackState::Paused);
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_duration(SEEK_TARGET),
        )))
        .unwrap();
    assert!(
        session.seek_commit().is_some(),
        "the seek is pending: {:?} {:?} {:?}",
        session.snapshot().playback_state,
        session.snapshot().last_error,
        session.snapshot().timeline
    );
    session
}

/// Шаг плеера с короткими сроками перемотки и молчания.
fn short_budget_config() -> PlayerTickConfig {
    PlayerTickConfig {
        seek_commit_timeout: SHORT_COMMIT_TIMEOUT,
        seek_live_source_stall_timeout: SHORT_STALL_TIMEOUT,
        ..PlayerTickConfig::default()
    }
}

/// Тикает сессию в реальном времени `duration` с коротким сроком перемотки.
fn tick_for(session: &mut PlayerSession, duration: Duration) {
    let tick_config = short_budget_config();
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let _tick = session.tick(PlayerTickContext::with_config(Instant::now(), tick_config));
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Отчёт сессии об ошибке перемотки по сроку.
fn reported_seek_timeout(session: &PlayerSession) -> bool {
    session
        .snapshot()
        .last_error
        .as_ref()
        .is_some_and(|error| error.kind == PlayerErrorKind::SeekTimeout)
}

/// Страница несёт байты для цели дольше обоих сроков (кусочек за кусочком): перемотка
/// не сдаётся, плеер стоит в перемотке (спиннер), ошибки нет.
#[test]
fn a_seek_waits_without_limit_while_the_live_producer_keeps_delivering() {
    let mut session = seeking_session(ProducerAnswer::LiveTrickling, SourceSupply::Dry);

    tick_for(&mut session, WAIT_LONGER_THAN_TIMEOUT);

    assert!(session.seek_commit().is_some(), "the seek still waits");
    // Плеер стоит в перемотке к цели (окно показывает спиннер), а не на паузе.
    assert!(matches!(
        session.snapshot().playback_state,
        PlaybackState::Scrubbing | PlaybackState::Seeking
    ));
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_duration(SEEK_TARGET))
    );
    assert!(!reported_seek_timeout(&session));
}

/// Страница на связи, но ни байта для цели (решение 151): дольше срока перемотки
/// перемотка ждёт, а по пределу молчания честно сдаётся «Seeking timed out».
#[test]
fn a_silent_live_producer_is_awaited_only_up_to_the_stall_timeout() {
    let mut session = seeking_session(ProducerAnswer::LiveSilent, SourceSupply::Dry);

    // Срок перемотки прошёл, предел молчания — ещё нет: ждём.
    tick_for(
        &mut session,
        SHORT_COMMIT_TIMEOUT + Duration::from_millis(30),
    );
    assert!(
        session.seek_commit().is_some(),
        "waited past the commit timeout"
    );
    assert!(!reported_seek_timeout(&session));

    tick_for(&mut session, WAIT_LONGER_THAN_TIMEOUT);
    assert!(session.seek_commit().is_none());
    assert!(reported_seek_timeout(&session));
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
}

/// Обычный источник (файл) ничего не уточняет: старая защита срабатывает по сроку.
#[test]
fn an_ordinary_source_still_times_out() {
    let mut session = seeking_session(ProducerAnswer::Unattributed, SourceSupply::Dry);

    tick_for(
        &mut session,
        SHORT_COMMIT_TIMEOUT + Duration::from_millis(60),
    );

    assert!(session.seek_commit().is_none());
    assert!(reported_seek_timeout(&session));
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
}

/// Данные у источника есть, а застряли у плеера (декодера нет, кадр не выходит):
/// это настоящий сбой — срок срабатывает, хотя поставщик жив и несёт байты.
#[test]
fn a_pipeline_stuck_with_data_at_hand_still_times_out_for_a_live_producer() {
    let mut session = seeking_session(ProducerAnswer::LiveTrickling, SourceSupply::Plentiful);

    tick_for(&mut session, WAIT_LONGER_THAN_TIMEOUT);

    assert!(session.seek_commit().is_none());
    assert!(reported_seek_timeout(&session));
}

/// Момент отказа считается от последнего честного ожидания, а при молчании — не
/// позже предела молчания от начала перемотки.
#[test]
fn the_deadline_follows_the_last_wait_but_never_past_the_silence_limit() {
    let mut session = seeking_session(ProducerAnswer::LiveSilent, SourceSupply::Dry);
    tick_for(
        &mut session,
        SHORT_COMMIT_TIMEOUT + Duration::from_millis(30),
    );
    let seek_commit = session.seek_commit().expect("still seeking");
    let wait = session
        .live_producer_wait()
        .expect("the source answered as a live producer");
    assert!(wait.last_wait_at > seek_commit.started_at + SHORT_COMMIT_TIMEOUT);
    let budget = crate::session::seek_completion::live_source_wait::SeekCommitBudget {
        commit_timeout: SHORT_COMMIT_TIMEOUT,
        live_source_stall_timeout: SHORT_STALL_TIMEOUT,
    };
    let deadline = session
        .seek_commit_deadline(seek_commit, budget)
        .expect("finite deadline");
    assert!(deadline > seek_commit.started_at + SHORT_COMMIT_TIMEOUT);
    assert!(deadline <= wait.last_progress_at.max(seek_commit.started_at) + SHORT_STALL_TIMEOUT);
}
