//! Поиск байтовой позиции по времени для далёкого seek-а (аналог `read_timestamp`
//! бисекции у ffmpeg).
//!
//! Индекс покрывает только начало файла, а seek дочитывает его по одному окну.
//! Для цели за многие окна вперёд проход всего файла недопустим, поэтому позиция
//! ищется по смещению: читается маленький «зонд» в точке файла, из первого PES
//! берётся PTS, и скобка `[нижняя, верхняя]` сужается интерполяцией, чередуемой с
//! делением пополам (худший случай — логарифм размера файла). Модуль читает только
//! `TransportPacketReader` и не владеет состоянием demuxer-а; возвращать reader на
//! позицию playback обязан вызывающий (`restore_after_index_scan`).
//!
//! Предположение: PTS монотонно растёт вместе со смещением. Склейка записей с
//! перезапуском PTS его нарушает — бисекция тогда сообщает `NonMonotonic`, а
//! вызывающий код переходит на пропорциональную позицию (`proportional_offset`).

use std::collections::HashSet;

use crate::MpegTsDemuxError;
use crate::framing::{TS_PACKET_BYTES, TransportPacketReader, TransportReadOutcome};
use crate::pes::peek_pes_pts;
use crate::timestamps::TimestampUnwrapper;

/// Жёсткий предел числа зондов одной бисекции (страховка от зацикливания).
const MAX_BISECTION_PROBES: usize = 48;
/// Насколько PTS зонда может быть «не в ту сторону» (B-кадры, расхождение
/// аудио/видео), не считаясь нарушением монотонности: 2 с в тактах 90 кГц.
const MONOTONICITY_TOLERANCE_90KHZ: i64 = 180_000;
/// Доля скобки, ближе которой к краям не выбирается точка зонда: гарантирует, что
/// скобка сужается минимум на 1/16 даже при плохой интерполяции.
const MIN_PROBE_MARGIN_DIVISOR: u64 = 16;

/// Точка файла с известным PTS (развёрнутым, 90 кГц).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimestampSample {
    /// Байтовая позиция начала TS-пакета, с которого начинается PES.
    pub(crate) byte_offset: u64,
    /// Развёрнутый PTS в тактах 90 кГц.
    pub(crate) pts_90khz: i64,
}

/// Параметры бисекции.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BisectionRequest<'a> {
    /// PID-ы elementary-потоков выбранной программы.
    pub(crate) elementary_pids: &'a HashSet<u16>,
    /// Желаемый PTS: нужна точка не позже него.
    pub(crate) aim_90khz: i64,
    /// Известная точка не позже цели (например, последний anchor индекса).
    pub(crate) lower: TimestampSample,
    /// Верхняя граница: обычно конец файла с PTS длительности.
    pub(crate) upper: TimestampSample,
    /// Сколько байт читает один зонд в поисках PES.
    pub(crate) probe_bytes: usize,
    /// Скобка уже этого размера считается достаточно узкой.
    pub(crate) bracket_bytes: u64,
}

/// Итог бисекции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BisectionOutcome {
    /// Найдена точка с PTS не позже цели; дальше локальное сканирование от неё.
    Located(TimestampSample),
    /// PTS не монотонен по смещению (склейка/мусор): бисекции верить нельзя.
    NonMonotonic,
}

/// Бисекция по смещению. Только `Cancelled` выходит ошибкой; прочие сбои зонда
/// (пустой участок, мусор) сужают скобку, не ломая поиск.
pub(crate) fn bisect_to_timestamp(
    reader: &mut TransportPacketReader,
    request: BisectionRequest<'_>,
) -> Result<BisectionOutcome, MpegTsDemuxError> {
    let mut lower = request.lower;
    let mut upper = request.upper;
    if request.aim_90khz <= lower.pts_90khz {
        return Ok(BisectionOutcome::Located(lower));
    }
    for iteration in 0..MAX_BISECTION_PROBES {
        if upper.byte_offset.saturating_sub(lower.byte_offset) <= request.bracket_bytes {
            break;
        }
        let guess = next_probe_offset(lower, upper, request.aim_90khz, iteration);
        let sample = probe_timestamp_at(
            reader,
            guess,
            request.elementary_pids,
            request.probe_bytes,
            lower.pts_90khz,
        )?;
        let Some(sample) = sample else {
            // На участке нет PES с PTS: верхняя граница сдвигается к точке зонда.
            upper.byte_offset = guess;
            continue;
        };
        if sample.byte_offset >= upper.byte_offset {
            // Зонд проскочил верхнюю границу — между guess и upper PES нет.
            upper.byte_offset = guess;
            continue;
        }
        if sample
            .pts_90khz
            .saturating_add(MONOTONICITY_TOLERANCE_90KHZ)
            < lower.pts_90khz
            || sample.pts_90khz > upper.pts_90khz.saturating_add(MONOTONICITY_TOLERANCE_90KHZ)
        {
            return Ok(BisectionOutcome::NonMonotonic);
        }
        if sample.pts_90khz <= request.aim_90khz {
            lower = sample;
        } else {
            upper = sample;
        }
    }
    Ok(BisectionOutcome::Located(lower))
}

