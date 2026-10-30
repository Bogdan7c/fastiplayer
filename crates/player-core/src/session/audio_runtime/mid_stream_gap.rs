//! Дыра в звуке посреди отрезка короче порога разрыва: тишина вместо склейки.
//!
//! Дефект D3 (живая приёмка 03e). Дорожка звука с дырой 321–340 мс посреди файла:
//! packet после дыры писался в output сразу за packet-ом до неё — звук «склеивался» и
//! дальше опережал видео на длину дыры до следующей перемотки. Разрывы от
//! [`MIN_AUDIO_GAP`] (500 мс) ведёт передача часов (`session/audio_gap.rs`): output
//! доигрывает звук, время ведут monotonic-часы, звук после дыры ждёт своего PTS. Дыры
//! короче туда не попадают (это могут быть и швы кодека), поэтому их закрывает тишина
//! той же длины — как у mpv/ffplay.
//!
//! Где измеряется дыра. Следующий sample звучит в курсоре отрезка
//! (`pipeline::expected_next_audio_sample_time`: конец записанного звука по PTS и числу
//! декодированных frames). Если первый sample нового packet-а позже курсора больше
//! допуска, перед ним пишется тишина длиной в разницу — тем же путём, что тишина начала
//! отрезка (`leading_silence.rs`): до tempo processor-а, поэтому при скорости ≠ 1 она
//! растягивается вместе со звуком. В курсор и в `written_audio_end` тишина не входит.
//!
//! Ветви (решения владельца, D3):
//! - разрыв до 20 мс ([`LeadingSilencePolicy::MID_STREAM`]) — шум меток времени, звук
//!   пишется как есть; настоящий пропавший AAC-кадр (21–23 мс) — уже дыра;
//! - разрыв до [`MIN_AUDIO_GAP`] — тишина; больше — packet обычно удержан передачей
//!   часов (`audio_gap.rs` меряет от того же курсора), а если дошёл (audio-only, пауза,
//!   seek preroll — там передачи нет) — прежнее поведение с логом;
//! - тишина, растянутая скоростью, не влезает в свободное место вывода
//!   (`silence_output_room.rs`) — она не пишется: в обычной игре A/V `audio_gap.rs`
//!   держит такой packet до освобождения места или до его PTS, сюда он доходит только
//!   там, где удержания нет, и тогда — прежнее поведение с предупреждением;
//! - перекрытие (packet раньше курсора) — без изменений: обрезки нет;
//! - live-timeline — без изменений (разрывы времени там ведёт своя логика);
//! - audio-only — тишина тоже пишется: позиция на шкале (её ведёт звук) остаётся честной.
//!
//! Ошибка декодирования packet-а курсор не двигает, поэтому следующий packet получает
//! тишину длиной в пропавший кадр — звук не уезжает раньше картинки.

use std::time::Duration;

use media_core::TimelineMode;
use tracing::{debug, info, warn};

use crate::{AudioOutputSpec, PlayerResult};

use super::super::PlayerSession;
use super::super::audio_gap::MIN_AUDIO_GAP;
use super::leading_silence::{
    AudioSegmentStartPlan, LeadingSilence, LeadingSilencePolicy, plan_leading_silence,
};
use super::silence_output_room::SilenceOutputRoom;

/// Чем можно закрыть разрыв между курсором отрезка и следующим звуком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum MidStreamGapClosure {
    /// Разрыва нет: звук вплотную, в пределах допуска или перекрывается с прежним.
    NoGap,

    /// Разрыв закрывается тишиной.
    Silence {
        /// Длина разрыва.
        gap: Duration,
    },

    /// Разрыв не меньше порога передачи часов: тишиной не закрывается.
    AboveSilenceCap {
        /// Длина разрыва.
        gap: Duration,
    },

    /// Тишина, растянутая скоростью, не влезает в свободное место вывода.
    ExceedsOutputRoom {
        /// Длина разрыва.
        gap: Duration,

        /// Насколько не хватает места.
        room: SilenceOutputRoom,
    },
}

