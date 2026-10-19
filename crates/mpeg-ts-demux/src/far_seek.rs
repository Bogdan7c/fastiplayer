//! Seek за пределы индекса: позиция ищется по смещению файла, а не проходом.
//!
//! Начальный индекс покрывает голову файла, `expand_index_towards` дочитывает его
//! по одному окну. Для цели за многие окна (гигабайтная запись) честный проход
//! нерационален, поэтому здесь: (1) бисекция по PTS (`seek_bisect`) либо, для
//! склеек, пропорциональная позиция; (2) локальное сканирование окна от найденной
//! точки обычной машинерией индекса; (3) выбор keyframe-anchor-а теми же правилами,
//! что у seek-а по индексу.
//!
//! Инварианты:
//! - `seek_index`, `index_scan_offset` и continuation НЕ меняются: индекс остаётся
//!   отсортированным префиксным покрытием `[0, index_scan_offset)`, а anchor-ы
//!   далёкого окна живут только на время этого seek-а. Обратный seek внутрь
//!   покрытия идёт обычным путём, далёкий seek вперёд повторяет поиск.
//! - Сканирование транзакционно: parser state и позиция reader-а восстанавливаются
//!   при любом исходе (успех, ошибка, отмена), реальный переход делает
//!   `land_on_anchor`.
//! - Склейки (`FromByteRate` или обнаруженная немонотонность PTS): позиция
//!   приблизительная. Шкала эпохи сдвигается так, чтобы точка пропорциональной
//!   позиции получила время цели минус preroll; итог — keyframe не позже цели с
//!   погрешностью неравномерности битрейта, но время дальше идёт непрерывно.

use std::collections::HashSet;

use media_core::{DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult};

use super::index::SeekAnchor;
use super::{ContainerDurationEstimate, MpegTsDemuxer, StreamTimestamps, mpeg_clock};
use crate::MpegTsDemuxError;
use crate::framing::{TS_PACKET_BYTES, TransportReadOutcome};
use crate::seek_bisect::{
    BisectionOutcome, BisectionRequest, TimestampSample, bisect_to_timestamp, probe_timestamp_at,
    proportional_offset, proportional_timestamp,
};
use crate::timeline_continuity::{EpochPlacement, TimelineEpochId};
use crate::timestamps::TimestampUnwrapper;

/// Во сколько раз скобка бисекции должна быть уже окна сканирования.
const BRACKET_WINDOW_DIVISOR: u64 = 4;

/// Сколько раз пропорциональная позиция отступает назад, не найдя PES.
const PROPORTIONAL_RETRIES: usize = 8;

/// Откуда начинать локальное сканирование и в какой эпохе шкалы.
struct ScanStart {
    sample: TimestampSample,
    epoch: EpochPlacement,
}

impl MpegTsDemuxer {
    /// Seek к цели за индексом; см. документацию модуля.
    pub(super) fn seek_beyond_index(
        &mut self,
        request: &DemuxSeekRequest,
        target_90khz: i64,
    ) -> Result<DemuxSeekResult, MpegTsDemuxError> {
        let resume_offset = self.reader.position();
        let snapshot = self.snapshot_parser_state();
        let located = self.locate_far_anchor(request, target_90khz);
        // Откат выполняется всегда; ошибка отката важнее ошибки поиска.
        let rollback = self.reader.restore_after_index_scan(resume_offset);
        self.restore_parser_state(snapshot);
        rollback?;
        self.land_on_anchor(located?, request.timestamp)
    }

    /// Ищет anchor далёкой цели; изменяет parser state (откатывает вызывающий).
    fn locate_far_anchor(
        &mut self,
        request: &DemuxSeekRequest,
        target_90khz: i64,
    ) -> Result<SeekAnchor, MpegTsDemuxError> {
        let unavailable = MpegTsDemuxError::SeekAnchorUnavailable {
            target: request.timestamp,
        };
        let (Some(content_length), Some(duration)) = (self.reader.content_length(), self.duration)
        else {
            return Err(unavailable);
        };
        let end_90khz = to_ticks(duration);
        let preroll_90khz = to_ticks(self.options.far_seek_preroll);
        // Цель за концом файла сжимается до конца: садимся на последний keyframe.
        let aim_90khz = target_90khz.min(end_90khz).saturating_sub(preroll_90khz);
        let start = self.far_scan_start(content_length, end_90khz, aim_90khz)?;
        let start = start.ok_or(unavailable)?;

        let unavailable = || MpegTsDemuxError::SeekAnchorUnavailable {
            target: request.timestamp,
        };
        self.begin_far_scan(&start)?;
        let window_packets = self.options.seek_scan_packets.get();
        let mut checked_anchors = 0_usize;
        for _ in 0..window_packets {
            match self.reader.next_packet()? {
                TransportReadOutcome::Packet(packet) => {
                    self.process_transport_packet(packet, false)?;
                }
                TransportReadOutcome::EndResource(_metadata) => {
                    self.finish_streamed_resource(false)?;
                }
                TransportReadOutcome::EndOfInput => {
                    self.finish_pending_elementary_streams(false)?;
                    break;
                }
            }
            if self.seek_index.len() != checked_anchors {
                checked_anchors = self.seek_index.len();
                let passed_target = self
                    .seek_index
                    .iter()
                    .any(|anchor| anchor.timestamp_90khz > target_90khz);
                if passed_target
                    && select_anchor(&self.seek_index, target_90khz, request.mode).is_some()
                {
                    break;
                }
            }
        }
        select_anchor(&self.seek_index, target_90khz, request.mode).ok_or_else(unavailable)
    }

