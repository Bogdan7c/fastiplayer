//! Передача presentation clock от доигранного звука monotonic-часам для хвоста видео.
//!
//! Зачем: пока audio clock установлен, он единственный источник времени показа. Если
//! звуковая дорожка короче видео (или seek пропустил короткий хвост звука как pre-target),
//! output доигрывает весь PCM и audio clock навсегда останавливается на конце звука.
//! Кадры видео с PTS после этой точки тогда никогда не становятся due, EOF drain ждёт
//! их вечно и playlist не переходит к следующему файлу.
//!
//! Решение как в mpv/VLC: после конца звука хвост видео доигрывается по monotonic
//! media clock с текущей скоростью, начиная с media-позиции, где звук закончился.
//!
//! Тот же механизм покрывает разрыв звука посреди файла (звук 0–5 с, тишины в контейнере
//! нет до 12 с, дальше звук снова есть): на время разрыва время ведёт monotonic clock,
//! а когда следующий audio packet становится due, звук забирает часы обратно через
//! [`PlaybackPipeline::resume_audio_clock_after_gap`] с той же media-позиции.
//!
//! Владелец обоих часов — [`PlaybackPipeline`], поэтому и состояние передачи живёт здесь.
//! Решение «когда» принимает EOF drain в session (он знает, что видео ещё не доиграно);
//! pipeline проверяет только свои инварианты: audio clock есть и звук действительно доигран.
//! Передача отменяется на каждом разрыве времени: новый seek generation, (пере)установка
//! или удаление audio clock, перепривязка audio clock к другой media-позиции, сброс media.

use std::time::{Duration, Instant};

use super::{AudioEofDrainState, PlaybackPipeline};
use crate::PlaybackRate;

/// Какой источник времени сейчас ведёт presentation clock.
///
/// Три состояния намеренно не сводятся к `bool`: «audio clock отсутствует» и «audio clock
/// есть, но уже передал время» по-разному влияют на pause/resume, диагностику и тесты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentationClockSource {
    /// Audio output clock установлен и ведёт время (обычное A/V воспроизведение).
    AudioOutput,

    /// Audio clock установлен, но звук доигран: хвост видео идёт по monotonic media clock.
    MonotonicAfterAudioEnd,

    /// Audio clock нет (video-only media или звук ещё не запущен): monotonic fallback.
    MonotonicWithoutAudio,
}

impl PresentationClockSource {
    /// Возвращает `true`, если время показа сейчас берётся из audio output clock.
    #[must_use]
    pub(crate) const fn is_audio_output(self) -> bool {
        matches!(self, Self::AudioOutput)
    }
}

/// Внутреннее состояние передачи времени; читать его можно только через pipeline-методы.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioEndClockHandover {
    /// Передачи не было: при наличии audio clock время ведёт он.
    #[default]
    NotHandedOver,

    /// Время передано monotonic-часам после конца звука.
    HandedOver {
        /// Media-позиция, на которой остановился доигранный audio clock (старт хвоста).
        audio_end_position: Duration,
    },
}

/// Результат попытки передать presentation clock после конца звука.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AudioEndClockHandoverOutcome {
    /// Передача выполнена: monotonic clock запущен от позиции конца звука.
    HandedOver {
        /// Media-позиция, с которой хвост видео продолжает идти по monotonic clock.
        audio_end_position: Duration,
    },

    /// Передача уже была выполнена ранее в этом же отрезке времени; ничего не изменилось.
    AlreadyHandedOver,

    /// Audio clock не установлен: время и так ведёт monotonic fallback, передавать нечего.
    NoAudioClock,

    /// Звук ещё не доигран (или нет output-а): audio clock продолжает вести время.
    AudioTailNotDrained {
        /// Фактическое состояние audio tail-а, из-за которого передача отклонена.
        audio_tail: AudioEofDrainState,
    },

    /// Разрыв звука доказан, но output ещё играет звук до разрыва: передавать рано.
    AudioOutputStillPlaying {
        /// Сколько миллисекунд записанного звука ещё не прозвучало.
        unplayed_tail_ms: f64,
    },

    /// Audio clock есть, а output-а нет: доигранность звука проверить нечем.
    NoAudioOutput,
}

