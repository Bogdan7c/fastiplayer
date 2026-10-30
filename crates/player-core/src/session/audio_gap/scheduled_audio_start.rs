//! Звук отрезка начинается позже базы часов больше потолка тишины: звук ждёт своего PTS.
//!
//! Дефект (сессия 03e моста). Первый PCM пустого output-а звучит в базе часов (старт,
//! цель seek-а). Разрыв до 1 с закрывает тишина (`audio_runtime/leading_silence.rs`), но
//! больше потолка тишину не записать: ring вывода держит ~2 с, а запись больше свободного
//! места — фатальная частичная запись. Раньше такой звук писался прямо в базу и играл
//! раньше видео на весь разрыв (живой стенд: +3 с у mp4/mkv со звуком с 3 с; поток моста
//! со звуком на 40 с при часах на 0 играл звук в 0 и вставал).
//!
//! Решение — тот же путь, что у разрыва посреди файла (`audio_gap.rs`): видео идёт по
//! monotonic clock с базы, звук ждёт в очереди сжатым до своего PTS, затем забирает часы
//! с текущей позиции показа. Здесь — только то, чем начало отрезка отличается:
//! - **доказательство** — первый pending packet отрезка лежит дальше базы больше потолка
//!   тишины ([`SegmentStartAudioSchedule::ScheduledLater`]). Packet уже прочитан, поэтому
//!   это факт контейнера, а не догадка по таймеру;
//! - **удержание** действует в любом состоянии (preroll старта, seek preroll, игра): иначе
//!   decoder создал бы output и записал звук в базу ещё до старта;
//! - **ворота старта и seek-а** не ждут звук, который не может прийти раньше своего PTS:
//!   старт идёт видео первым (видео здесь — более ранний поток);
//! - **вход звука без output-а** (старт: output ленивый и ещё не создан) — база часов
//!   переносится на текущую позицию показа, и output, созданный первым PCM, ставит свои
//!   часы ровно туда ([`PlayerSession::join_scheduled_audio_start_if_due`]). Если output
//!   уже есть (seek посреди игры), работает обычная передача часов разрыва: доказательство
//!   разрыва собирает `audio_gap.rs`, возврат — `resume_audio_after_gap_if_due`.
//!
//! Граница правдоподобия ([`classify_audio_start_gap`]): звук, начинающийся за концом
//! media или дальше [`MAX_PLAUSIBLE_AUDIO_START_GAP`] от базы, — это битые метки времени
//! (эпоха эфира, сломанный mux), а не отложенная дорожка. Ждать его — значит не услышать
//! звук вовсе, поэтому там прежнее поведение (звук в базу) с предупреждением в лог.
//!
//! Начало показа позже базы целиком ([`PlayerSession::start_at_earliest_content_if_late`]):
//! если на первом старте и звук, и первый кадр лежат дальше базы больше потолка тишины
//! (MPEG-TS от ffmpeg: оба потока с ~1,4 с при базе 0), это не «отложенная дорожка», а
//! поздно начинающаяся запись. Ждать её с чёрным экраном незачем: база старта переносится
//! на самое раннее содержимое — как audio-only стартует с первого sample-а (и как mpv).
//! Позиция показывает сырое время контейнера (решение s14: шкала не сдвигается).
//!
//! Live-timeline (dynamic) сюда не входит: база там — край эфира, а разрывы времени
//! ведёт своя логика; поведение там прежнее.

use std::time::{Duration, Instant};

use media_core::TimelineMode;
use tracing::debug;

use crate::PlaybackState;

use super::super::PlayerSession;
use super::super::audio_runtime::leading_silence::LeadingSilencePolicy;
use super::super::audio_runtime::{AudioAutoplayReadiness, SeekAudioGateStatus};

/// Самый большой разрыв «база → первый звук», который ещё считается задуманным.
///
/// Час — с запасом длиннее любой реальной немой заставки, но заметно короче того, что
/// дают битые метки времени (эпоха эфира, 33-битный перенос PTS — часы и сутки). Нужен,
/// когда длительность media неизвестна; известная длительность ограничивает строже.
pub(in crate::session) const MAX_PLAUSIBLE_AUDIO_START_GAP: Duration = Duration::from_secs(60 * 60);