impl MidStreamGapClosure {
    /// `true`, если тишиной разрыв сейчас не закрыть и звук должен ждать своего PTS.
    #[must_use]
    pub(in crate::session) const fn needs_hold(self) -> bool {
        matches!(
            self,
            Self::AboveSilenceCap { .. } | Self::ExceedsOutputRoom { .. }
        )
    }
}

/// Разрыв перед ещё не декодированным packet-ом, который тишиной сейчас не закрыть.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) struct PendingAudioGapAhead {
    /// Курсор отрезка: где прозвучал бы следующий sample.
    pub(in crate::session) expected_next: Duration,

    /// PTS front pending audio packet-а.
    pub(in crate::session) next_audio_start: Duration,

    /// Почему тишина не подходит (`AboveSilenceCap` или `ExceedsOutputRoom`).
    pub(in crate::session) closure: MidStreamGapClosure,
}

/// Что делать с PCM packet-а, начинающимся позже курсора отрезка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum MidStreamGapPlan {
    /// Перед PCM пишется тишина.
    Pad {
        /// Курсор отрезка: где прозвучал бы PCM без тишины.
        expected_next: Duration,

        /// Media-время первого sample-а packet-а.
        first_sample: Duration,

        /// Длина тишины в PCM frames decoded-частоты.
        silence_frames: usize,

        /// Длина разрыва.
        gap: Duration,
    },

    /// Разрыв не меньше порога передачи часов, а packet до decode всё же дошёл (там,
    /// где передачи часов нет): прежнее поведение — PCM пишется следом.
    AboveSilenceCap {
        /// Курсор отрезка.
        expected_next: Duration,

        /// Media-время первого sample-а packet-а.
        first_sample: Duration,

        /// Длина разрыва.
        gap: Duration,
    },

    /// Тишина не влезает в вывод, а удержание здесь не действовало: тишина не пишется
    /// (частичная запись была бы фатальной), PCM пишется следом.
    ExceedsOutputRoom {
        /// Курсор отрезка.
        expected_next: Duration,

        /// Media-время первого sample-а packet-а.
        first_sample: Duration,

        /// Длина разрыва.
        gap: Duration,

        /// Насколько не хватает места.
        room: SilenceOutputRoom,
    },
}

impl PlayerSession {
    /// Классифицирует разрыв между курсором отрезка и звуком, начинающимся в `next_audio`.
    ///
    /// Общая точка для удержания до decode (`audio_gap.rs`) и для записи тишины после
    /// decode: обе стороны меряют разрыв от одного курсора и одними порогами, поэтому
    /// диапазоны «тишина» и «передача часов» стыкуются ровно на [`MIN_AUDIO_GAP`].
    #[must_use]
    pub(in crate::session) fn classify_mid_stream_gap(
        &self,
        expected_next: Duration,
        next_audio: Duration,
    ) -> MidStreamGapClosure {
        let policy = LeadingSilencePolicy::MID_STREAM;
        let gap = next_audio.saturating_sub(expected_next);
        if gap <= policy.on_time_tolerance {
            return MidStreamGapClosure::NoGap;
        }
        if gap > policy.max_silence {
            return MidStreamGapClosure::AboveSilenceCap { gap };
        }
        match self.silence_output_room(gap) {
            SilenceOutputRoom::Fits => MidStreamGapClosure::Silence { gap },
            room @ SilenceOutputRoom::Exceeds { .. } => {
                MidStreamGapClosure::ExceedsOutputRoom { gap, room }
            }
        }
    }

    /// Разрыв перед front pending audio packet-ом, который тишиной сейчас не закрыть.
    ///
    /// `None` — курсора нет (отрезок ждёт первый PCM), очередь пуста или разрыв
    /// закрывается тишиной / его нет.
    #[must_use]
    pub(in crate::session) fn pending_audio_gap_too_large_for_silence(
        &self,
    ) -> Option<PendingAudioGapAhead> {
        let expected_next = self.pipeline.expected_next_audio_sample_time()?;
        let next_audio_start = self.pipeline.front_pending_audio_packet_start()?;
        let closure = self.classify_mid_stream_gap(expected_next, next_audio_start);
        closure.needs_hold().then_some(PendingAudioGapAhead {
            expected_next,
            next_audio_start,
            closure,
        })
    }

