//! Фоновый поток ленты: индекс -> планирование -> скачивание/разбор -> сообщения.
//!
//! Поток владеет всем изменяемым состоянием ленты (какие сегменты уже обработаны, очередь
//! ожидающих привязки, выученное смещение, серии сбоев). Снаружи его видно только через
//! сообщения, ячейку позиции и токен отмены.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::{Duration, Instant};

use source_core::CancellationToken;

use super::config::StreamSubtitleFeedConfig;
use super::message::{
    StreamSegmentSkipReason, StreamSubtitleEvents, StreamSubtitleMessage,
    StreamTrackUnavailableReason,
};
use super::position::StreamPlaybackPosition;
use super::schedule::{PlaylistOffset, WindowQuery, is_in_window, next_segment};
use crate::stream_subtitles::error::StreamSubtitleError;
use crate::stream_subtitles::model::StreamSubtitleTrackId;
use crate::stream_subtitles::transport::{
    DecodedSegment, PendingSegmentDecode, SegmentDecode, StreamIndexRefresh, StreamSegment,
    StreamSegmentIndex, StreamSegmentKey, StreamSegmentLimits, StreamSubtitleTransport,
    StreamTimelineEpoch,
};

/// Нижняя граница ожидания, когда делать нечего: защита от холостого вращения цикла.
const MINIMUM_IDLE_WAIT: Duration = Duration::from_millis(250);
/// Границы опроса «не появилась ли точка отсчёта времени видео» для ожидающих сегментов.
const MINIMUM_PENDING_POLL: Duration = Duration::from_millis(100);
const MAXIMUM_PENDING_POLL: Duration = Duration::from_secs(1);
/// Пауза между попытками положить сообщение в переполненную очередь.
const FULL_QUEUE_RETRY: Duration = Duration::from_millis(20);
/// Длительность сегмента, если индекс пуст: только для расчёта ожидания.
const FALLBACK_SEGMENT_DURATION: Duration = Duration::from_secs(1);

/// Лента должна остановиться (отмена, закрытый приёмник или терминальное сообщение).
#[derive(Debug)]
pub(super) struct Stop;

/// Результат шага: продолжать или остановиться.
type Step = Result<(), Stop>;

/// Какой раз загружается индекс: первая загрузка при сбое сразу делает дорожку недоступной.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexLoad {
    Initial,
    Reload,
}

/// Сегмент, ждущий точку отсчёта времени видео.
struct PendingEntry {
    key: StreamSegmentKey,
    /// Начало сегмента от начала плейлиста (для выучивания смещения после привязки).
    playlist_start: Duration,
    /// Конец сегмента от начала плейлиста (чтобы понять, дошло ли воспроизведение до него).
    playlist_end: Duration,
    decode: Box<dyn PendingSegmentDecode>,
}

/// Состояние и ресурсы фонового потока.
pub(super) struct FeedWorker {
    transport: Arc<dyn StreamSubtitleTransport>,
    track: StreamSubtitleTrackId,
    config: StreamSubtitleFeedConfig,
    position: StreamPlaybackPosition,
    wake: Arc<dyn Fn() + Send + Sync>,
    cancellation: CancellationToken,
    sender: SyncSender<StreamSubtitleMessage>,
    index: Option<StreamSegmentIndex>,
    /// Когда перечитать живой индекс; `None` - индекс завершён.
    next_refresh_at: Option<Instant>,
    /// Период перечитывания живого индекса (для повтора после сбоя).
    refresh_interval: Duration,
    /// Поколение шкалы `start` сегментов, при котором выучено `offset`.
    timeline_epoch: StreamTimelineEpoch,
    /// Обработанные сегменты (загружены или пропущены) ВНУТРИ окна ленты: повторно не качаем.
    /// Вышедшие из окна забываются (`forget_handled_outside_window`), чтобы перемотка назад
    /// за пределы хранения приложения снова их подтянула.
    handled: HashSet<StreamSegmentKey>,
    pending: VecDeque<PendingEntry>,
    offset: PlaylistOffset,
    segment_failure_streak: u32,
    skipped_since_success: bool,
    index_failure_streak: u32,
    /// Эпоха пробуждения, виденная при последнем плане: прыжок после неё прерывает ожидание.
    seen_wake_epoch: u64,
}