/// Правдоподобен ли разрыв между базой часов и первым звуком отрезка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum AudioStartGapPlausibility {
    /// Звук начинается внутри media и не дальше [`MAX_PLAUSIBLE_AUDIO_START_GAP`].
    Plausible,

    /// Звук начинается не раньше конца media: таких меток у настоящей дорожки нет.
    BeyondMediaEnd {
        /// Известный конец media (физическая длительность источника).
        media_end: Duration,
    },

    /// Разрыв больше [`MAX_PLAUSIBLE_AUDIO_START_GAP`].
    AbsurdGap,
}

impl AudioStartGapPlausibility {
    /// `true`, если разрыв можно выравнивать (ждать звук или переносить часы).
    #[must_use]
    pub(in crate::session) const fn is_plausible(self) -> bool {
        matches!(self, Self::Plausible)
    }
}

/// Чистая классификация разрыва; `media_end` — `None`, если длительность неизвестна.
#[must_use]
pub(in crate::session) fn classify_audio_start_gap(
    clock_base: Duration,
    audio_start: Duration,
    media_end: Option<Duration>,
) -> AudioStartGapPlausibility {
    if let Some(media_end) = media_end
        && audio_start >= media_end
    {
        return AudioStartGapPlausibility::BeyondMediaEnd { media_end };
    }
    if audio_start.saturating_sub(clock_base) > MAX_PLAUSIBLE_AUDIO_START_GAP {
        return AudioStartGapPlausibility::AbsurdGap;
    }
    AudioStartGapPlausibility::Plausible
}

/// Когда должен зазвучать звук текущего отрезка относительно базы часов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum SegmentStartAudioSchedule {
    /// Правило не применяется: отрезок уже начат, нет видео или звука, нет pending
    /// packet-а текущего поколения, либо live-timeline.
    NotApplicable,

    /// Первый звук не дальше потолка тишины: его выровняет `leading_silence.rs`.
    WithinSilenceCap,

    /// Первый звук дальше потолка тишины (или её не вместить в вывод при текущей
    /// скорости): он ждёт своего PTS, часы идут без него.
    ScheduledLater {
        /// База часов, к которой привязан отрезок.
        clock_base: Duration,

        /// PTS первого pending audio packet-а.
        audio_start: Duration,
    },

    /// Разрыв неправдоподобен (битые метки времени): прежнее поведение, звук в базу.
    ImplausibleGap {
        /// База часов, к которой привязан отрезок.
        clock_base: Duration,

        /// PTS первого pending audio packet-а.
        audio_start: Duration,

        /// Почему разрыв отвергнут.
        plausibility: AudioStartGapPlausibility,
    },
}

impl SegmentStartAudioSchedule {
    /// `true`, если звук отрезка сейчас ждёт своего PTS.
    #[must_use]
    pub(in crate::session) const fn holds_audio(self) -> bool {
        matches!(self, Self::ScheduledLater { .. })
    }
}

/// Audio gate стартового preroll-а с учётом звука, назначенного на потом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum PrerollAudioGate {
    /// Обычная готовность звука (decoder/output/preroll буфера).
    Readiness(AudioAutoplayReadiness),

    /// Звук начнётся позже потолка тишины: ждать его на старте нельзя — старт идёт
    /// видео первым, звук войдёт в свой PTS.
    ScheduledLater {
        /// PTS первого звука отрезка.
        audio_start: Duration,
    },
}

impl PrerollAudioGate {
    /// `true`, если звук не держит старт.
    #[must_use]
    pub(in crate::session) const fn is_ready(self) -> bool {
        match self {
            Self::Readiness(readiness) => readiness.is_ready(),
            Self::ScheduledLater { .. } => true,
        }
    }
}

/// Итог попытки впустить назначенный на потом звук при отсутствии output-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum ScheduledAudioJoinOutcome {
    /// Звук не назначен на потом: делать нечего.
    NotScheduled,

    /// Показ не идёт (preroll, пауза, seek): позиция стоит, звук ждёт.
    PresentationNotRunning,

    /// Output уже есть: вход ведёт передача часов разрыва (`audio_gap.rs`).
    AudioClockOwnsResume,

    /// Время показа ещё не дошло до звука.
    NotDue {
        /// Текущая позиция показа.
        presentation_position: Duration,

        /// PTS первого звука отрезка.
        audio_start: Duration,
    },

    /// Звук уже впущен, но первый PCM ещё не записан, а показ ушёл вперёд: база
    /// подтянута к позиции показа, чтобы часы звука не отбросили её назад.
    KeptUpWithPresentation {
        /// База, отставшая от показа.
        previous_base: Duration,

        /// Новая база (позиция показа).
        joined_at: Duration,
    },

    /// База часов перенесена на позицию показа: первый PCM ляжет ровно туда.
    Joined {
        /// Новая база часов (позиция показа в момент входа звука).
        joined_at: Duration,

        /// PTS первого звука отрезка.
        audio_start: Duration,
    },
}

