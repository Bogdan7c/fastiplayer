//! Замена audio output-а при смене формата decoded PCM посреди потока.
//!
//! Решение (что делать, когда менять, как считать хвост) принимает session
//! (`session/audio_spec_change.rs`). Pipeline владеет слотами output/clock/mapping и
//! делает только атомарную перестановку: старый output уходит доигрывать хвост внутрь
//! [`OutputWithRetiringTail`], новый становится активным, clock и media mapping
//! привязываются к точке стыка, которую передала session.

use std::time::Duration;

use super::overlapped_audio_output::OutputWithRetiringTail;
use super::{CapturedAudioClockMapping, PlaybackPipeline};
use crate::{PlaybackRate, PlayerAudioOutput};

/// Запущен ли новый output до установки в слот.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplacementOutputPlayback {
    /// Старый output играл — новый уже запущен session-ом.
    Started,

    /// Старый output стоял (пауза, buffering, ещё не запущен) — новый тоже ждёт `play`.
    NotStarted,
}

/// Где на осях нового clock-а и media начинается output нового формата.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpecChangeClockAnchor {
    /// Позиция clock-а нового output-а до запуска (обычно ноль).
    pub(crate) output_clock_start: Duration,

    /// Media-время, которое звучало в старом output-е в момент стыка.
    pub(crate) media_position: Duration,

    /// Как продолжается media-время на оси нового clock-а.
    pub(crate) timeline: SpecChangeTimeline,
}

/// Откуда новый clock берёт соответствие output-clock ↔ media-time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SpecChangeTimeline {
    /// Старый output имел clock: его mapping с позиции стыка переносится на новый
    /// clock целиком. Тишина длиной хвоста проходит media-время с теми скоростями,
    /// на которых хвост был произведён (смена rate внутри хвоста не сдвигает звук).
    ContinuePreviousMapping {
        /// Позиция clock-а старого output-а, снятая вместе с хвостом.
        previous_output_clock: Duration,
    },
    /// Clock-а у старого output-а не было (тестовый output без spec-а): хвоста
    /// нет, новый якорь открывается с текущей скоростью.
    StartAtPlaybackRate(PlaybackRate),
}

impl CapturedAudioClockMapping {
    /// Позиция clock-а output-а (слышимая), на которой снят capture.
    #[must_use]
    pub(crate) fn audible_output_clock(self) -> Duration {
        self.output_timing.audible_output_position()
    }

    /// Длительность PCM, который output принял, но ещё не проиграл, на момент capture.
    ///
    /// Берётся из того же чтения clock-а, что и media position: хвост и позиция стыка
    /// согласованы между собой, callback не может вклиниться между двумя чтениями.
    #[must_use]
    pub(crate) fn unplayed_output_tail(self) -> Duration {
        self.output_timing.submitted_output_tail()
    }
}

impl PlaybackPipeline {
    /// Сообщает, что активный output уже запущен (`play` принят и не снят паузой).
    #[must_use]
    pub(crate) fn audio_output_is_playing(&self) -> bool {
        self.audio_output.is_some() && self.audio_output_play_requested
    }

    /// Ставит output нового формата активным, а прежний оставляет доигрывать хвост.
    ///
    /// Вызывающий код уже записал в `replacement` тишину длиной хвоста старого output-а
    /// и, если старый играл, запустил его. Clock-mapping привязывается к стартовой
    /// позиции нового clock-а явно, а не к текущей: между запуском и установкой clock
    /// мог чуть продвинуться, и якорь «по текущей» сдвинул бы звук на это время.
    pub(crate) fn install_audio_output_after_spec_change(
        &mut self,
        replacement: Box<dyn PlayerAudioOutput>,
        replacement_spec: audio_core::AudioOutputSpec,
        anchor: SpecChangeClockAnchor,
        playback: ReplacementOutputPlayback,
    ) {
        // Mapping нового clock-а строится из mapping-а СТАРОГО clock-а до установки:
        // `install_audio_clock` сбрасывает mapping к одному якорю и стёр бы
        // rate-сегменты ещё не проигранного хвоста.
        let mut replacement_mapping = self.audio_clock_media_mapping.clone();
        match anchor.timeline {
            SpecChangeTimeline::ContinuePreviousMapping {
                previous_output_clock,
            } => replacement_mapping
                .carry_onto_replacement_clock(previous_output_clock, anchor.output_clock_start),
            SpecChangeTimeline::StartAtPlaybackRate(playback_rate) => replacement_mapping
                .reset_anchor(
                    anchor.output_clock_start,
                    anchor.media_position,
                    playback_rate,
                ),
        }

        let output: Box<dyn PlayerAudioOutput> = match self.audio_output.take() {
            Some(previous) => Box::new(OutputWithRetiringTail::new(replacement, previous)),
            None => replacement,
        };
        let clock = output.clock();

        self.install_audio_output(output, replacement_spec);
        if playback == ReplacementOutputPlayback::Started {
            self.mark_audio_output_play_request_accepted();
        }
        self.install_audio_clock(clock);
        self.media_clock_base = anchor.media_position;
        self.audio_clock_media_mapping = replacement_mapping;
    }
}
