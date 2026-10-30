//! Звук начинается позже часов: тишина перед первым PCM нового отрезка звука.
//!
//! Дефект (сессия 03e моста). Часы показа на старте ведёт audio output: позиция =
//! `media_clock_base` + сколько output уже проиграл. Первый PCM, записанный в пустой
//! output, поэтому звучит ровно в базе (старт воспроизведения, цель seek-а, позиция
//! смены дорожки). Если данные звука в контейнере начинаются позже базы (видео с 0 с,
//! звук с 0,4 с — отложенная дорожка, вход в живой поток, seek в место, где звук
//! начинается чуть позже цели), звук без выравнивания играет раньше видео ровно на этот
//! разрыв, и так до конца файла. Обрезку «звук раньше базы» делает
//! `audio_playback_bounds.rs`; здесь — зеркальный случай «звук позже базы».
//!
//! Решение как у mpv/ffplay: перед первым PCM отрезка пишется тишина длиной в разрыв,
//! и первый настоящий sample звучит в свой PTS. Тишина идёт тем же путём, что и звук
//! (до tempo processor-а), поэтому при скорости ≠ 1 растягивается вместе с ним.
//! В учёт записанного звука (`note_audio_packet_written`) тишина не входит: конец
//! звука по-прежнему меряется по PTS packet-а.
//!
//! Ветви политики (решения владельца, сессия 03e):
//! - разрыв до [`LeadingSilencePolicy::SEGMENT_START`] `on_time_tolerance` (20 мс) —
//!   шум таймстемпов и priming кодека, звук пишется как есть;
//! - разрыв до `max_silence` (1 с) — тишина;
//! - разрыв больше — сюда звук обычно не доходит: он ждёт своего PTS в очереди
//!   (`session/audio_gap/scheduled_audio_start.rs`) и входит, когда часы дошли до него.
//!   Дошёл всё-таки (live-timeline, где удержание не действует) — прежнее поведение
//!   (звук в базу) явной ветвью с логом;
//! - audio-only (видео не выбрано) — тишина не пишется: смотреть нечего, поэтому
//!   воспроизведение начинается с первого sample-а, а часы перепривязываются к его PTS,
//!   чтобы позиция на шкале совпадала со временем звука (как mpv/ffplay). Во время seek-а
//!   базу задаёт commit: он коммитит позицию первого sample-а (`seek_completion.rs`);
//! - разрыв неправдоподобен (звук за концом media или дальше часа от базы — битые метки
//!   времени) — прежнее поведение (звук в базу) с предупреждением в лог.
//!
//! Тишина, растянутая скоростью, должна влезть в свободное место вывода
//! (`silence_output_room.rs`): при 0,25× секунда тишины — это 4 с вывода, а ring держит
//! ~2 с. Не влезающий разрыв `audio_gap/scheduled_audio_start.rs` держит до PTS так же,
//! как разрыв больше потолка; дошедший всё-таки (live) — без тишины, с предупреждением,
//! а не фатальной частичной записью.
//!
//! Стартовая политика — только для ПЕРВОГО PCM отрезка (см.
//! `pipeline/audio_segment_start.rs`). Дальше та же единственная точка ведёт дыры посреди
//! отрезка: меньше 500 мс — тишина от курсора отрезка (`mid_stream_gap.rs`, D3), больше —
//! передача часов `audio_gap.rs`.

use std::time::Duration;

use tracing::{debug, info, warn};

use crate::{AudioOutputSpec, PlayerError, PlayerErrorKind, PlayerResult};

use super::super::PlayerSession;
use super::super::audio_gap::AudioStartGapPlausibility;
use super::super::audio_gap::MIN_AUDIO_GAP;
use super::super::audio_packet_window::DecodedPcmPacketClipOutcome;
use super::mid_stream_gap::MidStreamGapPlan;
use super::silence_output_room::SilenceOutputRoom;

/// Наносекунд в секунде (перевод длительности в PCM frames).
const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Пороги выравнивания первого PCM по его PTS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) struct LeadingSilencePolicy {
    /// Разрыв не больше этого считается «вовремя»: тишина не пишется.
    pub(in crate::session) on_time_tolerance: Duration,

    /// Самая длинная тишина, которую можно записать одним куском.
    pub(in crate::session) max_silence: Duration,
}

