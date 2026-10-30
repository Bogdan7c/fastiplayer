//! Журнал воспроизведения браузерного режима: что «зритель» увидел и услышал.
//!
//! Окна в процессе браузерного режима пока нет (решения 88, 92), поэтому наблюдаемый итог
//! прогона — строки журнала: e2e-тесты и живой прогон 03b читают их, а не экран.
//!
//! Устройство модуля — три слоя, которые не смешиваются:
//! 1. **Счёт** ([`PlaybackJournal`]) — чистая логика без времени ОС и без журнала:
//!    счётчики кадров, звука, загрузок, буферизаций и откатов позиции назад;
//! 2. **Формат** ([`PlaybackSummary`] через `Display`) — одна строка `key=value`;
//! 3. **Вывод** (`log_*`) — tracing `info!` с этой строкой.
//!
//! ПРИВАТНОСТЬ (инвариант 7): журнал лежит на диске, поэтому домен, заголовок, адрес и
//! содержимое страницы сюда не попадают никогда. Журнал устроен так, что ему некуда их
//! положить: в сводке только числа и фиксированные метки. Тест кормит журнал событием
//! с заголовком и проверяет, что в строке его нет.

use std::fmt;
use std::time::{Duration, Instant};

use player_core::{MediaInstanceId, PlaybackState, PlayerErrorKind, PlayerEvent};
use tracing::info;

use super::playback::metered_audio::AudioMeterReading;

/// Как часто журнал пишет промежуточную сводку во время воспроизведения.
///
/// 10 с — компромисс: e2e-прогон на 15–30 с видит хотя бы одну строку «по ходу»
/// (зависание видно до конца прогона), а двухчасовой фильм даёт ~720 строк, а не
/// тысячи. Это политика журнала, а не настройка пользователя, поэтому не в config.
pub(crate) const BROWSER_PLAYBACK_SUMMARY_INTERVAL: Duration = Duration::from_secs(10);

/// Числовые счётчики прогона (все монотонно растут).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlaybackCounters {
    /// Кадров, которые плеер вывел на показ (телеметрия тиков worker-а).
    pub(crate) presented_frames: u64,
    /// Кадров, которые забрал вывод видео (рендер или безоконный счётчик).
    pub(crate) taken_frames: u64,
    /// Шагов позиции назад, которые не объясняются перемоткой или новой загрузкой.
    pub(crate) position_regressions: u64,
    /// Сколько раз загружалось новое медиа (поколения потока).
    pub(crate) loads: u64,
    /// Сколько раз плеер вошёл в ожидание данных (Buffering).
    pub(crate) buffering_entries: u64,
}

/// Что журнал заметил и о чём вызывающий должен сообщить отдельной строкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalNotice {
    /// Плеер дошёл до конца текущего медиа.
    PlaybackEnded,
}

/// Сводка для строки журнала: только числа и фиксированные метки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackSummary {
    /// Счётчики на момент сводки.
    pub(crate) counters: PlaybackCounters,
    /// Последнее показание счётчика звука.
    pub(crate) audio: AudioMeterReading,
    /// Позиция воспроизведения.
    pub(crate) position: Duration,
    /// Состояние плеера.
    pub(crate) state: PlaybackState,
}

/// Счётчик прогона браузерного режима.
#[derive(Debug)]
pub(crate) struct PlaybackJournal {
    /// Числовые счётчики.
    counters: PlaybackCounters,
    /// Сторож монотонности позиции.
    regressions: PositionRegressionGuard,
    /// Последнее показание звука.
    audio: AudioMeterReading,
    /// Последняя позиция.
    position: Duration,
    /// Последнее состояние (для переходов «вошёл в Buffering», «дошёл до Ended»).
    state: PlaybackState,
    /// Ритм промежуточных сводок.
    cadence: SummaryCadence,
}

impl PlaybackJournal {
    /// Новый журнал; первая промежуточная сводка — через `interval` после `started_at`.
    pub(crate) fn new(started_at: Instant, interval: Duration) -> Self {
        Self {
            counters: PlaybackCounters::default(),
            regressions: PositionRegressionGuard::default(),
            audio: AudioMeterReading::default(),
            position: Duration::ZERO,
            state: PlaybackState::Idle,
            cadence: SummaryCadence {
                interval,
                last_summary_at: started_at,
            },
        }
    }

    /// Запрошена загрузка нового медиа (поколения).
    ///
    /// Шкалу времени здесь не сбрасываем: пока плеер не установил новое медиа, снимки
    /// ещё приходят от прежнего. Новая шкала начинается со сменой экземпляра медиа в
    /// снимке (см. [`Self::note_snapshot`]).
    pub(crate) fn note_media_load(&mut self) {
        self.counters.loads += 1;
    }

