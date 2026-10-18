//! Вывод «часов показа» из того, что UI знает про плеер и показанные кадры.
//!
//! # Откуда берутся часы
//! - Режим (игра / пауза / seek / scrub), скорость и оценка длительности кадра приходят из
//!   снимка плеера ([`PlaybackObservation`]).
//! - Шаг между кадрами во времени субтитров — НАБЛЮДАЕМАЯ разница pts двух соседних РАЗНЫХ
//!   отправленных кадров ([`ClockTracker::note_frame`]). На 2× и при пропуске кадров он растёт
//!   сам. Нереальная разница (назад, скачок после seek) не учитывается; пока шага нет,
//!   берётся оценка из снимка, а без неё горизонт не используется.
//! - Цели seek/scrub переводятся во время поиска реплик тем же сдвигом синхронизации, что и
//!   pts кадра ([`super::super::active_track::ActiveSubtitleTrack::lookup_time_for`]).
//!
//! Модуль чистый: потоков и растеризатора не знает.

use std::time::Duration;

use player_core::{PlaybackState, PlayerSnapshot, SubtitleReadRun};
use subtitle_core::SubtitleTime;

use super::schedule::{FrameCadence, FrameStep, PlaybackRate, PresentationClock, PresentationMode};
use crate::state::subtitles::active_track::ActiveSubtitleTrack;

/// Разница pts дольше этого — не шаг кадров, а скачок (seek, разрыв): в шаг не берётся.
const MAX_PLAUSIBLE_FRAME_STEP: Duration = Duration::from_secs(1);

/// Тысячных долей в единице скорости.
const THOUSANDTHS_PER_UNIT_RATE: f32 = 1_000.0;

/// Режим воспроизведения по снимку плеера; цели — позиции media (до сдвига синхронизации).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum ObservedMode {
    /// Идёт воспроизведение.
    Playing,
    /// Пауза (кадр неподвижен).
    Paused,
    /// Идёт seek к `target`.
    Seeking { target: Duration },
    /// Идёт перетаскивание по таймлайну; `target` — последняя цель.
    Scrubbing { target: Duration },
}

impl ObservedMode {
    /// Режим, где важна цель, а не показанный кадр.
    pub(in crate::state::subtitles) fn has_target(self) -> bool {
        matches!(self, Self::Seeking { .. } | Self::Scrubbing { .. })
    }
}

/// Наблюдение за плеером на этом кадре UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct PlaybackObservation {
    /// Режим.
    pub(in crate::state::subtitles) mode: ObservedMode,
    /// Скорость воспроизведения.
    pub(in crate::state::subtitles) rate: PlaybackRate,
    /// Оценка длительности кадра из снимка плеера (запасной шаг).
    pub(in crate::state::subtitles) estimated_step: Option<FrameStep>,
    /// Позиция media (шкала pts демуксера): цель в seek/scrub, иначе текущая позиция плеера;
    /// `None` — позиция неизвестна. Часов показа она не касается: по ней картинные дорожки
    /// (PGS) решают, что вытеснить из памяти, и она есть даже когда ни одна дорожка не
    /// показывается.
    pub(in crate::state::subtitles) position: Option<Duration>,
    /// Проход чтения демуксера для пакетов субтитров из снимка плеера. Его смена — разрыв
    /// потока (seek/scrub/потеря пакета): картинные дорожки (PGS) сбрасываются, не дожидаясь
    /// пакетов. Часов показа не касается.
    pub(in crate::state::subtitles) read_run: SubtitleReadRun,
}

impl Default for PlaybackObservation {
    /// Обычное воспроизведение без оценок (так ведут себя тесты, не знающие про плеер).
    fn default() -> Self {
        Self {
            mode: ObservedMode::Playing,
            rate: PlaybackRate::NORMAL,
            estimated_step: None,
            position: None,
            read_run: SubtitleReadRun::INITIAL,
        }
    }
}