impl FeedWorker {
    pub(super) fn new(
        transport: Arc<dyn StreamSubtitleTransport>,
        track: StreamSubtitleTrackId,
        config: StreamSubtitleFeedConfig,
        position: StreamPlaybackPosition,
        wake: Arc<dyn Fn() + Send + Sync>,
        cancellation: CancellationToken,
        sender: SyncSender<StreamSubtitleMessage>,
    ) -> Self {
        Self {
            transport,
            track,
            config,
            position,
            wake,
            cancellation,
            sender,
            index: None,
            next_refresh_at: None,
            refresh_interval: FALLBACK_SEGMENT_DURATION,
            timeline_epoch: StreamTimelineEpoch::default(),
            handled: HashSet::new(),
            pending: VecDeque::new(),
            offset: PlaylistOffset::Unknown,
            segment_failure_streak: 0,
            skipped_since_success: false,
            index_failure_streak: 0,
            seen_wake_epoch: 0,
        }
    }

    /// Главный цикл потока; возвращается при отмене, закрытом приёмнике или недоступности дорожки.
    pub(super) fn run(mut self) {
        // Результат `Err(Stop)` - штатное завершение; причину уже отправили сообщением (или это отмена).
        let _ = self.run_until_stopped();
    }

    fn run_until_stopped(&mut self) -> Step {
        self.reload_index(IndexLoad::Initial)?;
        loop {
            self.ensure_not_cancelled()?;
            if self.index_refresh_due() {
                self.reload_index(IndexLoad::Reload)?;
            }
            self.resolve_pending()?;
            let snapshot = self.position.snapshot();
            self.seen_wake_epoch = snapshot.wake_epoch;
            self.forget_handled_outside_window(snapshot.position);
            match self.next_segment_to_fetch(snapshot.position) {
                Some(segment) => self.fetch_segment(&segment)?,
                None => self.wait_for_work(),
            }
        }
    }

    fn ensure_not_cancelled(&self) -> Step {
        if self.cancellation.is_cancelled() {
            Err(Stop)
        } else {
            Ok(())
        }
    }

    fn index_refresh_due(&self) -> bool {
        self.next_refresh_at
            .is_some_and(|due| Instant::now() >= due)
    }

    // ----- индекс ---------------------------------------------------------------------------

    /// Загружает индекс; при сбое решает, продолжать ли.
    fn reload_index(&mut self, load: IndexLoad) -> Step {
        match self.transport.load_index(self.track, &self.cancellation) {
            Ok(index) => {
                self.index_failure_streak = 0;
                self.forget_offset_if_timeline_was_reanchored();
                self.install_index(index)
            }
            Err(error) => self.handle_index_failure(load, &error),
        }
    }

    fn handle_index_failure(&mut self, load: IndexLoad, error: &StreamSubtitleError) -> Step {
        let Some(kind) = error.failure_kind() else {
            return Err(Stop);
        };
        self.index_failure_streak = self.index_failure_streak.saturating_add(1);
        tracing::warn!(
            ?kind,
            streak = self.index_failure_streak,
            "stream subtitle index load failed"
        );
        let threshold_reached = self.index_failure_streak >= self.config.failure_threshold.get();
        if load == IndexLoad::Initial || threshold_reached {
            return self.stop_as_unavailable(StreamTrackUnavailableReason::IndexLoadFailed(kind));
        }
        // Живой плейлист мог моргнуть: повторим через обычный период.
        self.next_refresh_at = Some(Instant::now() + self.refresh_interval);
        Ok(())
    }

    /// Транспорт закрепил шкалу `start` заново (разрыв live-плейлиста): выученное смещение
    /// «плейлист -> плеер» относилось к старой шкале и недействительно; учим заново.
    fn forget_offset_if_timeline_was_reanchored(&mut self) {
        let current = self.transport.index_timeline_epoch(self.track);
        if current != self.timeline_epoch {
            self.timeline_epoch = current;
            if matches!(self.offset, PlaylistOffset::Learned(_)) {
                self.offset = PlaylistOffset::Unknown;
            }
        }
    }

