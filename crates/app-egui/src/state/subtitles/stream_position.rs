//! Позиция воспроизведения для ленты субтитров потока: наблюдение за плеером и решение,
//! «продвинулась позиция» или «прыгнула».
//!
//! # Шкала времени
//! Реплики потока лежат на шкале ПЛЕЕРА, а кадр ищет их по ВРЕМЕНИ ПОИСКА (позиция плеера минус
//! сдвиг синхронизации пользователя). Поэтому в ячейку ленты пишется время поиска: окно
//! загрузки строится вокруг того, что нужно кадру. Сюда приходят уже пересчитанные позиции;
//! пересчёт и прыжок при смене сдвига делает `owner_stream`.
//!
//! # Прыжок и продвижение
//! `advance_to` ленту не будит (она подхватит позицию в очередной проверке), `jump_to` будит
//! сразу, чтобы лента пересчитала окно вокруг новой позиции. Прыжком считаются: новая цель
//! seek/scrub, завершение seek (позиция осела на цели), движение назад и скачок вперёд дальше
//! [`FORWARD_JUMP_THRESHOLD`].

use std::time::Duration;

use media_core::TimelineMode;
use player_core::PlayerSnapshot;

/// Скачок вперёд дальше этого за один кадр интерфейса - не обычное воспроизведение, а прыжок
/// (например, после долгой остановки UI). Обычный ход кадра - миллисекунды.
const FORWARD_JUMP_THRESHOLD: Duration = Duration::from_secs(10);

/// Какая шкала у источника: у живой трансляции реплики старше истории вытесняются.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::state::subtitles) enum StreamTimelineKind {
    /// Конечное видео: реплики не вытесняются (лента не пришлёт обработанное повторно).
    #[default]
    Vod,
    /// Живая трансляция: память ограничена историей `live_retention_s`.
    Live,
}

/// Наблюдение за плеером, нужное ленте потоковых субтитров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct StreamPlaybackObservation {
    /// Позиция на шкале плеера: цель в seek/scrub, иначе текущая позиция.
    pub(in crate::state::subtitles) position: Duration,
    /// Цель идущего seek/scrub; `None` - воспроизведение или пауза.
    pub(in crate::state::subtitles) seek_target: Option<Duration>,
    /// VOD или live.
    pub(in crate::state::subtitles) timeline: StreamTimelineKind,
}

impl StreamPlaybackObservation {
    /// Собирает наблюдение из снимка плеера.
    pub(in crate::state::subtitles) fn from_snapshot(snapshot: &PlayerSnapshot) -> Self {
        let timeline = &snapshot.timeline;
        let target = timeline.target_position.map(|target| target.as_duration());
        let seek_target = target.filter(|_| timeline.scrubbing || timeline.seeking);
        Self {
            position: seek_target.unwrap_or(snapshot.current_position),
            seek_target,
            timeline: match timeline.mode {
                TimelineMode::Live => StreamTimelineKind::Live,
                TimelineMode::Static => StreamTimelineKind::Vod,
            },
        }
    }
}

/// Что делать с ячейкой позиции ленты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum PositionUpdate {
    /// Позиция не изменилась.
    Unchanged,
    /// Обычное продвижение: записать без пробуждения ленты.
    Advance(Duration),
    /// Разрыв: записать и разбудить ленту.
    Jump(Duration),
}

/// Различает продвижение и прыжок по последовательности наблюдений.
#[derive(Debug, Default)]
pub(in crate::state::subtitles) struct StreamPositionTracker {
    /// Позиция последнего наблюдения.
    last_position: Option<Duration>,
    /// Цель seek/scrub, о которой ленту уже известили.
    announced_seek_target: Option<Duration>,
}

