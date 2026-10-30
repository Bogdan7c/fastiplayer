//! Forward extension живого скраба: цель вперёд без нового demux seek-а.
//!
//! Перенесено из `seek_start.rs` (предел размера модуля) вместе с проверкой звука
//! (дефект D2, живая приёмка drag-а): extension очищает output под новую цель, а demux
//! не перечитывает уже прочитанное. Записанный PCM новой цели при этом пропадал бы:
//! после отпускания мыши — дыра в звуке или звук раньше картинки до следующего seek-а.
//!
//! Главная защита — звук живого прохода вообще не декодируется до commit-а
//! (`audio_gap.rs`, `pending_audio_waits_for_live_scrub_commit`): сжатые packets
//! extension не трогает. Здесь — страховки на случай, если PCM всё же записан:
//! - extension отклоняется (cold route с demux seek-ом перечитает и звук, и видео);
//! - переиспользование прохода на отпускании мыши отклоняется, если звук отрезка
//!   начинается позже цели commit-а (новый seek вместо прохода с дырой).

use std::time::{Duration, Instant};

use frame_server_core::LiveScrubDiagnostics;
use media_core::MediaTime;
use tracing::debug;

use crate::pipeline::audio_segment_start::AudioSegmentWrittenReach;
use crate::seek_state::SeekCommitState;

use super::super::PlayerSession;
use super::super::scrub_orchestration::LIVE_SCRUB_FORWARD_EXTENSION_MAX;

/// Сколько звука цели допустимо потерять при перезапуске отрезка звука.
///
/// Допуск A/V приёмки «до кадра» (±40 мс, меньше кадра 25 fps). Шов такого размера
/// закрывает тишина начала отрезка (`audio_runtime/leading_silence.rs`), A/V остаётся
/// в синхроне. Значение — свойство восприятия, а не настройка пользователя.
const LIVE_SCRUB_AUDIO_CONTINUITY_TOLERANCE: Duration = Duration::from_millis(40);

/// Сохранит ли forward extension звук новой цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveScrubExtensionAudio {
    /// Записанного PCM за новой целью нет (или нет звука): extension ничего не теряет.
    Kept,

    /// PCM отрезка записан дальше новой цели (или его конец неизвестен): очистка output-а
    /// выбросит звук, который demux без нового seek-а не перечитает.
    WouldDropWrittenAudio(AudioSegmentWrittenReach),
}