    /// Перемотка запрошена: позиция может уйти назад, пока плеер её не подтвердит.
    pub(crate) fn note_seek_requested(&mut self) {
        self.regressions.start_seek();
    }

    /// Событие плеера. Метаданные медиа (заголовок, метка источника) игнорируются.
    pub(crate) fn note_player_event(&mut self, event: &PlayerEvent) {
        match event {
            PlayerEvent::SeekCommitted(_) => self.regressions.finish_seek(),
            // Перемотка, которая не состоится, тоже закрывает окно «назад можно».
            PlayerEvent::RecoverableError(error) | PlayerEvent::FatalError(error)
                if is_seek_failure(&error.kind) =>
            {
                self.regressions.finish_seek();
            }
            _ => {}
        }
    }

    /// Кадры, выведенные плеером на показ с прошлого вызова.
    pub(crate) fn note_presented_frames(&mut self, frames: u64) {
        self.counters.presented_frames += frames;
    }

    /// Кадры, забранные выводом видео с прошлого вызова.
    pub(crate) fn note_taken_frames(&mut self, frames: u64) {
        self.counters.taken_frames += frames;
    }

    /// Последнее показание счётчика звука (накопительное, заменяет прежнее).
    pub(crate) fn note_audio(&mut self, reading: AudioMeterReading) {
        self.audio = reading;
    }

    /// Свежие позиция и состояние плеера.
    ///
    /// `media_instance` — экземпляр медиа, к которому относится позиция: его смена —
    /// новая шкала времени (сравнивать с прежней позицией нельзя). Возвращает заметку,
    /// если плеер только что дошёл до конца.
    pub(crate) fn note_snapshot(
        &mut self,
        media_instance: Option<MediaInstanceId>,
        position: Duration,
        state: PlaybackState,
    ) -> Option<JournalNotice> {
        let step = if state == PlaybackState::Ended {
            self.regressions.observe_end_of_media(media_instance)
        } else {
            self.regressions.observe(media_instance, position)
        };
        if step == PositionStep::UnexplainedRegression {
            self.counters.position_regressions += 1;
        }
        self.position = position;
        let previous_state = std::mem::replace(&mut self.state, state);
        if state == previous_state {
            return None;
        }
        match state {
            PlaybackState::Buffering => {
                self.counters.buffering_entries += 1;
                None
            }
            PlaybackState::Ended => Some(JournalNotice::PlaybackEnded),
            _ => None,
        }
    }

    /// Промежуточная сводка, если пришло её время (иначе `None`).
    pub(crate) fn take_due_summary(&mut self, now: Instant) -> Option<PlaybackSummary> {
        self.cadence.take_due(now).then(|| self.summary())
    }

    /// Сводка на текущий момент (для финальной строки и ответа вызывающему).
    pub(crate) fn summary(&self) -> PlaybackSummary {
        PlaybackSummary {
            counters: self.counters,
            audio: self.audio,
            position: self.position,
            state: self.state,
        }
    }
}

/// Относится ли ошибка к несостоявшейся перемотке.
const fn is_seek_failure(kind: &PlayerErrorKind) -> bool {
    matches!(
        kind,
        PlayerErrorKind::SeekUnavailable
            | PlayerErrorKind::SeekTimeout
            | PlayerErrorKind::SeekTargetExpired
    )
}

/// Ритм промежуточных сводок.
#[derive(Debug)]
struct SummaryCadence {
    /// Промежуток между сводками.
    interval: Duration,
    /// Когда была прошлая (или старт журнала).
    last_summary_at: Instant,
}

impl SummaryCadence {
    /// `true` и сдвиг отметки, если с прошлой сводки прошло не меньше `interval`.
    fn take_due(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.last_summary_at) < self.interval {
            return false;
        }
        self.last_summary_at = now;
        true
    }
}

/// Итог одного шага позиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositionStep {
    /// Вперёд, на месте или законный откат (после перемотки/загрузки).
    Accepted,
    /// Позиция ушла назад без причины — то, что зритель увидел бы как скачок назад.
    UnexplainedRegression,
}

/// Сторож монотонности позиции.
///
/// Правило: позиция не должна идти назад. Исключения — новый экземпляр медиа (новая
/// шкала: сравнивать не с чем) и перемотка: от запроса до `SeekCommitted` (или ошибки
/// перемотки) позиция может прыгать как угодно, а первая позиция после подтверждения
/// становится новой точкой отсчёта.
#[derive(Debug, Default)]
struct PositionRegressionGuard {
    /// Экземпляр медиа, к которому относится `last_position`.
    media_instance: Option<MediaInstanceId>,
    /// Прошлая позиция; `None` — точки отсчёта ещё нет.
    last_position: Option<Duration>,
    /// Идёт ли перемотка, которую плеер ещё не подтвердил.
    seek_in_flight: bool,
}