    /// План для PCM, продолжающего уже начатый отрезок (первый sample — `first_sample`).
    #[must_use]
    pub(in crate::session) fn plan_mid_stream_audio_gap(
        &self,
        first_sample: Duration,
        sample_rate: u32,
    ) -> AudioSegmentStartPlan {
        // Live: разрывы времени эфира ведёт своя логика (как у `audio_gap.rs`).
        if self.snapshot.timeline.mode != TimelineMode::Static {
            return AudioSegmentStartPlan::ContinuesSegment;
        }
        let Some(expected_next) = self.pipeline.expected_next_audio_sample_time() else {
            return AudioSegmentStartPlan::ContinuesSegment;
        };
        let plan = match self.classify_mid_stream_gap(expected_next, first_sample) {
            MidStreamGapClosure::NoGap => return AudioSegmentStartPlan::ContinuesSegment,
            MidStreamGapClosure::Silence { .. } => match plan_leading_silence(
                first_sample,
                expected_next,
                sample_rate,
                LeadingSilencePolicy::MID_STREAM,
            ) {
                LeadingSilence::Pad {
                    silence_frames,
                    gap,
                } => MidStreamGapPlan::Pad {
                    expected_next,
                    first_sample,
                    silence_frames,
                    gap,
                },
                // Те же пороги, что у классификации: сюда приходит только нулевая
                // частота (тишина из нуля frames ничего не меняет).
                LeadingSilence::None | LeadingSilence::TooLarge { .. } => {
                    return AudioSegmentStartPlan::ContinuesSegment;
                }
            },
            MidStreamGapClosure::AboveSilenceCap { gap } => MidStreamGapPlan::AboveSilenceCap {
                expected_next,
                first_sample,
                gap,
            },
            MidStreamGapClosure::ExceedsOutputRoom { gap, room } => {
                MidStreamGapPlan::ExceedsOutputRoom {
                    expected_next,
                    first_sample,
                    gap,
                    room,
                }
            }
        };
        AudioSegmentStartPlan::MidStreamGap(plan)
    }

    /// Выполняет план дыры посреди отрезка: пишет тишину или логирует прежнее поведение.
    ///
    /// Ошибка — ошибка записи тишины в output: фатальна так же, как запись самого звука.
    pub(in crate::session) fn apply_mid_stream_audio_gap(
        &mut self,
        plan: MidStreamGapPlan,
        output_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
        match plan {
            MidStreamGapPlan::Pad {
                expected_next,
                first_sample,
                silence_frames,
                gap,
            } => {
                debug!(
                    gap_ms = gap.as_millis(),
                    silence_frames,
                    expected_next_ms = expected_next.as_millis(),
                    first_sample_ms = first_sample.as_millis(),
                    "Audio hole mid-stream; silence keeps A/V in sync"
                );
                self.write_leading_silence(silence_frames, output_spec)
            }
            MidStreamGapPlan::AboveSilenceCap {
                expected_next,
                first_sample,
                gap,
            } => {
                info!(
                    gap_ms = gap.as_millis(),
                    min_audio_gap_ms = MIN_AUDIO_GAP.as_millis(),
                    expected_next_ms = expected_next.as_millis(),
                    first_sample_ms = first_sample.as_millis(),
                    "Audio hole mid-stream is above the silence cap and was not held \
                     (no clock handover here); audio continues right after the previous one"
                );
                Ok(())
            }
            MidStreamGapPlan::ExceedsOutputRoom {
                expected_next,
                first_sample,
                gap,
                room,
            } => {
                warn!(
                    gap_ms = gap.as_millis(),
                    room = ?room,
                    expected_next_ms = expected_next.as_millis(),
                    first_sample_ms = first_sample.as_millis(),
                    "Silence for a mid-stream audio hole does not fit the audio output and the \
                     audio was not held; audio continues right after the previous one"
                );
                Ok(())
            }
        }
    }
}
