//! Разрыв звука посреди воспроизведения: звук кончился (или пропал на время), видео идёт.
//!
//! Живой T410 (сессия 13b): H.264, видео 20 с, звук только первые 5 с. Audio clock встал
//! на 5 с, decode-ahead держал video packets, demux в audio-priority режиме дочитал 240
//! video packets в ожидании звука и остановился — взаимная блокировка в `Playing`, до EOF
//! дело не доходило. Тот же сценарий с разрывом посреди файла (звук 0–5 и 12–20 с) без
//! блокировки давал склейку: звук 12 с звучал на позиции 5 с (рассинхрон на 7 с).
//!
//! Поведение как у mpv/VLC: на время отсутствия звука время ведёт monotonic clock
//! (передача из `pipeline/presentation_clock_handover.rs`), видео играет непрерывно,
//! а звук после разрыва ждёт своего PTS и затем забирает часы обратно с текущей позиции.
//!
//! Передача выполняется только по доказательству, а не по таймеру:
//! - **packet после разрыва**: следующий audio packet начинается позже конца уже
//!   записанного звука больше чем на [`MIN_AUDIO_GAP`] — контейнер сам говорит, что звука
//!   между ними нет;
//! - **demux упёрся без звука**: audio-priority demux дочитал лимит video packets, ни
//!   одного audio packet-а в очереди нет, а output доиграл весь звук. Demux читает файл
//!   по порядку, поэтому звука в этом отрезке нет; без передачи это вечная блокировка.
//!   Но только если прочитанное видео ушло дальше конца записанного звука больше чем на
//!   [`MIN_AUDIO_GAP`]. Если video decoder отстаёт (HW на 4x), эти video packets лежат
//!   ДО конца звука, а следующий audio packet сразу за ними в файле: насыщение demux-а
//!   тогда ничего не доказывает, звук надо ждать, а не отрезать.
//!
//! В обоих случаях pipeline дополнительно требует, чтобы output доиграл записанный звук,
//! поэтому до разрыва звук и часы остаются прежними.

use std::time::{Duration, Instant};

use media_core::TimelineMode;
use tracing::{debug, info};

use crate::PlaybackState;
use crate::pipeline::{
    AudioEndClockHandoverOutcome, AudioGapResumeOutcome, PresentationClockSource,
};

use super::PlayerSession;

/// Минимальный разрыв между концом записанного звука и следующим audio packet-ом,
/// который считается настоящим отсутствием звука, а не джиттером timestamps.
///
/// Обычные кадры AAC/Opus/AC-3 — 20–40 мс, tempo-процессор держит до ~100 мс; полсекунды
/// заведомо больше любого штатного шва, но меньше заметного глазу рассинхрона.
pub(crate) const MIN_AUDIO_GAP: Duration = Duration::from_millis(500);

/// Можно ли сейчас отдать front pending audio packet в decoder/output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingAudioOutputAdmission {
    /// Обычная обработка звука.
    Admit,

    /// Packet лежит после разрыва звука и ждёт, пока до него дойдёт время показа.
    HoldForAudioGap,
}

/// Состояние demux audio-priority ожидания на этом tick-е (вычисляет `tick`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DemuxAudioPriorityWait {
    /// Demux ради звука дочитал лимит video packets и не может читать дальше.
    SaturatedWithoutAudio,

    /// Demux может читать дальше (или audio-priority режим не активен).
    NotSaturated,
}

/// Доказательство отсутствия звука на ближайшем отрезке (для диагностики).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioGapEvidence {
    /// Следующий audio packet начинается позже конца записанного звука.
    NextAudioStartsAfterGap {
        /// Media-позиция конца уже записанного звука.
        audio_end: Duration,

        /// PTS следующего audio packet-а.
        next_audio_start: Duration,
    },

    /// Demux дочитал лимит video packets, не встретив ни одного audio packet-а.
    DemuxSaturatedWithoutAudio,
}