impl LeadingSilencePolicy {
    /// Политика начала отрезка звука: старт, seek, смена дорожки.
    ///
    /// Допуск 20 мс: меньше одного AAC-кадра (21–23 мс) и заметно ниже порога, на
    /// котором зритель видит рассинхрон губ; ниже него разрыв — это шум таймстемпов
    /// контейнера и priming кодека. Потолок 1 с: тишина пишется в output одним
    /// куском вместе с первым packet-ом, а ring buffer вывода держит 2 с; запись,
    /// не влезшая в свободное место, — фатальная частичная запись.
    pub(in crate::session) const SEGMENT_START: Self = Self {
        on_time_tolerance: Duration::from_millis(20),
        max_silence: Duration::from_millis(1_000),
    };

    /// Политика дыры посреди отрезка (`mid_stream_gap.rs`).
    ///
    /// Допуск тот же 20 мс: пропавший AAC-кадр (21–23 мс) — уже дыра. Потолок — порог
    /// передачи часов разрыва [`MIN_AUDIO_GAP`]: больше него звук ждёт своего PTS
    /// (`audio_gap.rs`), поэтому диапазоны стыкуются без щели и без перекрытия.
    pub(in crate::session) const MID_STREAM: Self = Self {
        on_time_tolerance: Duration::from_millis(20),
        max_silence: MIN_AUDIO_GAP,
    };
}

/// Сколько тишины нужно перед первым PCM, чтобы он прозвучал в свой PTS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum LeadingSilence {
    /// Первый sample звучит вовремя (раньше ожидаемого или в пределах допуска).
    None,

    /// Перед первым sample-ом нужна тишина.
    Pad {
        /// Длина тишины в PCM frames decoded-частоты (до tempo processor-а).
        silence_frames: usize,

        /// Разрыв между ожидаемым стартом и первым sample-ом.
        gap: Duration,
    },

    /// Разрыв больше потолка тишины: одной записью его не закрыть.
    TooLarge {
        /// Разрыв между ожидаемым стартом и первым sample-ом.
        gap: Duration,
    },
}

/// Чистый расчёт тишины перед первым sample-ом.
///
/// `first_sample_time` — media-время первого sample-а, который будет записан;
/// `expected_start` — media-время, в которое этот sample прозвучит без тишины.
/// Длина тишины округляется до ближайшего PCM frame-а.
#[must_use]
pub(in crate::session) fn plan_leading_silence(
    first_sample_time: Duration,
    expected_start: Duration,
    sample_rate: u32,
    policy: LeadingSilencePolicy,
) -> LeadingSilence {
    let gap = first_sample_time.saturating_sub(expected_start);
    if gap <= policy.on_time_tolerance {
        return LeadingSilence::None;
    }
    if gap > policy.max_silence {
        return LeadingSilence::TooLarge { gap };
    }

    let silence_frames = duration_to_nearest_frames(gap, sample_rate);
    if silence_frames == 0 {
        // Только при нулевой частоте: такой spec до записи не доходит, но тишина
        // из нуля frames всё равно ничего не меняет.
        return LeadingSilence::None;
    }
    LeadingSilence::Pad {
        silence_frames,
        gap,
    }
}

/// Что делать с первым PCM packet-а относительно начала отрезка звука.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum AudioSegmentStartPlan {
    /// PCM продолжает уже начатый отрезок вплотную (или курсор неизвестен, live).
    ContinuesSegment,

    /// PCM продолжает отрезок после дыры (`mid_stream_gap.rs`).
    MidStreamGap(MidStreamGapPlan),

    /// Первый PCM отрезка звучит вовремя.
    OnTime,

    /// Есть видео: перед первым PCM пишется тишина.
    PadWithSilence {
        /// Длина тишины в PCM frames decoded-частоты.
        silence_frames: usize,

        /// Разрыв между базой часов и первым sample-ом.
        gap: Duration,
    },

    /// Есть видео, тишина по длине в потолке, но растянутая скоростью не влезает в вывод,
    /// а удержание до PTS здесь не действовало (live-timeline): тишина не пишется.
    SilenceExceedsOutputRoom {
        /// Разрыв между базой часов и первым sample-ом.
        gap: Duration,

        /// Насколько не хватает места.
        room: SilenceOutputRoom,
    },

    /// Есть видео, разрыв больше потолка и правдоподобен, но удержание звука до его PTS
    /// здесь не действует (live-timeline): прежнее поведение — PCM пишется сразу в базу.
    GapAboveSilenceCap {
        /// Разрыв между базой часов и первым sample-ом.
        gap: Duration,
    },

    /// Разрыв неправдоподобен (битые метки времени): PCM пишется в базу, часы не
    /// переносятся — ждать такой звук значило бы не услышать его вовсе.
    ImplausibleGap {
        /// Разрыв между базой часов и первым sample-ом.
        gap: Duration,

        /// Почему разрыв отвергнут.
        plausibility: AudioStartGapPlausibility,
    },

    /// Audio-only, разрыв правдоподобен: тишина не пишется, часы начинают с PTS
    /// первого sample-а.
    StartClockAtFirstSample {
        /// Media-время первого sample-а — новая база часов.
        first_sample_time: Duration,

        /// Разрыв между прежней базой и первым sample-ом.
        gap: Duration,
    },

    /// Audio-only во время seek-а: базу часов задаёт seek commit, поэтому здесь она не
    /// трогается; commit возьмёт позицию первого sample-а (её запоминает pipeline).
    AudioOnlySeekLandsAtFirstSample {
        /// Media-время первого sample-а — будущая позиция commit-а.
        first_sample_time: Duration,

        /// Разрыв между целью seek-а и первым sample-ом.
        gap: Duration,
    },
}

