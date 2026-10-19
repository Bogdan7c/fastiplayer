//! Длительность контейнера для больших seekable файлов, не влезших в начальный индекс.
//!
//! Начальный индекс сканирует ограниченное окно (`seek_scan_packets`, ~6 МБ). Если
//! файл больше, до EOF мы не доходим и честной длительности по головной части нет;
//! без неё player-core не строит таймлайн и отказывает в seek-е. Здесь длительность
//! восстанавливается без полного прохода:
//!
//! 1. читается хвост файла (`tail_probe_bytes`) и берётся максимальный PTS PES-заголовков
//!    известных elementary PID — это та же семантика, что у длительности при EOF
//!    (сырое время контейнера, включая стартовый сдвиг записи эфира);
//! 2. хвост принимается, только если согласован с головой (та же запись): его PTS
//!    позже головного, а средний битрейт всего файла правдоподобен относительно
//!    битрейта головы (`TAIL_BITRATE_TOLERANCE`);
//! 3. иначе (склейка записей с перезапуском PTS, мусор в хвосте) — оценка по размеру,
//!    как у ffmpeg: старт + размер файла / средний битрейт головы;
//! 4. если у головы нет диапазона PTS, длительность остаётся неизвестной.
//!
//! Модуль не владеет состоянием demuxer-а: он читает только `TransportPacketReader`
//! и возвращает значение; возврат reader-а на позицию playback делает вызывающий
//! код через `restore_after_index_scan`.

use std::collections::HashSet;
use std::time::Duration;

use crate::MpegTsDemuxError;
use crate::framing::{TS_PACKET_BYTES, TransportPacketReader, TransportReadOutcome};
use crate::pes::peek_pes_pts;
use crate::timestamps::TimestampUnwrapper;

/// Тактов PTS в секунде (90 кГц).
const TICKS_PER_SECOND: i128 = 90_000;
/// Наносекунд в секунде.
const NANOS_PER_SECOND: i128 = 1_000_000_000;
/// Допуск согласованности хвоста с головой выражен дробью 3/2: средний битрейт
/// файла, выведенный из хвостового PTS, должен лежать в `[голова * 2/3, голова * 3/2]`.
/// Верхняя граница намеренно строго ниже 2: склейка двух одинаковых записей даёт
/// ровно двукратный «битрейт» и обязана отвергаться. Цена: сильно неравномерный VBR
/// (тяжёлая голова, лёгкий остаток) уйдёт в оценку по размеру — она грубее, но
/// не хуже, чем была бы без пробы (раньше длительности не было вовсе).
const TAIL_BITRATE_TOLERANCE_NUMERATOR: u128 = 3;
/// Знаменатель дроби допуска (см. `TAIL_BITRATE_TOLERANCE_NUMERATOR`).
const TAIL_BITRATE_TOLERANCE_DENOMINATOR: u128 = 2;

/// Что известно о головной части файла после bounded-скана (в её шкале презентации).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadScanEvidence {
    /// Самый ранний PTS головы (старт контейнера).
    pub(crate) start: Duration,
    /// Самый поздний PTS головы (конец последнего просканированного кадра).
    pub(crate) end: Duration,
    /// Сколько байт файла покрыла голова (от начала playback до конца скана).
    pub(crate) scanned_bytes: u64,
}

/// Итог оценки; вариант говорит, насколько ему можно доверять.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerDurationEstimate {
    /// Максимальный PTS хвоста, согласованный с головой: точная длительность.
    FromTailTimestamps(Duration),
    /// Хвост не годится: оценка «размер файла / средний битрейт головы».
    FromByteRate(Duration),
    /// Данных для оценки нет (нет диапазона PTS у головы или размер файла неизвестен).
    Unknown,
}

impl ContainerDurationEstimate {
    /// Значение для `Demuxer::duration()`; `None` — длительность неизвестна.
    pub(crate) const fn duration(self) -> Option<Duration> {
        match self {
            Self::FromTailTimestamps(duration) | Self::FromByteRate(duration) => Some(duration),
            Self::Unknown => None,
        }
    }
}

/// Параметры чтения хвоста.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TailScanRequest<'a> {
    /// PID-ы elementary-потоков выбранной программы (из PMT).
    pub(crate) elementary_pids: &'a HashSet<u16>,
    /// Сколько байт с конца файла читать.
    pub(crate) tail_bytes: usize,
    /// Опорный PTS для разворачивания 33-битного счётчика (конец головы).
    pub(crate) unwrap_reference: Duration,
}