impl PlayerSession {
    /// Решает, можно ли сейчас обработать front pending audio packet.
    ///
    /// Звук после доказанного разрыва не пишется в output раньше времени: иначе output
    /// склеил бы его с предыдущим звуком и сдвинул весь A/V sync на длину разрыва.
    #[must_use]
    pub(crate) fn pending_audio_output_admission(&self) -> PendingAudioOutputAdmission {
        match self.pipeline.presentation_clock_source() {
            // Пока время ведёт monotonic clock, звук возвращается только через
            // `resume_audio_after_gap_if_due` — он перепривязывает часы до записи PCM.
            PresentationClockSource::MonotonicAfterAudioEnd => {
                if self.pipeline.front_pending_audio_packet_start().is_some() {
                    PendingAudioOutputAdmission::HoldForAudioGap
                } else {
                    PendingAudioOutputAdmission::Admit
                }
            }
            PresentationClockSource::AudioOutput if self.audio_gap_ahead().is_some() => {
                PendingAudioOutputAdmission::HoldForAudioGap
            }
            PresentationClockSource::AudioOutput
            | PresentationClockSource::MonotonicWithoutAudio => PendingAudioOutputAdmission::Admit,
        }
    }

    /// Передаёт часы monotonic clock-у, если отсутствие звука впереди доказано.
    ///
    /// Вызывается tick-ом после обработки звука и до video scheduler-а, чтобы видео
    /// этого же tick-а уже шло по новым часам.
    pub(crate) fn hand_over_presentation_clock_for_audio_gap_if_proven(
        &mut self,
        now: Instant,
        demux_audio_wait: DemuxAudioPriorityWait,
    ) {
        if !self.audio_gap_policy_applies() || !self.audio_clock_drives_presentation() {
            return;
        }
        let Some(evidence) = self.audio_gap_evidence(demux_audio_wait) else {
            return;
        };

        let outcome = self
            .pipeline
            .hand_over_presentation_clock_for_audio_gap(now, self.snapshot.playback_rate);
        match outcome {
            AudioEndClockHandoverOutcome::HandedOver { audio_end_position } => {
                info!(
                    evidence = ?evidence,
                    audio_end_position_ms = audio_end_position.as_millis(),
                    pending_video_packets = self.pipeline.pending_video_packet_len(),
                    "Audio is absent ahead; video continues on monotonic clock"
                );
            }
            // Output ещё доигрывает звук до разрыва: передача случится на следующем tick-е.
            AudioEndClockHandoverOutcome::AudioOutputStillPlaying { .. }
            | AudioEndClockHandoverOutcome::AlreadyHandedOver
            | AudioEndClockHandoverOutcome::NoAudioClock
            | AudioEndClockHandoverOutcome::NoAudioOutput
            | AudioEndClockHandoverOutcome::AudioTailNotDrained { .. } => {}
        }
    }

    /// Возвращает часы звуку, когда время показа дошло до первого packet-а после разрыва.
    ///
    /// Часы перепривязываются к текущей monotonic-позиции, а PCM раньше неё обрезается
    /// обычной trim-логикой: позиция не прыгает назад, звук совпадает с видео.
    pub(crate) fn resume_audio_after_gap_if_due(&mut self, now: Instant) {
        if self.pipeline.presentation_clock_source()
            != PresentationClockSource::MonotonicAfterAudioEnd
            || !self.audio_gap_policy_applies()
        {
            return;
        }
        let Some(next_audio_start) = self.pipeline.front_pending_audio_packet_start() else {
            return;
        };
        let presentation_position = self.presentation_clock_position_at(now);
        if next_audio_start > presentation_position {
            return;
        }

        match self
            .pipeline
            .resume_audio_clock_after_gap(presentation_position)
        {
            AudioGapResumeOutcome::Resumed { resumed_at } => {
                // Stall-детектор не должен принять ожидание первых samples за остановку.
                let audio_now = self.audio_clock_now();
                self.pipeline.reset_audio_clock_sample(audio_now, now);
                self.publish_clock_sample(resumed_at);
                debug!(
                    resumed_at_ms = resumed_at.as_millis(),
                    next_audio_start_ms = next_audio_start.as_millis(),
                    "Audio returned after gap; audio clock drives presentation again"
                );
            }
            AudioGapResumeOutcome::NotHandedOver => {}
        }
    }