impl PlayerSession {
    /// Выравнивает первый PCM нового отрезка звука по его PTS (см. модуль).
    ///
    /// Вызывается между обрезкой decoded PCM и его записью. Ошибка — это ошибка записи
    /// тишины в output: она фатальна так же, как ошибка записи самого звука.
    pub(in crate::session) fn align_audio_segment_start(
        &mut self,
        packet_pts: Duration,
        packet_clip: DecodedPcmPacketClipOutcome,
        output_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
        let first_sample_time =
            first_retained_sample_time(packet_pts, packet_clip, output_spec.sample_rate);
        let plan = self.plan_audio_segment_start(first_sample_time, output_spec.sample_rate);
        let clock_base = self.pipeline.media_clock_base();
        // Звук раньше базы обрезается до неё: первый прозвучавший sample — не раньше базы.
        // Запоминается только для первого PCM отрезка (иначе no-op в pipeline).
        self.pipeline
            .plan_audio_segment_first_sample(first_sample_time.max(clock_base));
        match plan {
            AudioSegmentStartPlan::ContinuesSegment | AudioSegmentStartPlan::OnTime => Ok(()),
            AudioSegmentStartPlan::MidStreamGap(mid_stream_plan) => {
                self.apply_mid_stream_audio_gap(mid_stream_plan, output_spec)
            }
            AudioSegmentStartPlan::SilenceExceedsOutputRoom { gap, room } => {
                warn!(
                    gap_ms = gap.as_millis(),
                    room = ?room,
                    clock_base_ms = clock_base.as_millis(),
                    "Leading audio silence does not fit the audio output and the audio was \
                     not held until its PTS; it is written at the clock base"
                );
                Ok(())
            }
            AudioSegmentStartPlan::PadWithSilence {
                silence_frames,
                gap,
            } => {
                debug!(
                    gap_ms = gap.as_millis(),
                    silence_frames,
                    clock_base_ms = clock_base.as_millis(),
                    "Audio starts after the clock base; leading silence keeps A/V in sync"
                );
                self.write_leading_silence(silence_frames, output_spec)
            }
            AudioSegmentStartPlan::GapAboveSilenceCap { gap } => {
                info!(
                    gap_ms = gap.as_millis(),
                    max_silence_ms = LeadingSilencePolicy::SEGMENT_START.max_silence.as_millis(),
                    clock_base_ms = clock_base.as_millis(),
                    "Audio starts too far after the clock base and was not held until its PTS \
                     (live timeline); it is written at the clock base ahead of video"
                );
                Ok(())
            }
            AudioSegmentStartPlan::ImplausibleGap { gap, plausibility } => {
                warn!(
                    gap_ms = gap.as_millis(),
                    first_sample_ms = first_sample_time.as_millis(),
                    clock_base_ms = clock_base.as_millis(),
                    plausibility = ?plausibility,
                    has_video = self.pipeline.has_selected_video_track(),
                    "Audio start is implausibly far from the clock base (broken timestamps?); \
                     it is written at the clock base"
                );
                Ok(())
            }
            AudioSegmentStartPlan::StartClockAtFirstSample {
                first_sample_time,
                gap,
            } => {
                // Output ещё пуст в этом отрезке, поэтому перепривязка ставит его
                // текущую позицию ровно на PTS первого sample-а; позиция на шкале
                // сразу показывает, откуда начнётся звук.
                self.pipeline.set_media_clock_base(first_sample_time);
                self.publish_clock_sample(first_sample_time);
                debug!(
                    gap_ms = gap.as_millis(),
                    first_sample_ms = first_sample_time.as_millis(),
                    "Audio-only playback starts at the first audio sample"
                );
                Ok(())
            }
            AudioSegmentStartPlan::AudioOnlySeekLandsAtFirstSample {
                first_sample_time,
                gap,
            } => {
                debug!(
                    gap_ms = gap.as_millis(),
                    first_sample_ms = first_sample_time.as_millis(),
                    "Audio-only seek: audio starts after the target; commit lands at its first sample"
                );
                Ok(())
            }
        }
    }