/// Читает хвост файла и возвращает максимальный PTS его PES-заголовков.
///
/// Только bounded-чтение: не больше `tail_bytes` байт. Reader остаётся в хвосте —
/// вызывающий обязан вернуть его через `restore_after_index_scan`. Мусор/обрыв
/// в хвосте не ошибка (`Ok(None)` или то, что успели собрать); наружу выходит
/// только `Cancelled`.
pub(crate) fn scan_tail_max_pts(
    reader: &mut TransportPacketReader,
    content_length: u64,
    request: TailScanRequest<'_>,
) -> Result<Option<Duration>, MpegTsDemuxError> {
    let tail_start = content_length.saturating_sub(request.tail_bytes as u64);
    match reader.begin_index_scan(tail_start) {
        Ok(()) => {}
        Err(MpegTsDemuxError::Cancelled) => return Err(MpegTsDemuxError::Cancelled),
        Err(_) => return Ok(None),
    }
    // Старт хвоста не выровнен по 188 байтам: reader сам ищет синхронизацию,
    // требуя байт 0x47 на двух соседних пакетах подряд.
    let packet_budget = request.tail_bytes / TS_PACKET_BYTES + 1;
    let mut unwrapper =
        TimestampUnwrapper::from_unwrapped_reference(duration_to_ticks(request.unwrap_reference));
    let mut max_ticks: Option<i64> = None;
    for _ in 0..packet_budget {
        match reader.next_packet() {
            Ok(TransportReadOutcome::Packet(packet)) => {
                let carries_pes_start = packet.payload_unit_start
                    && !packet.transport_error
                    && packet.scrambling == 0
                    && request.elementary_pids.contains(&packet.pid);
                if !carries_pes_start {
                    continue;
                }
                if let Some(raw_pts) = peek_pes_pts(&packet.payload) {
                    let ticks = unwrapper.unwrap(raw_pts);
                    max_ticks = Some(max_ticks.map_or(ticks, |current| current.max(ticks)));
                }
            }
            Ok(TransportReadOutcome::EndResource(_) | TransportReadOutcome::EndOfInput) => break,
            Err(MpegTsDemuxError::Cancelled) => return Err(MpegTsDemuxError::Cancelled),
            // Битый хвост: берём то, что уже прочитано; согласованность проверит estimate.
            Err(_) => break,
        }
    }
    Ok(max_ticks.and_then(ticks_to_duration))
}

/// Выбирает длительность по правилам из документации модуля.
///
/// `content_length` — полный размер файла; `tail_end` — результат
/// [`scan_tail_max_pts`] (`None` — в хвосте нет PTS).
pub(crate) fn estimate_container_duration(
    head: HeadScanEvidence,
    content_length: Option<u64>,
    tail_end: Option<Duration>,
) -> ContainerDurationEstimate {
    let Some(content_length) = content_length else {
        return ContainerDurationEstimate::Unknown;
    };
    let head_span = head.end.saturating_sub(head.start).as_nanos();
    if head_span == 0 || head.scanned_bytes == 0 {
        // Скорость потока неизвестна: ни проверить хвост, ни оценить по размеру.
        return ContainerDurationEstimate::Unknown;
    }
    let file_bytes = u128::from(content_length);
    let head_bytes = u128::from(head.scanned_bytes);

    if let Some(tail_end) = tail_end
        && tail_end > head.end
        && tail_is_consistent_with_head(head_span, head_bytes, file_bytes, tail_end - head.start)
    {
        return ContainerDurationEstimate::FromTailTimestamps(tail_end);
    }

    // Размер / битрейт головы: file_bytes / (head_bytes / head_span).
    let span_by_size = file_bytes.saturating_mul(head_span) / head_bytes;
    match u64::try_from(span_by_size) {
        Ok(nanos) => ContainerDurationEstimate::FromByteRate(
            (head.start + Duration::from_nanos(nanos)).max(head.end),
        ),
        Err(_) => ContainerDurationEstimate::Unknown,
    }
}