    /// Выбирает точку старта локального сканирования: бисекция по PTS для одной
    /// записи, пропорциональная позиция для склеек и немонотонного PTS.
    fn far_scan_start(
        &mut self,
        content_length: u64,
        end_90khz: i64,
        aim_90khz: i64,
    ) -> Result<Option<ScanStart>, MpegTsDemuxError> {
        let elementary_pids: HashSet<u16> = self.stream_by_pid.keys().copied().collect();
        let head_start_90khz = self
            .presentation_start
            .presentation_start()
            .map_or(0, to_ticks);
        let probe_bytes = self.options.far_seek_probe_bytes.get();
        if matches!(
            self.duration_basis,
            ContainerDurationEstimate::FromTailTimestamps(_)
        ) {
            let coverage_end = self.index_coverage_end();
            let lower = self
                .seek_index
                .iter()
                .rev()
                .filter(|anchor| anchor.byte_offset < coverage_end)
                .find(|anchor| anchor.epoch.id == TimelineEpochId::StreamStart)
                .map_or(
                    TimestampSample {
                        byte_offset: 0,
                        pts_90khz: head_start_90khz,
                    },
                    |anchor| TimestampSample {
                        byte_offset: anchor.byte_offset,
                        pts_90khz: anchor.timestamp_90khz,
                    },
                );
            let window_bytes = (self.options.seek_scan_packets.get() * TS_PACKET_BYTES) as u64;
            let outcome = bisect_to_timestamp(
                &mut self.reader,
                BisectionRequest {
                    elementary_pids: &elementary_pids,
                    aim_90khz,
                    lower,
                    upper: TimestampSample {
                        byte_offset: content_length,
                        pts_90khz: end_90khz,
                    },
                    probe_bytes,
                    bracket_bytes: window_bytes / BRACKET_WINDOW_DIVISOR,
                },
            )?;
            if let BisectionOutcome::Located(sample) = outcome {
                return Ok(Some(ScanStart {
                    sample,
                    epoch: EpochPlacement::STREAM_START,
                }));
            }
        } else if !matches!(
            self.duration_basis,
            ContainerDurationEstimate::FromByteRate(_)
        ) {
            return Ok(None);
        }
        // Склейка/немонотонный PTS: позиция пропорциональна времени.
        let offset = proportional_offset(content_length, head_start_90khz, end_90khz, aim_90khz);
        // Точка может попасть в участок без PES (null-пакеты, мусор): отступаем
        // назад по зонду, ограниченное число раз.
        for attempt in 0..PROPORTIONAL_RETRIES {
            let attempt_offset = offset.saturating_sub((attempt * probe_bytes) as u64);
            let sample = probe_timestamp_at(
                &mut self.reader,
                attempt_offset,
                &elementary_pids,
                probe_bytes,
                head_start_90khz,
            )?;
            if let Some(sample) = sample {
                return Ok(Some(ScanStart {
                    sample,
                    epoch: EpochPlacement {
                        id: TimelineEpochId::StartsAtByte(sample.byte_offset),
                        // Время точки по модели «равномерный битрейт» в её
                        // реальном месте (после отступов назад и поиска PES).
                        offset_90khz: proportional_timestamp(
                            content_length,
                            head_start_90khz,
                            end_90khz,
                            sample.byte_offset,
                        )
                        .saturating_sub(sample.pts_90khz),
                    },
                }));
            }
        }
        Ok(None)
    }

    /// Готовит чистое сканирование с `start`: пустой локальный индекс, опора
    /// unwrap-ов, шкала в нужной эпохе.
    fn begin_far_scan(&mut self, start: &ScanStart) -> Result<(), MpegTsDemuxError> {
        // Локальные anchor-ы не должны смешиваться с индексом и его лимитом.
        self.seek_index = Vec::new();
        self.reader.begin_index_scan(start.sample.byte_offset)?;
        self.reset_elementary_state();
        self.timeline.restart_in_epoch(start.epoch);
        let reference = start.sample.pts_90khz;
        self.pcr_timestamp = TimestampUnwrapper::from_unwrapped_reference(reference);
        for pid in self.stream_by_pid.keys().copied() {
            self.timestamps_by_pid.insert(
                pid,
                StreamTimestamps {
                    pts: TimestampUnwrapper::from_unwrapped_reference(reference),
                    dts: TimestampUnwrapper::from_unwrapped_reference(reference),
                },
            );
        }
        self.pending_events.clear();
        self.reached_end = false;
        Ok(())
    }
}

/// Последний anchor не позже цели (для `DecodePointBefore` — только decode-safe).
/// В отличие от seek-а по индексу, запасного anchor-а «с нуля» нет: далёкий seek
/// без найденного keyframe-а честно сообщает `SeekAnchorUnavailable`.
fn select_anchor(
    anchors: &[SeekAnchor],
    target_90khz: i64,
    mode: DemuxSeekMode,
) -> Option<SeekAnchor> {
    anchors
        .iter()
        .rev()
        .filter(|anchor| mode != DemuxSeekMode::DecodePointBefore || anchor.decode_safe)
        .find(|anchor| anchor.timestamp_90khz <= target_90khz)
        .copied()
}

/// Время → такты 90 кГц с насыщением.
fn to_ticks(duration: std::time::Duration) -> i64 {
    mpeg_clock()
        .duration_to_track_units_saturating(duration.into())
        .get()
}