/// Следующая точка зонда строго внутри `(lower, upper)`: чётные итерации —
/// интерполяция по PTS, нечётные — середина (гарантия логарифмической сходимости).
pub(crate) fn next_probe_offset(
    lower: TimestampSample,
    upper: TimestampSample,
    aim_90khz: i64,
    iteration: usize,
) -> u64 {
    let span = upper.byte_offset.saturating_sub(lower.byte_offset);
    let margin = (span / MIN_PROBE_MARGIN_DIVISOR).max(1);
    let earliest = lower.byte_offset.saturating_add(margin);
    let latest = upper.byte_offset.saturating_sub(margin).max(earliest);
    let pts_span = i128::from(upper.pts_90khz) - i128::from(lower.pts_90khz);
    let interpolated = if iteration.is_multiple_of(2) && pts_span > 0 {
        let progress = i128::from(aim_90khz) - i128::from(lower.pts_90khz);
        let advance = i128::from(span) * progress / pts_span;
        u64::try_from(advance)
            .ok()
            .map(|advance| lower.byte_offset.saturating_add(advance))
    } else {
        None
    };
    interpolated
        .unwrap_or(lower.byte_offset.saturating_add(span / 2))
        .clamp(earliest, latest)
}

/// Позиция пропорционально времени: `content_length * (aim - start) / (end - start)`.
///
/// Для склеек и файлов без монотонного PTS: точность ограничена равномерностью
/// битрейта, зато работает без доверия к timestamps. Результат ограничен длиной файла.
pub(crate) fn proportional_offset(
    content_length: u64,
    start_90khz: i64,
    end_90khz: i64,
    aim_90khz: i64,
) -> u64 {
    let total = i128::from(end_90khz) - i128::from(start_90khz);
    if total <= 0 {
        return 0;
    }
    let elapsed = (i128::from(aim_90khz) - i128::from(start_90khz)).clamp(0, total);
    let offset = i128::from(content_length) * elapsed / total;
    u64::try_from(offset)
        .unwrap_or(content_length)
        .min(content_length)
}

/// Обратное к `proportional_offset`: время, которое пропорциональная модель
/// приписывает байтовой позиции.
pub(crate) fn proportional_timestamp(
    content_length: u64,
    start_90khz: i64,
    end_90khz: i64,
    byte_offset: u64,
) -> i64 {
    if content_length == 0 {
        return start_90khz;
    }
    let total = i128::from(end_90khz) - i128::from(start_90khz);
    let elapsed = total * i128::from(byte_offset.min(content_length)) / i128::from(content_length);
    i64::try_from(i128::from(start_90khz) + elapsed).unwrap_or(end_90khz)
}

/// Читает короткий участок с `byte_offset` и возвращает первый PES с PTS.
///
/// `reference_90khz` — опора для разворачивания 33-битного счётчика. `Ok(None)` —
/// в пределах `probe_bytes` нет PES известных PID или участок повреждён.
pub(crate) fn probe_timestamp_at(
    reader: &mut TransportPacketReader,
    byte_offset: u64,
    elementary_pids: &HashSet<u16>,
    probe_bytes: usize,
    reference_90khz: i64,
) -> Result<Option<TimestampSample>, MpegTsDemuxError> {
    match reader.begin_index_scan(byte_offset) {
        Ok(()) => {}
        Err(MpegTsDemuxError::Cancelled) => return Err(MpegTsDemuxError::Cancelled),
        Err(_) => return Ok(None),
    }
    let mut unwrapper = TimestampUnwrapper::from_unwrapped_reference(reference_90khz);
    for _ in 0..probe_bytes / TS_PACKET_BYTES + 1 {
        match reader.next_packet() {
            Ok(TransportReadOutcome::Packet(packet)) => {
                if !packet.payload_unit_start
                    || packet.transport_error
                    || packet.scrambling != 0
                    || !elementary_pids.contains(&packet.pid)
                {
                    continue;
                }
                if let Some(raw_pts) = peek_pes_pts(&packet.payload) {
                    return Ok(Some(TimestampSample {
                        byte_offset: packet.byte_offset,
                        pts_90khz: unwrapper.unwrap(raw_pts),
                    }));
                }
            }
            Ok(_) => return Ok(None),
            Err(MpegTsDemuxError::Cancelled) => return Err(MpegTsDemuxError::Cancelled),
            Err(_) => return Ok(None),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(byte_offset: u64, pts_90khz: i64) -> TimestampSample {
        TimestampSample {
            byte_offset,
            pts_90khz,
        }
    }

    #[test]
    fn interpolation_hits_linear_stream_and_odd_iterations_bisect() {
        let lower = sample(0, 0);
        let upper = sample(1_000_000, 1_000_000);
        assert_eq!(next_probe_offset(lower, upper, 300_000, 0), 300_000);
        assert_eq!(next_probe_offset(lower, upper, 300_000, 1), 500_000);
    }

    #[test]
    fn probe_offset_stays_strictly_inside_bracket() {
        let lower = sample(1_000, 0);
        let upper = sample(2_000, 100);
        for aim in [-50, 0, 50, 100, 10_000] {
            for iteration in 0..4 {
                let offset = next_probe_offset(lower, upper, aim, iteration);
                assert!((1_001..2_000).contains(&offset), "aim {aim}: {offset}");
            }
        }
    }

    #[test]
    fn proportional_timestamp_inverts_proportional_offset() {
        assert_eq!(proportional_timestamp(1_000, 100, 200, 500), 150);
        assert_eq!(proportional_timestamp(1_000, 100, 200, 5_000), 200);
        assert_eq!(proportional_timestamp(0, 100, 200, 5), 100);
    }

    #[test]
    fn proportional_offset_maps_time_linearly_and_clamps() {
        assert_eq!(proportional_offset(1_000, 100, 200, 150), 500);
        assert_eq!(proportional_offset(1_000, 100, 200, 10_000), 1_000);
        assert_eq!(proportional_offset(1_000, 100, 200, -5), 0);
        assert_eq!(proportional_offset(1_000, 100, 100, 150), 0);
    }
}