impl StreamPositionTracker {
    /// Решает, как обновить ячейку позиции по новому наблюдению.
    pub(in crate::state::subtitles) fn update(
        &mut self,
        observation: &StreamPlaybackObservation,
    ) -> PositionUpdate {
        if let Some(target) = observation.seek_target {
            self.last_position = Some(target);
            // Цель обновляется на каждом шаге scrub: будим ленту только при новой цели.
            return if self.announced_seek_target.replace(target) == Some(target) {
                PositionUpdate::Unchanged
            } else {
                PositionUpdate::Jump(target)
            };
        }
        let position = observation.position;
        let seek_just_settled = self.announced_seek_target.take().is_some();
        let previous = self.last_position.replace(position);
        let Some(previous) = previous else {
            return PositionUpdate::Jump(position);
        };
        if seek_just_settled
            || position < previous
            || position.saturating_sub(previous) > FORWARD_JUMP_THRESHOLD
        {
            PositionUpdate::Jump(position)
        } else if position == previous {
            PositionUpdate::Unchanged
        } else {
            PositionUpdate::Advance(position)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(seconds: u64) -> StreamPlaybackObservation {
        StreamPlaybackObservation {
            position: Duration::from_secs(seconds),
            seek_target: None,
            timeline: StreamTimelineKind::Vod,
        }
    }

    fn seeking(target_seconds: u64) -> StreamPlaybackObservation {
        StreamPlaybackObservation {
            position: Duration::from_secs(target_seconds),
            seek_target: Some(Duration::from_secs(target_seconds)),
            timeline: StreamTimelineKind::Vod,
        }
    }

    #[test]
    fn first_observation_is_a_jump_then_steady_playback_only_advances() {
        let mut tracker = StreamPositionTracker::default();
        assert_eq!(
            tracker.update(&playing(5)),
            PositionUpdate::Jump(Duration::from_secs(5))
        );
        assert_eq!(
            tracker.update(&playing(6)),
            PositionUpdate::Advance(Duration::from_secs(6))
        );
        assert_eq!(tracker.update(&playing(6)), PositionUpdate::Unchanged);
    }

    #[test]
    fn seek_target_is_announced_once_and_settling_is_a_jump() {
        let mut tracker = StreamPositionTracker::default();
        tracker.update(&playing(5));
        assert_eq!(
            tracker.update(&seeking(120)),
            PositionUpdate::Jump(Duration::from_secs(120))
        );
        // Тот же target в следующих кадрах seek не будит ленту снова.
        assert_eq!(tracker.update(&seeking(120)), PositionUpdate::Unchanged);
        // Scrub двигает цель: новая цель - новый прыжок.
        assert_eq!(
            tracker.update(&seeking(150)),
            PositionUpdate::Jump(Duration::from_secs(150))
        );
        // Seek завершён, позиция осела на цели: ещё один прыжок, дальше обычный ход.
        assert_eq!(
            tracker.update(&playing(150)),
            PositionUpdate::Jump(Duration::from_secs(150))
        );
        assert_eq!(
            tracker.update(&playing(151)),
            PositionUpdate::Advance(Duration::from_secs(151))
        );
    }

    #[test]
    fn backward_move_and_long_forward_gap_are_jumps() {
        let mut tracker = StreamPositionTracker::default();
        tracker.update(&playing(100));
        assert_eq!(
            tracker.update(&playing(40)),
            PositionUpdate::Jump(Duration::from_secs(40))
        );
        assert_eq!(
            tracker.update(&playing(41)),
            PositionUpdate::Advance(Duration::from_secs(41))
        );
        assert_eq!(
            tracker.update(&playing(41 + 11)),
            PositionUpdate::Jump(Duration::from_secs(52))
        );
    }

    #[test]
    fn snapshot_observation_uses_the_seek_target_and_the_live_mode() {
        let mut snapshot = PlayerSnapshot::default();
        snapshot.timeline.current_position =
            media_core::MediaTime::from_duration(Duration::from_secs(5));
        snapshot.current_position = Duration::from_secs(5);
        let calm = StreamPlaybackObservation::from_snapshot(&snapshot);
        assert_eq!(calm.position, Duration::from_secs(5));
        assert_eq!(calm.seek_target, None);
        assert_eq!(calm.timeline, StreamTimelineKind::Vod);

        snapshot.timeline.mode = TimelineMode::Live;
        snapshot.timeline.seeking = true;
        snapshot.timeline.target_position = Some(media_core::MediaTime::from_duration(
            Duration::from_secs(90),
        ));
        let seeking = StreamPlaybackObservation::from_snapshot(&snapshot);
        assert_eq!(seeking.position, Duration::from_secs(90));
        assert_eq!(seeking.seek_target, Some(Duration::from_secs(90)));
        assert_eq!(seeking.timeline, StreamTimelineKind::Live);

        // Цель без идущего seek/scrub - не seek (цель остаётся в снимке после завершения).
        snapshot.timeline.seeking = false;
        let settled = StreamPlaybackObservation::from_snapshot(&snapshot);
        assert_eq!(settled.seek_target, None);
        assert_eq!(settled.position, Duration::from_secs(5));
    }
}
