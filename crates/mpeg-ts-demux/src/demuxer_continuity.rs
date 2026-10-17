//! «Мягкие» разрывы `MpegTsDemuxer`: потеря/склейка транспорта и перенос
//! elementary timestamps на одну шкалу презентации.
//!
//! Вынесено из `demuxer.rs` (лимит размера модуля); владелец состояния тот же —
//! `MpegTsDemuxer`, модуль группирует методы этой границы.
//!
//! Разрыв `continuity_counter` (склейка `cat a.ts b.ts`, потеря пакетов в записи
//! эфира) раньше публиковал `TracksChanged`, а player-core на него пересобирает
//! весь pipeline: терялось 0,2–0,4 с звука, видео ждало keyframe. Теперь разрыв
//! затрагивает только повреждённую elementary единицу этого PID-а; состав
//! дорожек меняют лишь PAT/PMT/config evidence. Скачок самих timestamps на стыке
//! записей склеивает `PresentationTimeline` (`timeline_continuity.rs`).

use tracing::warn;

use super::MpegTsDemuxer;
use crate::MpegTsDemuxError;
use crate::framing::TransportPacket;
use crate::pes::{InterruptedPes, PesPacket, peek_pes_pts};
use crate::psi::StreamKind;
use crate::timeline_continuity::{
    DiscontinuityWindow, ElementaryClock, EpochPlacement, EpochRevision,
};

/// Timestamps elementary unit-а на шкале презентации.
#[derive(Debug, Clone, Copy)]
pub(super) struct PresentationTimestamps {
    pub(super) pts: i64,
    pub(super) dts: Option<i64>,
    /// Эпоха, в которой лежит unit (её получает seek anchor).
    pub(super) epoch: EpochPlacement,
}

impl MpegTsDemuxer {
    /// Восстанавливает PID после разрыва `continuity_counter` без lifecycle-события.
    pub(super) fn recover_from_continuity_gap(
        &mut self,
        packet: &TransportPacket,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        let pid = packet.pid;
        if !self.stream_by_pid.contains_key(&pid) {
            // PSI PID: незаконченная секция потеряна, сборка начнётся со
            // следующего pointer_field. Повтор PAT/PMT придёт сам.
            warn!(
                pid,
                "MPEG-TS continuity gap on a PSI PID; section assembly restarts"
            );
            self.reset_pid(pid);
            return Ok(());
        }
        if packet.payload_unit_start {
            // Пакет начинает новый PES: потеряны целые пакеты до него (или это
            // первая запись склейки). Предыдущий PES мог быть полным —
            // дособираем его, а не выбрасываем последний кадр записи.
            warn!(
                pid,
                "MPEG-TS continuity gap before a PES start; previous PES is closed"
            );
            self.close_pes_interrupted_by_gap(pid, &packet.payload, publish_lifecycle)?;
        } else {
            // Потеряна середина PES: он повреждён целиком. Продолжение без
            // заголовка `PesAssembler` пропустит до следующего payload_unit_start.
            warn!(
                pid,
                "MPEG-TS continuity gap inside a PES; the damaged PES is dropped"
            );
            if let Some(assembler) = self.pes_by_pid.get_mut(&pid) {
                assembler.reset();
            }
            self.discard_incomplete_strict_audio_tail(pid);
        }
        Ok(())
    }