    /// Применима ли gap-политика: обычное воспроизведение static media с видео и звуком.
    ///
    /// Seek/scrub владеют своим lifecycle-ом; live-timeline имеет свои разрывы времени;
    /// audio-only некому продолжать показ, поэтому там поведение прежнее.
    fn audio_gap_policy_applies(&self) -> bool {
        matches!(
            self.playback_state(),
            PlaybackState::Playing | PlaybackState::Draining
        ) && !self.has_active_seek_commit()
            && self.snapshot.timeline.mode == TimelineMode::Static
            && self.pipeline.has_selected_video_track()
            && self.pipeline.has_selected_audio_track()
    }

    /// Возвращает границы разрыва, если следующий audio packet начинается после разрыва.
    fn audio_gap_ahead(&self) -> Option<AudioGapEvidence> {
        if !self.audio_gap_policy_applies() {
            return None;
        }
        let next_audio_start = self.pipeline.front_pending_audio_packet_start()?;
        let audio_end = self.pipeline.written_audio_end()?;
        let gap_threshold = audio_end.checked_add(MIN_AUDIO_GAP)?;
        (next_audio_start > gap_threshold).then_some(AudioGapEvidence::NextAudioStartsAfterGap {
            audio_end,
            next_audio_start,
        })
    }

    /// Собирает доказательство отсутствия звука впереди.
    fn audio_gap_evidence(
        &self,
        demux_audio_wait: DemuxAudioPriorityWait,
    ) -> Option<AudioGapEvidence> {
        if let Some(evidence) = self.audio_gap_ahead() {
            return Some(evidence);
        }

        // Блокировка demux-а доказывает отсутствие звука только при обычном чтении файла:
        // после EOF demux не читает, там работает EOF-drain передача.
        let demux_saturated = demux_audio_wait == DemuxAudioPriorityWait::SaturatedWithoutAudio
            && self.playback_state() == PlaybackState::Playing
            && self.pipeline.pending_audio_packet_is_empty();
        if !demux_saturated || !self.demux_read_video_far_past_written_audio() {
            return None;
        }
        Some(AudioGapEvidence::DemuxSaturatedWithoutAudio)
    }

    /// Прочитал ли demux видео дальше последнего слышанного звука больше чем на `MIN_AUDIO_GAP`.
    ///
    /// Demux отдаёт packets в порядке файла. Если самый дальний прочитанный video packet
    /// позже опорной точки на полсекунды и больше, а audio packet-а между ними не
    /// встретилось, то звука в этом отрезке файла действительно нет. Если же video packets
    /// лежат до конца звука (decoder отстаёт, очередь забита «старым» видео), следующий
    /// audio packet ещё не прочитан и может идти сразу за ними: доказательства нет.
    ///
    /// Опорная точка: конец записанного звука. После seek/reset он сброшен (звук нового
    /// отрезка ещё не писался, например seek в область файла без звука), и тогда опора —
    /// текущая позиция показа: после seek-а demux начал читать ровно с неё, значит всё
    /// прочитанное видео правее позиции прошло без audio packet-ов (pending audio пуст,
    /// записанного звука нет — это проверяет вызывающий код).
    fn demux_read_video_far_past_written_audio(&self) -> bool {
        let Some(video_frontier) = self.pipeline.furthest_pending_video_packet_start() else {
            return false;
        };
        let reference_point = self
            .pipeline
            .written_audio_end()
            .unwrap_or(self.snapshot.current_position);
        reference_point
            .checked_add(MIN_AUDIO_GAP)
            .is_some_and(|gap_threshold| video_frontier > gap_threshold)
    }
}