    /// Принимает свежий индекс, обновляет расписание и забывает ушедшие сегменты.
    fn install_index(&mut self, index: StreamSegmentIndex) -> Step {
        match index.refresh {
            StreamIndexRefresh::Never => {
                self.next_refresh_at = None;
                if self.index.is_none() {
                    // VOD: шкала плеера строится по длительностям сегментов.
                    self.offset = PlaylistOffset::Identity;
                }
            }
            StreamIndexRefresh::After(interval) => {
                self.refresh_interval = interval;
                self.next_refresh_at = Some(Instant::now() + interval);
            }
        }
        let present: HashSet<StreamSegmentKey> =
            index.segments.iter().map(|segment| segment.key).collect();
        self.handled.retain(|key| present.contains(key));
        self.index = Some(index);
        self.drop_pending_outside(&present)
    }

    /// Ожидающие сегменты, вышедшие из live-окна, привязать уже нельзя.
    fn drop_pending_outside(&mut self, present: &HashSet<StreamSegmentKey>) -> Step {
        let (kept, expired): (VecDeque<_>, VecDeque<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|entry| present.contains(&entry.key));
        self.pending = kept;
        for entry in expired {
            self.abandon_pending(&entry, StreamSegmentSkipReason::LeftPlaylist)?;
        }
        Ok(())
    }

    // ----- планирование и скачивание --------------------------------------------------------

    fn next_segment_to_fetch(&self, position: Duration) -> Option<StreamSegment> {
        let index = self.index.as_ref()?;
        let query = WindowQuery {
            index,
            position,
            ahead: self.config.ahead,
            behind: self.config.behind,
            offset: self.offset,
        };
        let waiting = self.keys_waiting_behind_pending();
        next_segment(&query, |key| {
            self.handled.contains(&key)
                || waiting.contains(&key)
                || self.pending.iter().any(|entry| entry.key == key)
        })
        .cloned()
    }

    fn fetch_segment(&mut self, segment: &StreamSegment) -> Step {
        let limits = StreamSegmentLimits {
            max_bytes: self.config.max_segment_bytes,
        };
        let outcome = self
            .transport
            .fetch_and_decode(segment, limits, &self.cancellation);
        self.ensure_not_cancelled()?;
        self.handled.insert(segment.key);
        match outcome {
            Ok(SegmentDecode::Ready(decoded)) => {
                self.record_success()?;
                self.deliver(segment.key, segment.start, decoded)
            }
            // Ожидание точки отсчёта - ни успех, ни сбой: серию сбоев не сбрасываем.
            Ok(SegmentDecode::Pending(decode)) => self.enqueue_pending(PendingEntry {
                key: segment.key,
                playlist_start: segment.start,
                playlist_end: segment.end(),
                decode,
            }),
            Err(error) => self.record_failure(segment.key, &error),
        }
    }

    /// Пробует привязать ожидающие сегменты (дёшево: без сети).
    fn resolve_pending(&mut self) -> Step {
        if self.pending.is_empty() {
            return Ok(());
        }
        let waiting = std::mem::take(&mut self.pending);
        for entry in waiting {
            let PendingEntry {
                key,
                playlist_start,
                playlist_end,
                decode,
            } = entry;
            match decode.try_resolve() {
                Ok(SegmentDecode::Ready(decoded)) => {
                    self.record_success()?;
                    self.deliver(key, playlist_start, decoded)?;
                }
                Ok(SegmentDecode::Pending(decode)) => self.pending.push_back(PendingEntry {
                    key,
                    playlist_start,
                    playlist_end,
                    decode,
                }),
                Err(error) => self.record_failure(key, &error)?,
            }
        }
        Ok(())
    }

    fn enqueue_pending(&mut self, entry: PendingEntry) -> Step {
        let capacity = self.config.max_pending_segments.get();
        while self.pending.len() >= capacity {
            let Some(evicted) = self.pending.pop_front() else {
                break;
            };
            self.abandon_pending(&evicted, StreamSegmentSkipReason::PendingOverflow)?;
        }
        self.pending.push_back(entry);
        Ok(())
    }