    /// Закрывает PES, прерванный разрывом перед новым payload_unit_start.
    ///
    /// - Объявленная длина набрана — PES полный, обрабатывается как обычно.
    /// - Объявленной длины не хватило — потерян его хвост: PES отбрасывается с
    ///   предупреждением, а не роняет поток typed `Malformed` (это потеря
    ///   пакетов, а не нарушение формата).
    /// - Длина не объявлена (видео): если новый PES начинает другую шкалу
    ///   времени, это стык склеенных записей и прежний PES закончился вместе со
    ///   своей записью — он полный. Если время идёт дальше, потерянные пакеты
    ///   могли быть его хвостом: обрезанный кадр отбрасывается, как и раньше.
    fn close_pes_interrupted_by_gap(
        &mut self,
        pid: u16,
        next_payload: &[u8],
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        let interrupted = match self.pes_by_pid.get_mut(&pid) {
            Some(assembler) => assembler.close_after_continuity_gap(),
            None => Ok(InterruptedPes::Absent),
        };
        match interrupted {
            Ok(InterruptedPes::Absent) => {}
            Ok(InterruptedPes::Complete(pes)) => self.process_pes(pes, publish_lifecycle)?,
            Ok(InterruptedPes::LengthNotDeclared(pes)) => {
                let window = DiscontinuityWindow::new(self.options.timestamp_discontinuity);
                if next_pes_restarts_timeline(window, &pes, next_payload) {
                    self.process_pes(pes, publish_lifecycle)?;
                } else {
                    warn!(
                        pid,
                        "PES of unknown length before a continuity gap may be truncated; dropped"
                    );
                }
            }
            Err(MpegTsDemuxError::Malformed { reason }) => {
                warn!(pid, %reason, "PES cut by a continuity gap is dropped");
            }
            Err(error) => return Err(error),
        }
        self.discard_incomplete_strict_audio_tail(pid);
        Ok(())
    }

    /// ADTS/MPEG audio разбираются строго: начало кадра без продолжения
    /// (продолжение было в потерянных пакетах) с новыми байтами дало бы
    /// `Malformed`. Хвост отбрасывается; AC-3 сам ищет синхронизацию, а его
    /// E-AC-3 lookahead — целый кадр, поэтому AC-3 буфер не трогается.
    fn discard_incomplete_strict_audio_tail(&mut self, pid: u16) {
        let strict_audio = matches!(
            self.stream_by_pid.get(&pid),
            Some(StreamKind::AacAdts | StreamKind::MpegAudio)
        );
        if !strict_audio {
            return;
        }
        let Some(accumulator) = self.audio_by_pid.get(&pid) else {
            return;
        };
        if !accumulator.bytes.is_empty() {
            warn!(
                pid,
                discarded_bytes = accumulator.bytes.len(),
                "incomplete audio frame before a continuity gap is dropped"
            );
            self.audio_by_pid.remove(&pid);
        }
    }

    /// Переносит raw (unwrapped) timestamps unit-а на шкалу презентации.
    ///
    /// Полосу ведёт decode-время: DTS (с B-кадрами монотонен только он), без
    /// DTS — PTS. Если первый аудиокадр уточнил сдвиг эпохи, anchor-ы этой
    /// эпохи переносятся вместе с ней.
    pub(super) fn present_on_timeline(
        &mut self,
        pid: u16,
        clock: ElementaryClock,
        raw_pts: i64,
        raw_dts: Option<i64>,
        duration_90khz: Option<i64>,
        byte_offset: u64,
    ) -> PresentationTimestamps {
        let placement = self.timeline.place_elementary(
            pid,
            clock,
            raw_dts.unwrap_or(raw_pts),
            duration_90khz,
            byte_offset,
        );
        if placement.revision == EpochRevision::OffsetRefined {
            self.move_seek_anchors_to_refined_epoch(placement.epoch);
        }
        PresentationTimestamps {
            pts: placement.epoch.present(raw_pts),
            dts: raw_dts.map(|dts| placement.epoch.present(dts)),
            epoch: placement.epoch,
        }
    }
}

/// Начинает ли PES в `next_payload` другую шкалу времени, чем `previous`.
/// Без PTS у одного из них стык не доказан.
fn next_pes_restarts_timeline(
    window: DiscontinuityWindow,
    previous: &PesPacket,
    next_payload: &[u8],
) -> bool {
    match (previous.pts, peek_pes_pts(next_payload)) {
        (Some(previous_pts), Some(next_pts)) => {
            window.raw_timestamps_restart(previous_pts, next_pts)
        }
        _ => false,
    }
}