/// Результат возврата часов звуку после разрыва.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioGapResumeOutcome {
    /// Audio clock снова ведёт время с заданной media-позиции.
    Resumed {
        /// Media-позиция, к которой привязан первый новый PCM звука.
        resumed_at: Duration,
    },

    /// Передачи не было: возвращать нечего, состояние не менялось.
    NotHandedOver,
}

impl PlaybackPipeline {
    /// Возвращает источник времени, который сейчас ведёт presentation clock.
    #[must_use]
    pub(crate) fn presentation_clock_source(&self) -> PresentationClockSource {
        if !self.has_audio_clock() {
            return PresentationClockSource::MonotonicWithoutAudio;
        }

        match self.audio_end_clock_handover {
            AudioEndClockHandover::NotHandedOver => PresentationClockSource::AudioOutput,
            AudioEndClockHandover::HandedOver { .. } => {
                PresentationClockSource::MonotonicAfterAudioEnd
            }
        }
    }

    /// Передаёт presentation clock monotonic-часам, когда audio output полностью доигран.
    ///
    /// Вызывает только EOF drain session-а, когда видео ещё не доиграно. Pipeline сам
    /// проверяет свои инварианты: audio clock установлен, передачи ещё не было, а audio
    /// tail в состоянии `DrainedOutput`. Monotonic clock стартует от media-позиции, где
    /// остановился audio clock, с переданной скоростью воспроизведения.
    pub(crate) fn hand_over_presentation_clock_after_audio_end(
        &mut self,
        now: Instant,
        playback_rate: PlaybackRate,
    ) -> AudioEndClockHandoverOutcome {
        if !self.has_audio_clock() {
            return AudioEndClockHandoverOutcome::NoAudioClock;
        }

        if let AudioEndClockHandover::HandedOver { .. } = self.audio_end_clock_handover {
            return AudioEndClockHandoverOutcome::AlreadyHandedOver;
        }

        let audio_tail = self.audio_eof_drain_state();
        if !matches!(audio_tail, AudioEofDrainState::DrainedOutput { .. }) {
            return AudioEndClockHandoverOutcome::AudioTailNotDrained { audio_tail };
        }

        let audio_end_position = self.start_handed_over_presentation_clock(now, playback_rate);
        AudioEndClockHandoverOutcome::HandedOver { audio_end_position }
    }

    /// Передаёт presentation clock monotonic-часам на время доказанного разрыва звука.
    ///
    /// Доказательство разрыва (packet после разрыва или demux, упёршийся в лимит без
    /// звука) собирает session; pipeline проверяет только свой инвариант: output уже
    /// проиграл весь записанный звук. Pending audio packets здесь допустимы — это звук
    /// после разрыва, он ждёт своего PTS.
    pub(crate) fn hand_over_presentation_clock_for_audio_gap(
        &mut self,
        now: Instant,
        playback_rate: PlaybackRate,
    ) -> AudioEndClockHandoverOutcome {
        if !self.has_audio_clock() {
            return AudioEndClockHandoverOutcome::NoAudioClock;
        }

        if let AudioEndClockHandover::HandedOver { .. } = self.audio_end_clock_handover {
            return AudioEndClockHandoverOutcome::AlreadyHandedOver;
        }

        let Some(unplayed_tail_ms) = self.audio_output_unplayed_tail_ms() else {
            return AudioEndClockHandoverOutcome::NoAudioOutput;
        };
        if unplayed_tail_ms > 0.0 {
            return AudioEndClockHandoverOutcome::AudioOutputStillPlaying { unplayed_tail_ms };
        }

        let audio_end_position = self.start_handed_over_presentation_clock(now, playback_rate);
        AudioEndClockHandoverOutcome::HandedOver { audio_end_position }
    }