    /// Сегмент оставлен без привязки (вытеснен или ушёл из плейлиста).
    ///
    /// Сегмент снова доступен для скачивания. Сбоем он считается ТОЛЬКО если воспроизведение
    /// уже прошло его конец: видео к этому моменту обязано было опубликовать точку отсчёта
    /// эпохи, а её нет - постоянное расхождение кончается ровно одним `TrackUnavailable`.
    /// Пока позиция раньше сегмента (например, окно вперёд зашло в эпоху после разрыва, до
    /// которой видео ещё не дошло), отсутствие точки отсчёта - норма и сбоем не считается.
    fn abandon_pending(&mut self, entry: &PendingEntry, reason: StreamSegmentSkipReason) -> Step {
        let segment = entry.key;
        self.handled.remove(&segment);
        let passed = self.playback_has_passed(entry.playlist_end);
        tracing::warn!(
            segment = segment.value(),
            ?reason,
            counted_as_failure = passed,
            "stream subtitle segment left without its timeline origin"
        );
        self.emit(StreamSubtitleMessage::SegmentSkipped { segment, reason })?;
        if passed {
            self.count_segment_failure()
        } else {
            Ok(())
        }
    }

    /// Позиция воспроизведения на шкале плейлиста, микросекунды; `None` - смещение ещё не
    /// выучено (live до первой точной привязки): тогда считаем, что до сегментов не дошли.
    fn position_in_playlist_micros(&self) -> Option<i64> {
        let offset = match self.offset {
            PlaylistOffset::Identity => 0,
            PlaylistOffset::Learned(offset) => offset,
            PlaylistOffset::Unknown => return None,
        };
        let position = i64::try_from(self.position.snapshot().position.as_micros()).ok()?;
        Some(position.saturating_sub(offset))
    }

    /// Прошло ли воспроизведение конец сегмента (по шкале плейлиста).
    fn playback_has_passed(&self, playlist_end: Duration) -> bool {
        let Some(position) = self.position_in_playlist_micros() else {
            return false;
        };
        i64::try_from(playlist_end.as_micros()).is_ok_and(|end| position >= end)
    }

    /// Сегменты, которые пока не стоит скачивать: они не раньше самого раннего ожидающего
    /// сегмента и воспроизведение до них не дошло. Раз ожидающий сегмент ждёт точку отсчёта
    /// эпохи, последующие (обычно той же эпохи) ждали бы так же, переполняя список ожидания.
    /// Как только ожидающий сегмент привяжется, ограничение пропадает само.
    fn keys_waiting_behind_pending(&self) -> HashSet<StreamSegmentKey> {
        let Some(barrier) = self.pending.iter().map(|entry| entry.playlist_start).min() else {
            return HashSet::new();
        };
        let position = self.position_in_playlist_micros();
        let Some(index) = self.index.as_ref() else {
            return HashSet::new();
        };
        index
            .segments
            .iter()
            .filter(|segment| segment.start >= barrier)
            .filter(|segment| {
                let start = i64::try_from(segment.start.as_micros()).unwrap_or(i64::MAX);
                // Позиция внутри или после сегмента - до него дошли, ждать нечего.
                position.is_none_or(|position| start > position)
            })
            .map(|segment| segment.key)
            .collect()
    }

    /// Забывает обработанные сегменты, вышедшие из окна ленты: они снова скачиваемы, если
    /// позиция вернётся к ним (приложение могло уже вытеснить их реплики по сроку хранения).
    /// Дубли реплик при повторной выдаче снимает шкала приложения.
    fn forget_handled_outside_window(&mut self, position: Duration) {
        let Some(index) = self.index.as_ref() else {
            return;
        };
        let query = WindowQuery {
            index,
            position,
            ahead: self.config.ahead,
            behind: self.config.behind,
            offset: self.offset,
        };
        let outside: Vec<StreamSegmentKey> = index
            .segments
            .iter()
            .filter(|segment| is_in_window(&query, segment) == Some(false))
            .map(|segment| segment.key)
            .collect();
        for key in outside {
            self.handled.remove(&key);
        }
    }