impl PlaybackObservation {
    /// Собирает наблюдение из снимка плеера.
    pub(in crate::state::subtitles) fn from_snapshot(snapshot: &PlayerSnapshot) -> Self {
        let timeline = &snapshot.timeline;
        let target = timeline.target_position.map(|target| target.as_duration());
        let mode = match (target, timeline.scrubbing, timeline.seeking) {
            (Some(target), true, _) => ObservedMode::Scrubbing { target },
            (Some(target), false, true) => ObservedMode::Seeking { target },
            _ => match snapshot.playback_state {
                PlaybackState::Paused | PlaybackState::Scrubbing => ObservedMode::Paused,
                _ => ObservedMode::Playing,
            },
        };
        let rate_value = snapshot.playback_rate.as_f32();
        let rate = if rate_value.is_finite() {
            // Скорость ограничена 0.25..=4.0, насыщение `as` недостижимо.
            PlaybackRate::from_thousandths((rate_value * THOUSANDTHS_PER_UNIT_RATE).round() as i32)
        } else {
            PlaybackRate::NORMAL
        };
        let estimated_micros =
            i64::try_from(snapshot.video_frame_duration_estimate.as_micros()).unwrap_or(i64::MAX);
        Self {
            mode,
            rate,
            estimated_step: FrameStep::from_micros(estimated_micros),
            position: Some(target.unwrap_or(snapshot.current_position)),
            read_run: snapshot.subtitle_read_run,
        }
    }
}

/// Наблюдатель за шагом кадров.
#[derive(Debug, Default)]
pub(in crate::state::subtitles) struct ClockTracker {
    last_pts: Option<Duration>,
    observed_step: Option<FrameStep>,
}

impl ClockTracker {
    /// Учитывает кадр, уходящий в renderer. Тот же pts (повторный показ) шаг не меняет.
    pub(in crate::state::subtitles) fn note_frame(&mut self, frame_pts: Duration) {
        let previous = self.last_pts.replace(frame_pts);
        let Some(previous) = previous.filter(|&previous| previous != frame_pts) else {
            return;
        };
        let Some(delta) = frame_pts
            .checked_sub(previous)
            .filter(|delta| *delta <= MAX_PLAUSIBLE_FRAME_STEP)
        else {
            return;
        };
        let micros = i64::try_from(delta.as_micros()).unwrap_or(i64::MAX);
        if let Some(step) = FrameStep::from_micros(micros) {
            self.observed_step = Some(step);
        }
    }

    /// Разрыв потока кадров (seek, scrub, новый проход чтения): следующий кадр не сравнивается
    /// с предыдущим. Иначе разница pts «до» и «после» разрыва (до секунды, это ещё «правдоподобно»)
    /// стала бы шагом кадров и раздула сроки годности анимированных картинок. Уже наблюдённый
    /// шаг остаётся: частота кадров media при разрыве не меняется.
    pub(in crate::state::subtitles) fn note_discontinuity(&mut self) {
        self.last_pts = None;
    }

    /// Только тесты: известен ли наблюдаемый шаг.
    #[cfg(test)]
    pub(in crate::state::subtitles) fn has_observed_step(&self) -> bool {
        self.observed_step.is_some()
    }

    /// Шаг для часов: наблюдаемый, иначе оценка плеера, иначе неизвестен.
    fn cadence(&self, estimated: Option<FrameStep>) -> FrameCadence {
        self.observed_step
            .or(estimated)
            .map_or(FrameCadence::Unknown, FrameCadence::Known)
    }