impl PositionRegressionGuard {
    /// Запрошена перемотка: до подтверждения позиции не сравниваются.
    fn start_seek(&mut self) {
        self.seek_in_flight = true;
    }

    /// Перемотка подтверждена или сорвалась: следующая позиция — новая точка отсчёта.
    fn finish_seek(&mut self) {
        if self.seek_in_flight {
            self.seek_in_flight = false;
            self.last_position = None;
        }
    }

    /// Плеер дошёл до конца: позиция последнего снимка не сравнивается.
    ///
    /// В конце плеер ставит позицию ровно на длительность контейнера, а звуковая
    /// дорожка бывает длиннее её на долю кадра (AC-3 фикстуры: 1,024 с при длительности
    /// 1,000 с) — последний шаг выглядит как откат на 24 мс, хотя зритель ничего назад
    /// не видел: воспроизведение уже кончилось. После конца отсчёт начинается заново.
    fn observe_end_of_media(&mut self, media_instance: Option<MediaInstanceId>) -> PositionStep {
        self.media_instance = media_instance;
        self.last_position = None;
        PositionStep::Accepted
    }

    /// Учитывает новую позицию экземпляра `media_instance`.
    fn observe(
        &mut self,
        media_instance: Option<MediaInstanceId>,
        position: Duration,
    ) -> PositionStep {
        if media_instance != self.media_instance {
            // Новое медиа: точки отсчёта нет, перемотка прежнего к нему не относится.
            self.media_instance = media_instance;
            self.last_position = None;
            self.seek_in_flight = false;
        }
        if self.seek_in_flight {
            return PositionStep::Accepted;
        }
        let step = match self.last_position {
            Some(last) if position < last => PositionStep::UnexplainedRegression,
            _ => PositionStep::Accepted,
        };
        self.last_position = Some(position);
        step
    }
}

impl fmt::Display for PlaybackSummary {
    /// Одна строка `key=value` через пробел; ключи стабильны (их читают тесты и разбор).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let counters = &self.counters;
        write!(
            formatter,
            "presented_frames={} taken_frames={} audio_written_ms={} audio_played_ms={} \
             audio_output={} position_regressions={} loads={} buffering_entries={} \
             position_ms={} state={}",
            counters.presented_frames,
            counters.taken_frames,
            self.audio.written.as_millis(),
            self.audio.played.as_millis(),
            self.audio.output.log_label(),
            counters.position_regressions,
            counters.loads,
            counters.buffering_entries,
            self.position.as_millis(),
            playback_state_label(self.state),
        )
    }
}

/// Стабильная метка состояния плеера для строки журнала.
const fn playback_state_label(state: PlaybackState) -> &'static str {
    match state {
        PlaybackState::Idle => "idle",
        PlaybackState::Opening => "opening",
        PlaybackState::Paused => "paused",
        PlaybackState::Playing => "playing",
        PlaybackState::Buffering => "buffering",
        PlaybackState::Seeking => "seeking",
        PlaybackState::Scrubbing => "scrubbing",
        PlaybackState::Draining => "draining",
        PlaybackState::Ended => "ended",
        PlaybackState::Stopped => "stopped",
        PlaybackState::Failed => "failed",
    }
}

/// Какая это сводка: по ходу или итоговая при завершении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SummaryKind {
    /// Промежуточная, раз в [`BROWSER_PLAYBACK_SUMMARY_INTERVAL`].
    Periodic,
    /// Итоговая, при завершении воспроизведения.
    Final,
}

impl SummaryKind {
    /// Метка в строке журнала.
    const fn log_label(self) -> &'static str {
        match self {
            Self::Periodic => "periodic",
            Self::Final => "final",
        }
    }
}

/// Пишет сводку в журнал: `Browser playback summary kind=… presented_frames=… …`.
pub(crate) fn log_playback_summary(kind: SummaryKind, summary: &PlaybackSummary) {
    info!(
        "Browser playback summary kind={} {summary}",
        kind.log_label()
    );
}

/// Пишет в журнал, что плеер дошёл до конца текущего медиа.
pub(crate) fn log_playback_ended(summary: &PlaybackSummary) {
    info!("Browser playback ended {summary}");
}

#[cfg(test)]
mod tests;