    /// Решает, как выровнять PCM, первый sample которого звучит в `first_sample_time`.
    #[must_use]
    pub(in crate::session) fn plan_audio_segment_start(
        &self,
        first_sample_time: Duration,
        sample_rate: u32,
    ) -> AudioSegmentStartPlan {
        if !self.pipeline.audio_segment_awaits_first_pcm() {
            return self.plan_mid_stream_audio_gap(first_sample_time, sample_rate);
        }

        // Без тишины первый PCM пустого output-а звучит ровно в базе часов; той же
        // базой `audio_playback_bounds` обрезает звук, начавшийся раньше неё.
        let clock_base = self.pipeline.media_clock_base();
        let leading_silence = plan_leading_silence(
            first_sample_time,
            clock_base,
            sample_rate,
            LeadingSilencePolicy::SEGMENT_START,
        );
        if self.pipeline.has_selected_video_track() {
            return match leading_silence {
                LeadingSilence::None => AudioSegmentStartPlan::OnTime,
                LeadingSilence::Pad {
                    silence_frames,
                    gap,
                } => match self.silence_output_room(gap) {
                    SilenceOutputRoom::Fits => AudioSegmentStartPlan::PadWithSilence {
                        silence_frames,
                        gap,
                    },
                    room @ SilenceOutputRoom::Exceeds { .. } => {
                        AudioSegmentStartPlan::SilenceExceedsOutputRoom { gap, room }
                    }
                },
                LeadingSilence::TooLarge { gap } => {
                    match self.audio_start_gap_plausibility(clock_base, first_sample_time) {
                        AudioStartGapPlausibility::Plausible => {
                            AudioSegmentStartPlan::GapAboveSilenceCap { gap }
                        }
                        plausibility => AudioSegmentStartPlan::ImplausibleGap { gap, plausibility },
                    }
                }
            };
        }

        // Audio-only: тишина не пишется вовсе; перенос часов ограничивает только
        // правдоподобие разрыва (потолок тишины к audio-only не относится).
        let gap = match leading_silence {
            LeadingSilence::None => return AudioSegmentStartPlan::OnTime,
            LeadingSilence::Pad { gap, .. } | LeadingSilence::TooLarge { gap } => gap,
        };
        let plausibility = self.audio_start_gap_plausibility(clock_base, first_sample_time);
        if !plausibility.is_plausible() {
            return AudioSegmentStartPlan::ImplausibleGap { gap, plausibility };
        }
        if self.has_active_seek_commit() {
            return AudioSegmentStartPlan::AudioOnlySeekLandsAtFirstSample {
                first_sample_time,
                gap,
            };
        }
        AudioSegmentStartPlan::StartClockAtFirstSample {
            first_sample_time,
            gap,
        }
    }

    /// Пишет тишину тем же путём, что и decoded PCM (с tempo processor-ом при rate ≠ 1).
    ///
    /// Вызывающий код заранее проверил, что тишина влезает в вывод (`silence_output_room`).
    pub(in crate::session) fn write_leading_silence(
        &mut self,
        silence_frames: usize,
        output_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
        let channel_count = usize::try_from(output_spec.channels())
            .map_err(|_| leading_silence_size_error(silence_frames, output_spec.channels()))?;
        let silence_sample_count = silence_frames
            .checked_mul(channel_count)
            .ok_or_else(|| leading_silence_size_error(silence_frames, output_spec.channels()))?;
        let silence = vec![0.0_f32; silence_sample_count];
        self.write_decoded_audio_samples_at_current_rate(&silence, output_spec)
    }
}