impl PlayerSession {
    /// Продолжает активный live scrub проход к более поздней цели без нового cold seek.
    ///
    /// Реюз позиции декодера: движение вперёд в пределах капа не делает flush
    /// декодера и demux seek на keyframe-before — уже идущий decode-проход просто
    /// доезжает до новой цели, а прокат показывает кадры по пути. Generation,
    /// pending video packets и очередь кадров остаются валидными. Audio runtime
    /// сбрасывается под новый target: output очищается, clock base перепривязывается,
    /// и trace начинается заново, чтобы landing новой цели снова опубликовал
    /// `SeekTargetFramePresented`. Сжатый звук остаётся в очереди: пока мышь не
    /// отпущена, он не декодируется (`pending_audio_waits_for_live_scrub_commit`),
    /// поэтому очистка output-а не теряет звук новой цели. Если PCM всё же записан
    /// дальше новой цели, extension отклоняется — см. [`LiveScrubExtensionAudio`].
    ///
    /// Возвращает `true`, если расширение применено и cold route не нужен.
    pub(super) fn try_extend_active_live_scrub_landing_forward(
        &mut self,
        target_position: MediaTime,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        // Расширять можно только активный live scrub decode-проход до EndScrub:
        // после запрошенного commit-а целью владеет release-логика.
        if !self.seek_runtime.active_seek_landing_is_live_scrub()
            || !self.seek_runtime.seek_landing_decode_active()
            || self.seek_runtime.active_seek_landing_commit_allowed()
        {
            return false;
        }

        // Accurate-политика проката обязана быть активной, иначе окажемся в
        // расширении неподдерживаемого маршрута.
        if !seek_commit.drops_decode_preroll_before_target() {
            return false;
        }

        // EOF drain: demuxer уже не читает — новая цель обязана пройти cold route,
        // который заново seek-ает demuxer и выходит из drain-а.
        if self.is_eof_draining() {
            return false;
        }

        let current_target = seek_commit.target_position.as_duration();
        let new_target = target_position.as_duration();
        if new_target < current_target {
            return false;
        }
        // Дистанцию меряем от фактического прогресса decoder-а в этом проходе, а не от
        // прошлой цели указателя: иначе цепочка мелких шагов (< капа каждый) тянет один
        // проход от старого keyframe бесконечно, и медленный (HW) decoder отстаёт от
        // указателя на секунды. Кап «докатить дешевле cold seek» верен, только пока цель
        // близко к decoder-у. Кадров в проходе ещё нет — берём прошлую цель, как раньше.
        let decode_progress = self
            .pipeline
            .latest_video_decoder_output_pts()
            .unwrap_or(current_target);
        if new_target.saturating_sub(decode_progress) > LIVE_SCRUB_FORWARD_EXTENSION_MAX {
            return false;
        }
        // Очистка output-а ниже выбросит записанный PCM, а demux без нового seek-а его
        // повторно не прочитает: звук новой цели пропал бы (дефект D2).
        if let LiveScrubExtensionAudio::WouldDropWrittenAudio(written_reach) =
            self.live_scrub_extension_audio(new_target)
        {
            debug!(
                kind = "seek",
                new_target_ms = new_target.as_millis(),
                written_reach = ?written_reach,
                "Live scrub: written audio reaches past the new target; cold route keeps it"
            );
            return false;
        }

        // Audio под новый target: буфер от старой цели очищается, decoder
        // сбрасывается, clock base перепривязывается. Video decoder не трогаем.
        self.reset_audio_runtime_for_seek_landing(seek_commit.generation);
        self.pipeline.reset_clocks_for_seek(new_target);
        // Сжатый звук до новой цели уже не прозвучит: очередь не копит весь drag.
        let discarded_audio_packets = self
            .pipeline
            .discard_pending_audio_superseded_by_target(new_target);

        self.seek_runtime.set_active_commit(SeekCommitState {
            target_position,
            started_at: Instant::now(),
            ..seek_commit
        });

        // Trace заново: landing новой цели снова должен эмитить
        // `SeekTargetFramePresented`, иначе UI completion-gate ждёт fallback budget.
        self.seek_runtime.clear_trace();
        self.seek_runtime.begin_trace(seek_commit.generation);
        self.seek_runtime
            .update_active_live_scrub_diagnostics(live_scrub_diagnostics);

        self.enter_seek_landing_public_scrubbing(target_position);

        // Generation не менялся: уже показанный target-or-after кадр остаётся
        // landing для маленького сдвига внутри его интервала. Повторяем evidence
        // для новой цели, иначе каждый drag event принудительно съедает ещё кадр.
        if let Some(frame_pts) = self.pipeline.present_video_frame().map(|frame| frame.pts) {
            self.note_presented_frame_for_seek(frame_pts);
        }

        debug!(
            kind = "seek",
            old_target_ms = current_target.as_millis(),
            new_target_ms = new_target.as_millis(),
            generation = seek_commit.generation,
            discarded_audio_packets,
            "Live scrub: target extended forward without a new cold seek"
        );
        true
    }

    /// Оценивает, переживёт ли звук новой цели перезапуск отрезка без demux seek-а.
    fn live_scrub_extension_audio(&self, new_target: Duration) -> LiveScrubExtensionAudio {
        let written_reach = self.pipeline.audio_segment_written_reach();
        let lost_audio_fits_tolerance = match written_reach {
            AudioSegmentWrittenReach::NoSelectedAudio | AudioSegmentWrittenReach::NoPcmWritten => {
                true
            }
            AudioSegmentWrittenReach::EndsAt(written_end) => {
                written_end <= new_target.saturating_add(LIVE_SCRUB_AUDIO_CONTINUITY_TOLERANCE)
            }
            // Сколько звука в output-е, неизвестно: безопасная сторона — не терять его.
            AudioSegmentWrittenReach::EndUnknown => false,
        };
        if lost_audio_fits_tolerance {
            LiveScrubExtensionAudio::Kept
        } else {
            LiveScrubExtensionAudio::WouldDropWrittenAudio(written_reach)
        }
    }

    /// `true`, если звук переиспользуемого live scrub прохода начинается с цели commit-а.
    ///
    /// Страховка на отпускании мыши: проход переиспользуется вместо нового seek-а только
    /// когда его отрезок звука ещё не начат или начат не позже цели (с допуском). Отрезок,
    /// начатый позже цели, значит, что звук цели уже потерян — новый seek перечитает его.
    /// Звук, который в самом файле начинается позже цели, тоже уйдёт в новый seek: исход
    /// тот же (тишина до звука), лишняя только стоимость seek-а.
    pub(in crate::session) fn reused_live_scrub_audio_starts_at_target(
        &self,
        commit_target: Duration,
    ) -> bool {
        match self.pipeline.audio_segment_first_sample() {
            Some(first_sample) => {
                first_sample <= commit_target.saturating_add(LIVE_SCRUB_AUDIO_CONTINUITY_TOLERANCE)
            }
            // PCM отрезка ещё не записан: звук декодируется после commit-а с обрезкой по цели.
            None => true,
        }
    }
}