    /// Собирает часы показа для кадра со временем поиска `current`.
    pub(in crate::state::subtitles) fn clock_for(
        &self,
        current: SubtitleTime,
        observation: &PlaybackObservation,
        track: &ActiveSubtitleTrack,
    ) -> PresentationClock {
        let mode = match observation.mode {
            ObservedMode::Playing => PresentationMode::Playing,
            ObservedMode::Paused => PresentationMode::Paused,
            ObservedMode::Seeking { target } => PresentationMode::Seeking {
                target: track.lookup_time_for(target),
            },
            ObservedMode::Scrubbing { target } => PresentationMode::Scrubbing {
                target: track.lookup_time_for(target),
            },
        };
        PresentationClock {
            current,
            cadence: self.cadence(observation.estimated_step),
            rate: observation.rate,
            mode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn millis(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn step_is_the_pts_delta_of_consecutive_different_frames() {
        let mut tracker = ClockTracker::default();
        tracker.note_frame(millis(1_000));
        tracker.note_frame(millis(1_000));
        assert_eq!(
            tracker.observed_step, None,
            "a repeated frame is not a step"
        );
        tracker.note_frame(millis(1_040));
        tracker.note_frame(millis(1_040));
        assert_eq!(tracker.observed_step, FrameStep::from_micros(40_000));
        tracker.note_frame(millis(1_120));
        assert_eq!(
            tracker.observed_step,
            FrameStep::from_micros(80_000),
            "at 2x or with dropped frames the step grows by itself"
        );
    }

    #[test]
    fn jumps_and_backward_moves_do_not_become_a_step() {
        let mut tracker = ClockTracker::default();
        tracker.note_frame(millis(1_000));
        tracker.note_frame(millis(1_040));
        tracker.note_frame(millis(90_000));
        tracker.note_frame(millis(500));
        assert_eq!(tracker.observed_step, FrameStep::from_micros(40_000));
    }

    #[test]
    fn a_discontinuity_keeps_the_known_step_and_does_not_learn_a_new_one() {
        let mut tracker = ClockTracker::default();
        tracker.note_frame(millis(1_000));
        tracker.note_frame(millis(1_040));
        tracker.note_discontinuity();
        // Скачок в пределах «правдоподобного» шага после разрыва шагом не становится.
        tracker.note_frame(millis(1_700));
        assert_eq!(tracker.observed_step, FrameStep::from_micros(40_000));
        tracker.note_frame(millis(1_740));
        assert_eq!(tracker.observed_step, FrameStep::from_micros(40_000));
    }

    #[test]
    fn cadence_falls_back_to_the_player_estimate_and_then_to_unknown() {
        let tracker = ClockTracker::default();
        let estimate = FrameStep::from_micros(33_000);
        assert_eq!(
            tracker.cadence(estimate),
            FrameCadence::Known(estimate.expect("positive step"))
        );
        assert_eq!(tracker.cadence(None), FrameCadence::Unknown);
    }

    #[test]
    fn snapshot_modes_map_to_observed_modes() {
        let mut snapshot = PlayerSnapshot::empty();
        snapshot.playback_state = PlaybackState::Paused;
        assert_eq!(
            PlaybackObservation::from_snapshot(&snapshot).mode,
            ObservedMode::Paused
        );
        snapshot.playback_state = PlaybackState::Playing;
        assert_eq!(
            PlaybackObservation::from_snapshot(&snapshot).mode,
            ObservedMode::Playing
        );
        snapshot.timeline.scrubbing = true;
        snapshot.timeline.target_position =
            Some(media_core::MediaTime::from_duration(millis(7_000)));
        assert_eq!(
            PlaybackObservation::from_snapshot(&snapshot).mode,
            ObservedMode::Scrubbing {
                target: millis(7_000)
            }
        );
        snapshot.timeline.scrubbing = false;
        snapshot.timeline.seeking = true;
        assert_eq!(
            PlaybackObservation::from_snapshot(&snapshot).mode,
            ObservedMode::Seeking {
                target: millis(7_000)
            }
        );
    }

    #[test]
    fn observation_carries_the_subtitle_read_run_of_the_snapshot() {
        let mut snapshot = PlayerSnapshot::empty();
        assert_eq!(
            PlaybackObservation::from_snapshot(&snapshot).read_run,
            SubtitleReadRun::INITIAL
        );
        snapshot.subtitle_read_run = SubtitleReadRun::INITIAL.next().next();
        assert_eq!(
            PlaybackObservation::from_snapshot(&snapshot).read_run.get(),
            2
        );
    }
}