/// Хвост согласован с головой: средний битрейт файла по хвостовому PTS
/// (`file_bytes / tail_span`) отличается от битрейта головы (`head_bytes / head_span`)
/// в пределах дроби `TAIL_BITRATE_TOLERANCE_*` (3/2). Сравнение без деления:
/// перемножением обеих дробей крест-накрест.
fn tail_is_consistent_with_head(
    head_span_nanos: u128,
    head_bytes: u128,
    file_bytes: u128,
    tail_span: Duration,
) -> bool {
    let tail_span_nanos = tail_span.as_nanos();
    let file_side = file_bytes.saturating_mul(head_span_nanos);
    let head_side = head_bytes.saturating_mul(tail_span_nanos);
    let file_side_scaled = file_side.saturating_mul(TAIL_BITRATE_TOLERANCE_NUMERATOR);
    let head_side_scaled = head_side.saturating_mul(TAIL_BITRATE_TOLERANCE_NUMERATOR);
    // file/tail >= head / (3/2)  и  file/tail <= head * (3/2).
    file_side_scaled >= head_side.saturating_mul(TAIL_BITRATE_TOLERANCE_DENOMINATOR)
        && file_side.saturating_mul(TAIL_BITRATE_TOLERANCE_DENOMINATOR) <= head_side_scaled
}

/// Время → такты 90 кГц (с округлением вниз).
fn duration_to_ticks(duration: Duration) -> i64 {
    let ticks = i128::try_from(duration.as_nanos()).unwrap_or(i128::MAX) * TICKS_PER_SECOND
        / NANOS_PER_SECOND;
    i64::try_from(ticks).unwrap_or(i64::MAX)
}

/// Такты 90 кГц → время; отрицательные такты не имеют длительности.
fn ticks_to_duration(ticks: i64) -> Option<Duration> {
    let nanos = u64::try_from(i128::from(ticks) * NANOS_PER_SECOND / TICKS_PER_SECOND).ok()?;
    Some(Duration::from_nanos(nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> HeadScanEvidence {
        // 10 с головы на 10 МБ => 1 МБ/с.
        HeadScanEvidence {
            start: Duration::from_millis(1_400),
            end: Duration::from_millis(11_400),
            scanned_bytes: 10_000_000,
        }
    }

    #[test]
    fn consistent_tail_is_exact() {
        let estimate =
            estimate_container_duration(head(), Some(60_000_000), Some(Duration::from_secs(61)));
        assert_eq!(
            estimate,
            ContainerDurationEstimate::FromTailTimestamps(Duration::from_secs(61))
        );
    }

    #[test]
    fn restarted_tail_falls_back_to_byte_rate() {
        // Хвост «перезапустил» PTS: меньше головы.
        let estimate =
            estimate_container_duration(head(), Some(60_000_000), Some(Duration::from_secs(5)));
        // 1.4 с + 60 МБ / (1 МБ/с) = 61.4 с.
        assert_eq!(
            estimate,
            ContainerDurationEstimate::FromByteRate(Duration::from_millis(61_400))
        );
    }

    #[test]
    fn implausible_tail_bitrate_falls_back_to_byte_rate() {
        // Хвостовой PTS в 10 раз дальше, чем позволяет размер файла.
        let estimate =
            estimate_container_duration(head(), Some(60_000_000), Some(Duration::from_secs(600)));
        assert!(matches!(
            estimate,
            ContainerDurationEstimate::FromByteRate(_)
        ));
    }

    #[test]
    fn missing_tail_pts_uses_byte_rate_and_missing_span_is_unknown() {
        assert_eq!(
            estimate_container_duration(head(), Some(30_000_000), None),
            ContainerDurationEstimate::FromByteRate(Duration::from_millis(31_400))
        );
        let no_span = HeadScanEvidence {
            end: head().start,
            ..head()
        };
        assert_eq!(
            estimate_container_duration(no_span, Some(30_000_000), Some(Duration::from_secs(40))),
            ContainerDurationEstimate::Unknown
        );
        assert_eq!(
            estimate_container_duration(head(), None, Some(Duration::from_secs(40))),
            ContainerDurationEstimate::Unknown
        );
    }

    #[test]
    fn tick_conversion_round_trips_and_rejects_negative() {
        assert_eq!(duration_to_ticks(Duration::from_secs(2)), 180_000);
        assert_eq!(ticks_to_duration(180_000), Some(Duration::from_secs(2)));
        assert_eq!(ticks_to_duration(-1), None);
    }
}