    /// Учит смещение «плейлист -> плеер» и отправляет реплики.
    fn deliver(
        &mut self,
        key: StreamSegmentKey,
        playlist_start: Duration,
        decoded: DecodedSegment,
    ) -> Step {
        self.learn_offset(playlist_start, &decoded);
        self.emit(StreamSubtitleMessage::Events(StreamSubtitleEvents {
            segment: key,
            events: decoded.events,
            covered: decoded.covered,
        }))
    }

    fn learn_offset(&mut self, playlist_start: Duration, decoded: &DecodedSegment) {
        // У VOD смещение фиксировано (Identity): шкала плеера построена по тем же EXTINF.
        if self.offset == PlaylistOffset::Identity {
            return;
        }
        let Some(player_start) = decoded.segment_start else {
            return;
        };
        let Ok(playlist_micros) = i64::try_from(playlist_start.as_micros()) else {
            return;
        };
        if let Some(offset) = player_start.as_micros().checked_sub(playlist_micros) {
            self.offset = PlaylistOffset::Learned(offset);
        }
    }

    // ----- сбои -----------------------------------------------------------------------------

    fn record_success(&mut self) -> Step {
        self.segment_failure_streak = 0;
        if std::mem::take(&mut self.skipped_since_success) {
            self.emit(StreamSubtitleMessage::Recovered)?;
        }
        Ok(())
    }

    fn record_failure(&mut self, segment: StreamSegmentKey, error: &StreamSubtitleError) -> Step {
        let Some(kind) = error.failure_kind() else {
            return Err(Stop);
        };
        tracing::warn!(
            segment = segment.value(),
            ?kind,
            transient = error.is_transient(),
            "stream subtitle segment skipped"
        );
        self.emit(StreamSubtitleMessage::SegmentSkipped {
            segment,
            reason: StreamSegmentSkipReason::Failed(kind),
        })?;
        self.count_segment_failure()
    }

    /// Учитывает сбой сегмента в серии; по достижении порога дорожка становится недоступной.
    fn count_segment_failure(&mut self) -> Step {
        self.segment_failure_streak = self.segment_failure_streak.saturating_add(1);
        self.skipped_since_success = true;
        if self.segment_failure_streak >= self.config.failure_threshold.get() {
            return self.stop_as_unavailable(
                StreamTrackUnavailableReason::ConsecutiveSegmentFailures {
                    count: self.segment_failure_streak,
                },
            );
        }
        Ok(())
    }

    /// Единственное место, где отправляется `TrackUnavailable`; после него лента завершается.
    fn stop_as_unavailable(&mut self, reason: StreamTrackUnavailableReason) -> Step {
        self.emit(StreamSubtitleMessage::TrackUnavailable(reason))?;
        Err(Stop)
    }

    // ----- ожидание и отправка --------------------------------------------------------------

    /// Ждёт прыжок позиции, срок перечитывания индекса или очередной опрос ожидающих сегментов.
    fn wait_for_work(&self) {
        let longest_segment = self
            .index
            .as_ref()
            .and_then(|index| index.segments.iter().map(|segment| segment.duration).max())
            .unwrap_or(FALLBACK_SEGMENT_DURATION);
        let idle = (longest_segment / 2)
            .clamp(MINIMUM_IDLE_WAIT, self.config.ahead.max(MINIMUM_IDLE_WAIT));
        let mut timeout = if self.pending.is_empty() {
            idle
        } else {
            (idle / 4).clamp(MINIMUM_PENDING_POLL, MAXIMUM_PENDING_POLL)
        };
        if let Some(due) = self.next_refresh_at {
            timeout = timeout.min(due.saturating_duration_since(Instant::now()));
        }
        self.position.wait_for_wake(self.seen_wake_epoch, timeout);
    }

    /// Кладёт сообщение в ограниченную очередь (при переполнении ждёт, но слышит отмену), затем будит UI.
    fn emit(&self, message: StreamSubtitleMessage) -> Step {
        let mut message = message;
        loop {
            self.ensure_not_cancelled()?;
            match self.sender.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    message = returned;
                    std::thread::sleep(FULL_QUEUE_RETRY);
                }
                // Приложение закрыло приёмник: слушать больше некому.
                Err(TrySendError::Disconnected(_)) => return Err(Stop),
            }
        }
        (self.wake)();
        Ok(())
    }
}