/// Итог попытки начать первый показ с самого раннего содержимого.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum EarliestContentStart {
    /// Правило не применяется: не первый старт, звук не назначен на потом, кадра ещё
    /// нет или кадр не дальше потолка тишины от базы (тогда видео идёт с базы).
    NotApplicable,

    /// База старта перенесена на самое раннее содержимое.
    Rebased {
        /// Прежняя база (начало шкалы).
        previous_base: Duration,

        /// Новая база: min(первый кадр, первый звук).
        content_start: Duration,
    },
}

impl PlayerSession {
    /// Классифицирует первый pending звук текущего отрезка относительно базы часов.
    #[must_use]
    pub(in crate::session) fn segment_start_audio_schedule(&self) -> SegmentStartAudioSchedule {
        if self.snapshot.timeline.mode != TimelineMode::Static
            || !self.pipeline.has_selected_video_track()
            || !self.pipeline.has_selected_audio_track()
            || !self.pipeline.audio_segment_awaits_first_pcm()
        {
            return SegmentStartAudioSchedule::NotApplicable;
        }
        let Some(audio_start) = self.pipeline.front_pending_audio_packet_start() else {
            return SegmentStartAudioSchedule::NotApplicable;
        };

        let clock_base = self.pipeline.media_clock_base();
        let gap = audio_start.saturating_sub(clock_base);
        // Тишина закрывает разрыв, только если и по длине в потолке, и растянутая
        // скоростью влезает в вывод (0,25×: 0,5 с тишины — 2 с вывода, ring — ~2 с).
        // Иначе звук ждёт своего PTS тем же путём, что и разрыв больше потолка.
        if gap <= LeadingSilencePolicy::SEGMENT_START.max_silence
            && self.silence_output_room(gap).fits()
        {
            return SegmentStartAudioSchedule::WithinSilenceCap;
        }
        let plausibility = self.audio_start_gap_plausibility(clock_base, audio_start);
        if !plausibility.is_plausible() {
            return SegmentStartAudioSchedule::ImplausibleGap {
                clock_base,
                audio_start,
                plausibility,
            };
        }
        SegmentStartAudioSchedule::ScheduledLater {
            clock_base,
            audio_start,
        }
    }

    /// Правдоподобие разрыва с учётом известной длительности текущего media.
    #[must_use]
    pub(in crate::session) fn audio_start_gap_plausibility(
        &self,
        clock_base: Duration,
        audio_start: Duration,
    ) -> AudioStartGapPlausibility {
        classify_audio_start_gap(clock_base, audio_start, self.source_duration)
    }

    /// Audio gate стартового preroll-а: обычная готовность или «звук назначен на потом».
    #[must_use]
    pub(in crate::session) fn preroll_audio_gate(
        &self,
        audio_preroll_target_ms: f64,
    ) -> PrerollAudioGate {
        if let SegmentStartAudioSchedule::ScheduledLater { audio_start, .. } =
            self.segment_start_audio_schedule()
        {
            return PrerollAudioGate::ScheduledLater { audio_start };
        }
        PrerollAudioGate::Readiness(self.autoplay_audio_readiness(audio_preroll_target_ms))
    }

    /// Отпускает ли назначенный на потом звук audio gate seek commit-а.
    ///
    /// Звук цели начнётся позже потолка тишины, поэтому буфер до commit-а не наберётся
    /// никогда. Ждать нужно только подтверждение очистки буфера этого поколения: без
    /// неё в output-е может остаться звук до seek-а.
    #[must_use]
    pub(in crate::session) fn scheduled_audio_start_releases_seek_audio_gate(
        &self,
        audio_gate: SeekAudioGateStatus,
    ) -> bool {
        audio_gate != SeekAudioGateStatus::WaitingForClear
            && self.segment_start_audio_schedule().holds_audio()
    }