/// Media-время первого sample-а, который останется после exact clip-а packet-а.
///
/// Обрезка по базе часов здесь не учитывается намеренно: она срабатывает только для
/// звука раньше базы, а такой звук и так начинается вовремя (разрыв ноль).
fn first_retained_sample_time(
    packet_pts: Duration,
    packet_clip: DecodedPcmPacketClipOutcome,
    sample_rate: u32,
) -> Duration {
    let clipped_leading_frames = match packet_clip {
        DecodedPcmPacketClipOutcome::Retained(retained_frames) => retained_frames.first_frame(),
        DecodedPcmPacketClipOutcome::Unbounded | DecodedPcmPacketClipOutcome::FullyDropped => 0,
    };
    packet_pts.saturating_add(frames_to_duration(clipped_leading_frames, sample_rate))
}

/// Переводит длительность в ближайшее целое число PCM frames.
fn duration_to_nearest_frames(duration: Duration, sample_rate: u32) -> usize {
    let scaled = duration.as_nanos().saturating_mul(u128::from(sample_rate));
    let frames = scaled.saturating_add(NANOS_PER_SECOND / 2) / NANOS_PER_SECOND;
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// Переводит число PCM frames в длительность (с округлением вниз).
fn frames_to_duration(frames: usize, sample_rate: u32) -> Duration {
    if sample_rate == 0 {
        return Duration::ZERO;
    }
    let nanos = (frames as u128).saturating_mul(NANOS_PER_SECOND) / u128::from(sample_rate);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// Ошибка размера тишины: число samples не помещается в адресное пространство.
fn leading_silence_size_error(silence_frames: usize, channels: u32) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!(
            "Leading audio silence of {silence_frames} frames x {channels} channels does not fit in memory"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Частота, на которой считаем основные границы.
    const RATE_48K: u32 = 48_000;

    /// Политика начала отрезка.
    const POLICY: LeadingSilencePolicy = LeadingSilencePolicy::SEGMENT_START;

    #[test]
    fn gap_within_tolerance_needs_no_silence() {
        let base = Duration::from_secs(5);

        for first_sample in [
            base,
            base + Duration::from_millis(10),
            base + POLICY.on_time_tolerance,
        ] {
            assert_eq!(
                plan_leading_silence(first_sample, base, RATE_48K, POLICY),
                LeadingSilence::None,
                "first sample at {first_sample:?}"
            );
        }
    }

    #[test]
    fn audio_before_the_base_needs_no_silence() {
        // Звук раньше базы обрезается, а не выравнивается тишиной.
        let plan = plan_leading_silence(
            Duration::from_millis(4_900),
            Duration::from_secs(5),
            RATE_48K,
            POLICY,
        );

        assert_eq!(plan, LeadingSilence::None);
    }

    #[test]
    fn gap_just_above_tolerance_and_up_to_cap_is_padded_exactly() {
        let base = Duration::from_secs(2);

        let just_above =
            plan_leading_silence(base + Duration::from_millis(21), base, RATE_48K, POLICY);
        let typical =
            plan_leading_silence(base + Duration::from_millis(400), base, RATE_48K, POLICY);
        let at_cap = plan_leading_silence(base + POLICY.max_silence, base, RATE_48K, POLICY);

        assert_eq!(
            just_above,
            LeadingSilence::Pad {
                silence_frames: 1_008,
                gap: Duration::from_millis(21),
            }
        );
        assert_eq!(
            typical,
            LeadingSilence::Pad {
                silence_frames: 19_200,
                gap: Duration::from_millis(400),
            }
        );
        assert_eq!(
            at_cap,
            LeadingSilence::Pad {
                silence_frames: 48_000,
                gap: POLICY.max_silence,
            }
        );
    }

    #[test]
    fn gap_above_cap_is_too_large_for_silence() {
        let base = Duration::from_secs(1);
        let gap = POLICY.max_silence + Duration::from_millis(1);

        let plan = plan_leading_silence(base + gap, base, RATE_48K, POLICY);

        assert_eq!(plan, LeadingSilence::TooLarge { gap });
    }

    #[test]
    fn silence_length_rounds_to_the_nearest_frame_at_any_sample_rate() {
        // 30,02 мс при 44,1 кГц = 1323,88 frame-а: ближайший — 1324, а не 1323.
        let rounded_up = plan_leading_silence(
            Duration::from_micros(30_020),
            Duration::ZERO,
            44_100,
            POLICY,
        );
        // 30,01 мс при 44,1 кГц = 1323,44 frame-а: ближайший — 1323.
        let rounded_down = plan_leading_silence(
            Duration::from_micros(30_010),
            Duration::ZERO,
            44_100,
            POLICY,
        );
        // 0,4 с при 22,05 кГц — ровно 8820 frames.
        let exact =
            plan_leading_silence(Duration::from_millis(400), Duration::ZERO, 22_050, POLICY);

        assert!(matches!(
            rounded_up,
            LeadingSilence::Pad {
                silence_frames: 1_324,
                ..
            }
        ));
        assert!(matches!(
            rounded_down,
            LeadingSilence::Pad {
                silence_frames: 1_323,
                ..
            }
        ));
        assert!(matches!(
            exact,
            LeadingSilence::Pad {
                silence_frames: 8_820,
                ..
            }
        ));
    }

    #[test]
    fn zero_sample_rate_never_writes_silence() {
        let plan = plan_leading_silence(Duration::from_millis(400), Duration::ZERO, 0, POLICY);

        assert_eq!(plan, LeadingSilence::None);
    }

    #[test]
    fn mid_stream_policy_pads_from_just_above_20_ms_up_to_the_gap_threshold() {
        let expected_next = Duration::from_secs(15);
        let mid_stream = LeadingSilencePolicy::MID_STREAM;
        let plan = |gap: Duration| {
            plan_leading_silence(expected_next + gap, expected_next, RATE_48K, mid_stream)
        };

        assert_eq!(plan(Duration::from_millis(20)), LeadingSilence::None);
        assert_eq!(
            plan(Duration::from_millis(21)),
            LeadingSilence::Pad {
                silence_frames: 1_008,
                gap: Duration::from_millis(21),
            }
        );
        // Один пропавший AAC-кадр (1024 sample при 48 кГц = 21,33 мс) — уже дыра.
        assert!(matches!(
            plan(Duration::from_nanos(21_333_333)),
            LeadingSilence::Pad {
                silence_frames: 1_024,
                ..
            }
        ));
        assert_eq!(
            plan(MIN_AUDIO_GAP),
            LeadingSilence::Pad {
                silence_frames: 24_000,
                gap: MIN_AUDIO_GAP,
            }
        );
        // Больше порога — дело передачи часов разрыва (`audio_gap.rs`), не тишины.
        assert_eq!(
            plan(MIN_AUDIO_GAP + Duration::from_millis(1)),
            LeadingSilence::TooLarge {
                gap: MIN_AUDIO_GAP + Duration::from_millis(1),
            }
        );
    }

    #[test]
    fn custom_policy_moves_both_thresholds() {
        // Будущая политика (например, мелкие разрывы посреди отрезка) задаёт свои пороги.
        let mid_stream = LeadingSilencePolicy {
            on_time_tolerance: Duration::from_millis(5),
            max_silence: Duration::from_millis(500),
        };

        assert!(matches!(
            plan_leading_silence(
                Duration::from_millis(10),
                Duration::ZERO,
                RATE_48K,
                mid_stream
            ),
            LeadingSilence::Pad {
                silence_frames: 480,
                ..
            }
        ));
        assert_eq!(
            plan_leading_silence(
                Duration::from_millis(600),
                Duration::ZERO,
                RATE_48K,
                mid_stream
            ),
            LeadingSilence::TooLarge {
                gap: Duration::from_millis(600)
            }
        );
    }

    #[test]
    fn first_sample_time_includes_exact_packet_clip() {
        use super::super::super::audio_packet_window::DecodedPcmFrameRange;

        // Окно packet-а отрезало первые 4 frame-а при 4 Гц: звук начинается на 1 с позже PTS.
        let clipped = first_retained_sample_time(
            Duration::from_secs(2),
            DecodedPcmPacketClipOutcome::Retained(DecodedPcmFrameRange::ordered_or_empty(4, 8)),
            4,
        );
        let unbounded = first_retained_sample_time(
            Duration::from_secs(2),
            DecodedPcmPacketClipOutcome::Unbounded,
            4,
        );

        assert_eq!(clipped, Duration::from_secs(3));
        assert_eq!(unbounded, Duration::from_secs(2));
    }
}