    /// Возвращает часы звуку после разрыва: новый PCM начнёт звучать с `resumed_at`.
    ///
    /// `resumed_at` — текущая monotonic-позиция показа. Audio mapping перепривязывается
    /// так, что ещё не записанный PCM соответствует именно ей, а PCM раньше неё будет
    /// обрезан обычной trim-логикой по media base. Поэтому позиция не прыгает назад
    /// (часы продолжают с того же места), а звук не опережает и не отстаёт от видео.
    pub(crate) fn resume_audio_clock_after_gap(
        &mut self,
        resumed_at: Duration,
    ) -> AudioGapResumeOutcome {
        if self.audio_end_clock_handover == AudioEndClockHandover::NotHandedOver {
            return AudioGapResumeOutcome::NotHandedOver;
        }

        // Перепривязка к новой media base сама отменяет передачу (см. `audio.rs`).
        self.set_media_clock_base(resumed_at);
        self.clear_monotonic_media_clock();
        // Новый непрерывный отрезок звука: старый конец (до разрыва) больше не сравниваем.
        self.forget_written_audio_end();
        self.begin_audio_segment();
        AudioGapResumeOutcome::Resumed { resumed_at }
    }

    /// Запоминает media-конец звука, только что записанного в output.
    ///
    /// Это timestamps самого контейнера (PTS packet-а + длительность декодированного
    /// PCM), а не оценка по часам: следующий packet, начинающийся заметно позже этой
    /// точки, доказывает разрыв звука.
    pub(crate) fn note_audio_packet_written(
        &mut self,
        packet_start: Duration,
        decoded_sample_count: usize,
        output_spec: audio_core::AudioOutputSpec,
    ) {
        let channel_count = output_spec.channels() as usize;
        if channel_count == 0 || output_spec.sample_rate == 0 {
            return;
        }
        let frame_count = (decoded_sample_count / channel_count) as u128;
        let pcm_nanos =
            frame_count.saturating_mul(1_000_000_000) / u128::from(output_spec.sample_rate);
        let pcm_duration = Duration::from_nanos(u64::try_from(pcm_nanos).unwrap_or(u64::MAX));
        let packet_end = packet_start.saturating_add(pcm_duration);
        // Перекрывающиеся packets (priming, B-кадры кодека) не сдвигают конец назад.
        self.written_audio_end = Some(
            self.written_audio_end
                .map_or(packet_end, |previous_end| previous_end.max(packet_end)),
        );
    }

    /// Media-конец последнего записанного звука текущего непрерывного отрезка.
    ///
    /// `None` — после seek/reset/возврата звука ещё ничего не записано: разрыв мерить
    /// не от чего, и первый packet нового отрезка никогда не задерживается.
    #[must_use]
    pub(crate) const fn written_audio_end(&self) -> Option<Duration> {
        self.written_audio_end
    }

    /// Забывает конец записанного звука на разрыве timeline-а (seek, reset, возврат звука).
    pub(super) fn forget_written_audio_end(&mut self) {
        self.written_audio_end = None;
    }

    /// PTS начала первого pending audio packet-а выбранного трека текущего поколения.
    ///
    /// Чужие и устаревшие packets не считаются: их отбросит обычная обработка звука.
    #[must_use]
    pub(crate) fn front_pending_audio_packet_start(&self) -> Option<Duration> {
        let packet = self.pending_audio_packets.front()?;
        let belongs_to_current_audio = self.audio_track_id == Some(packet.track_id())
            && self.packet_generation_is_current(packet.generation());
        belongs_to_current_audio.then(|| packet.pts())
    }

    /// Запускает monotonic clock с позиции остановившегося audio clock и помечает передачу.
    fn start_handed_over_presentation_clock(
        &mut self,
        now: Instant,
        playback_rate: PlaybackRate,
    ) -> Duration {
        // Доигранный audio clock больше не движется: его media-позиция и есть конец звука.
        let audio_end_position = self.media_position_from_audio_clock();
        self.start_monotonic_media_clock(audio_end_position, now, playback_rate);
        self.audio_end_clock_handover = AudioEndClockHandover::HandedOver { audio_end_position };
        audio_end_position
    }

    /// Отменяет передачу времени на разрыве: audio clock снова главный, если он есть.
    ///
    /// Monotonic anchor здесь не трогается: его lifecycle (seek, pause, install) уже
    /// ведут вызывающие boundary-методы, а лишний сброс сломал бы no-audio playback.
    pub(super) fn cancel_audio_end_clock_handover(&mut self) {
        self.audio_end_clock_handover = AudioEndClockHandover::NotHandedOver;
    }
}