    /// Можно ли закрыть seek с намерением играть без output-а звука.
    ///
    /// Звук, назначенный на потом, создаёт output сам, когда войдёт в свой PTS; до
    /// этого output-а может не быть (seek во время старта видео первым) — это не ошибка.
    #[must_use]
    pub(in crate::session) fn seek_commit_defers_audio_output_to_scheduled_start(&self) -> bool {
        !self.pipeline.has_audio_output() && self.segment_start_audio_schedule().holds_audio()
    }

    /// Должен ли output, созданный первым PCM, сразу начать играть.
    ///
    /// Да, если показ уже идёт: обычная игра или EOF drain уже начатого показа (звук,
    /// назначенный на потом, может войти после EOF источника). Во время preroll-а и
    /// пока EOF drain ещё ждёт первый старт, output запускает сам старт.
    #[must_use]
    pub(in crate::session) fn lazily_created_audio_output_joins_running_presentation(
        &self,
    ) -> bool {
        self.playback_state() == PlaybackState::Playing
            || (self.eof_drain_needs_progress()
                && !self.startup_autoplay_pending_during_eof_drain())
    }

    /// Первый старт: и звук, и первый кадр позже базы больше потолка тишины — показ
    /// начинается с самого раннего из них, без чёрного экрана до первого кадра.
    ///
    /// Вызывается воротами старта до проверки готовности: после переноса звук может
    /// оказаться в пределах потолка тишины и тогда стартует обычным путём (видео и звук
    /// вместе, часы у звука), а не «видео первым».
    pub(in crate::session) fn start_at_earliest_content_if_late(&mut self) -> EarliestContentStart {
        if !self.startup_autoplay_preroll_pending() || self.has_active_seek_commit() {
            return EarliestContentStart::NotApplicable;
        }
        let SegmentStartAudioSchedule::ScheduledLater {
            clock_base,
            audio_start,
        } = self.segment_start_audio_schedule()
        else {
            return EarliestContentStart::NotApplicable;
        };
        let Some(first_frame) = self.pipeline.earliest_ready_video_frame_pts() else {
            return EarliestContentStart::NotApplicable;
        };
        if first_frame.saturating_sub(clock_base) <= LeadingSilencePolicy::SEGMENT_START.max_silence
        {
            return EarliestContentStart::NotApplicable;
        }

        let content_start = first_frame.min(audio_start);
        self.pipeline.set_media_clock_base(content_start);
        // Monotonic clock старта якорится от опубликованной позиции.
        self.publish_clock_sample(content_start);
        debug!(
            first_frame_ms = first_frame.as_millis(),
            audio_start_ms = audio_start.as_millis(),
            "Start moved to the earliest content (first frame or first audio)"
        );
        EarliestContentStart::Rebased {
            previous_base: clock_base,
            content_start,
        }
    }

    /// Через сколько настоящего времени показ дойдёт до удержанного звука.
    ///
    /// Для wakeup-а worker-а: без кадров впереди (видео кончилось раньше звука) иначе
    /// некому разбудить worker к PTS звука, и вход опоздал бы на грубый интервал —
    /// звук обрезался бы на это опоздание. `None` — звук не удержан или показ не идёт.
    #[must_use]
    pub(in crate::session) fn held_audio_start_wall_delay(&self, now: Instant) -> Option<Duration> {
        if !self.pending_audio_is_held_until_its_pts()
            || self.has_active_seek_commit()
            || self.audio_clock_drives_presentation()
            || !self.monotonic_media_clock_drives_position()
        {
            return None;
        }
        let audio_start = self.pipeline.front_pending_audio_packet_start()?;
        Some(self.wall_delay_until_media_deadline(now, audio_start))
    }

    /// Впускает назначенный на потом звук, когда показ дошёл до его PTS, а output-а нет.
    ///
    /// Без output-а нет и audio clock: время ведёт monotonic clock. База часов переносится
    /// на текущую позицию показа — output, который создаст первый PCM, поставит свои часы
    /// туда же (`install_audio_clock` берёт базу), а PCM раньше неё обрежет обычная
    /// обрезка по базе (она идёт после создания output-а).
    ///
    /// Если первый PCM не появился в tick входа (decoder отдаёт звук с задержкой), база
    /// осталась бы позади показа, и часы звука отбросили бы позицию назад. Поэтому, пока
    /// отрезок ждёт первый PCM, а показ идёт без audio clock, база подтягивается к позиции
    /// показа на каждом tick-е ([`ScheduledAudioJoinOutcome::KeptUpWithPresentation`]).
    /// Задержку старта устройства это не убирает: до первых прозвучавших sample-ов часы
    /// звука стоят — звук ведёт синхрон, видео ждёт его (как при возврате после разрыва).
    pub(in crate::session) fn join_scheduled_audio_start_if_due(
        &mut self,
        now: Instant,
    ) -> ScheduledAudioJoinOutcome {
        let schedule = self.segment_start_audio_schedule();
        if !matches!(
            schedule,
            SegmentStartAudioSchedule::ScheduledLater { .. }
                | SegmentStartAudioSchedule::WithinSilenceCap
        ) {
            return ScheduledAudioJoinOutcome::NotScheduled;
        }
        let Some(audio_start) = self.pipeline.front_pending_audio_packet_start() else {
            return ScheduledAudioJoinOutcome::NotScheduled;
        };
        if self.has_active_seek_commit()
            || !(self.playback_state() == PlaybackState::Playing || self.eof_drain_needs_progress())
        {
            return ScheduledAudioJoinOutcome::PresentationNotRunning;
        }
        if self.pipeline.has_audio_clock() {
            return ScheduledAudioJoinOutcome::AudioClockOwnsResume;
        }

        let presentation_position = self.presentation_clock_position_at(now);
        let clock_base = self.pipeline.media_clock_base();
        if schedule == SegmentStartAudioSchedule::WithinSilenceCap {
            if presentation_position <= clock_base {
                return ScheduledAudioJoinOutcome::NotDue {
                    presentation_position,
                    audio_start,
                };
            }
            self.pipeline.set_media_clock_base(presentation_position);
            return ScheduledAudioJoinOutcome::KeptUpWithPresentation {
                previous_base: clock_base,
                joined_at: presentation_position,
            };
        }
        if presentation_position < audio_start {
            return ScheduledAudioJoinOutcome::NotDue {
                presentation_position,
                audio_start,
            };
        }

        // Monotonic clock продолжает вести время до установки audio clock-а: тот снимет
        // monotonic anchor сам, уже с этой базой.
        self.pipeline.set_media_clock_base(presentation_position);
        debug!(
            joined_at_ms = presentation_position.as_millis(),
            audio_start_ms = audio_start.as_millis(),
            "Audio scheduled after the clock base joins the running presentation"
        );
        ScheduledAudioJoinOutcome::Joined {
            joined_at: presentation_position,
            audio_start,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Длительность media для проверок границы.
    const MEDIA_END: Duration = Duration::from_secs(10);

    #[test]
    fn gap_inside_known_media_is_plausible() {
        let plausibility =
            classify_audio_start_gap(Duration::ZERO, Duration::from_secs(3), Some(MEDIA_END));

        assert_eq!(plausibility, AudioStartGapPlausibility::Plausible);
    }

    #[test]
    fn audio_starting_at_or_after_media_end_is_implausible() {
        for audio_start in [MEDIA_END, MEDIA_END + Duration::from_secs(3 * 3_600)] {
            assert_eq!(
                classify_audio_start_gap(Duration::ZERO, audio_start, Some(MEDIA_END)),
                AudioStartGapPlausibility::BeyondMediaEnd {
                    media_end: MEDIA_END
                },
                "audio start {audio_start:?}"
            );
        }
    }

    #[test]
    fn unknown_duration_is_bounded_by_the_absolute_gap_cap() {
        let base = Duration::from_secs(5);

        let at_cap = classify_audio_start_gap(base, base + MAX_PLAUSIBLE_AUDIO_START_GAP, None);
        let above_cap = classify_audio_start_gap(
            base,
            base + MAX_PLAUSIBLE_AUDIO_START_GAP + Duration::from_millis(1),
            None,
        );

        assert_eq!(at_cap, AudioStartGapPlausibility::Plausible);
        assert_eq!(above_cap, AudioStartGapPlausibility::AbsurdGap);
    }

    #[test]
    fn preroll_gate_scheduled_later_never_blocks_the_start() {
        assert!(
            PrerollAudioGate::ScheduledLater {
                audio_start: Duration::from_secs(2)
            }
            .is_ready()
        );
        assert!(!PrerollAudioGate::Readiness(AudioAutoplayReadiness::WaitingForDecoder).is_ready());
    }
}
