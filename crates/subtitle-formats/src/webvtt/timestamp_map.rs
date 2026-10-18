//! Заголовок WebVTT `X-TIMESTAMP-MAP` (RFC 8216 §3.5): связь времени реплик сегмента HLS
//! с MPEG-TS временем сегмента.
//!
//! Формат: `X-TIMESTAMP-MAP=MPEGTS:<целое, 90 кГц, 33 бита>,LOCAL:<время WebVTT>`; параметры
//! в любом порядке, оба обязательны. Заголовок — строка между `WEBVTT` и первой пустой строкой.
//! Плохой заголовок НЕ фатален: он попадает в отчёт, значение отсутствует, реплики разбираются.
//! Что делать с этим значением (перевод на шкалу плеера) решает вызывающий код, не парсер.

use subtitle_core::SubtitleTime;

use super::blocks::NumberedLine;
use super::timestamp::{is_webvtt_space, parse_webvtt_timestamp};
use crate::report::LineNumber;

/// Имя строки заголовка.
const HEADER_NAME: &str = "X-TIMESTAMP-MAP=";
const MPEGTS_PARAM: &str = "MPEGTS";
const LOCAL_PARAM: &str = "LOCAL";
/// Разделитель параметров и разделитель имени и значения внутри параметра.
const PARAM_SEPARATOR: char = ',';
const NAME_VALUE_SEPARATOR: char = ':';
/// Разрядность счётчика MPEG-TS PTS/DTS (33 бита, затем переполнение к нулю).
const MPEGTS_TIMESTAMP_BITS: u32 = 33;

/// Значение MPEG-TS PTS в тиках 90 кГц; гарантированно помещается в 33 бита.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MpegTsTimestamp90kHz(u64);

/// Число не помещается в 33 бита (`> 2^33 - 1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("MPEG-TS timestamp {ticks} does not fit into 33 bits")]
pub struct MpegTsTimestampOutOfRange {
    ticks: u64,
}

impl MpegTsTimestamp90kHz {
    /// Частота тактов, Гц.
    pub const TICKS_PER_SECOND: u64 = 90_000;
    /// Модуль счётчика: значения «заворачиваются» через `2^33` тиков (~26,5 ч).
    pub const MODULUS: u64 = 1 << MPEGTS_TIMESTAMP_BITS;

    /// Создаёт значение; `ticks >= MODULUS` — ошибка.
    pub const fn new(ticks: u64) -> Result<Self, MpegTsTimestampOutOfRange> {
        if ticks >= Self::MODULUS {
            return Err(MpegTsTimestampOutOfRange { ticks });
        }
        Ok(Self(ticks))
    }

    /// Тики 90 кГц (меньше [`Self::MODULUS`]).
    pub const fn ticks(self) -> u64 {
        self.0
    }

    /// Значение в микросекундах (округление вниз; помещается в i64: 2^33·10^6/9·10^4 < 2^44).
    pub fn as_micros(self) -> i64 {
        // `self.0 < 2^33`, умножение на 10^6 не переполняет u64, результат < 2^44.
        i64::try_from(self.0 * 1_000_000 / Self::TICKS_PER_SECOND).unwrap_or(i64::MAX)
    }
}

/// Разобранный `X-TIMESTAMP-MAP`: момент `local` времени реплик соответствует `mpegts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebVttTimestampMap {
    pub mpegts: MpegTsTimestamp90kHz,
    pub local: SubtitleTime,
}

/// Почему заголовок отвергнут.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimestampMapError {
    /// Параметр без `имя:значение`, повтор параметра или нет `MPEGTS`/`LOCAL`.
    Malformed,
    /// `MPEGTS` не целое число или не помещается в 33 бита; `LOCAL` не время WebVTT.
    InvalidValue,
}

/// Итог просмотра заголовка файла.
#[derive(Debug, Default)]
pub(super) struct HeaderTimestampMap {
    /// Первое корректное значение.
    pub(super) map: Option<WebVttTimestampMap>,
    /// Некорректные (и повторные) строки `X-TIMESTAMP-MAP` с номерами для отчёта.
    pub(super) rejected_lines: Vec<LineNumber>,
}

/// Просматривает строки заголовка: первая корректная строка задаёт значение, плохие и
/// повторные попадают в `rejected_lines`. Остальные строки заголовка (например, `Kind:`) игнорируются.
pub(super) fn scan_header_for_timestamp_map(
    header_lines: &[NumberedLine<'_>],
) -> HeaderTimestampMap {
    let mut result = HeaderTimestampMap::default();
    for line in header_lines {
        let Some(value) = line.text.strip_prefix(HEADER_NAME) else {
            continue;
        };
        match parse_timestamp_map_value(value) {
            Ok(map) if result.map.is_none() => result.map = Some(map),
            // Повторная строка неоднозначна: первая побеждает, вторая — проблема в отчёте.
            Ok(_) | Err(_) => result.rejected_lines.push(line.number),
        }
    }
    result
}

/// Разбирает значение после `X-TIMESTAMP-MAP=`.
pub(super) fn parse_timestamp_map_value(
    value: &str,
) -> Result<WebVttTimestampMap, TimestampMapError> {
    let mut mpegts = None;
    let mut local = None;
    for parameter in value.split(PARAM_SEPARATOR) {
        let (name, parameter_value) = parameter
            .split_once(NAME_VALUE_SEPARATOR)
            .ok_or(TimestampMapError::Malformed)?;
        let name = name.trim_matches(is_webvtt_space);
        let parameter_value = parameter_value.trim_matches(is_webvtt_space);
        let slot = match name {
            MPEGTS_PARAM => &mut mpegts,
            LOCAL_PARAM => &mut local,
            // Неизвестные параметры игнорируем: будущие расширения не должны ломать карту.
            _ => continue,
        };
        if slot.replace(parameter_value).is_some() {
            return Err(TimestampMapError::Malformed);
        }
    }
    let (Some(mpegts_text), Some(local_text)) = (mpegts, local) else {
        return Err(TimestampMapError::Malformed);
    };
    Ok(WebVttTimestampMap {
        mpegts: parse_mpegts_ticks(mpegts_text)?,
        local: parse_webvtt_timestamp(local_text).ok_or(TimestampMapError::InvalidValue)?,
    })
}

/// Десятичное целое без знака и пробелов; значение вне 33 бит отвергается.
fn parse_mpegts_ticks(text: &str) -> Result<MpegTsTimestamp90kHz, TimestampMapError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TimestampMapError::InvalidValue);
    }
    let ticks: u64 = text.parse().map_err(|_| TimestampMapError::InvalidValue)?;
    MpegTsTimestamp90kHz::new(ticks).map_err(|_| TimestampMapError::InvalidValue)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> Result<WebVttTimestampMap, TimestampMapError> {
        parse_timestamp_map_value(value)
    }

    #[test]
    fn both_parameter_orders_give_the_same_map() {
        let expected = WebVttTimestampMap {
            mpegts: MpegTsTimestamp90kHz::new(900_000).unwrap(),
            local: SubtitleTime::from_millis(1_500),
        };
        assert_eq!(parse("MPEGTS:900000,LOCAL:00:00:01.500"), Ok(expected));
        assert_eq!(parse("LOCAL:00:01.500,MPEGTS:900000"), Ok(expected));
    }

    #[test]
    fn boundary_values_of_33_bits() {
        let max = (1u64 << 33) - 1;
        assert_eq!(
            parse(&format!("MPEGTS:{max},LOCAL:00:00.000"))
                .unwrap()
                .mpegts
                .ticks(),
            max
        );
        assert_eq!(
            parse(&format!("MPEGTS:{},LOCAL:00:00.000", max + 1)),
            Err(TimestampMapError::InvalidValue)
        );
        assert_eq!(
            parse("MPEGTS:99999999999999999999999,LOCAL:00:00.000"),
            Err(TimestampMapError::InvalidValue)
        );
    }

    #[test]
    fn malformed_values_are_rejected() {
        assert_eq!(parse("MPEGTS:900000"), Err(TimestampMapError::Malformed));
        assert_eq!(parse("LOCAL:00:00.000"), Err(TimestampMapError::Malformed));
        assert_eq!(parse("garbage"), Err(TimestampMapError::Malformed));
        assert_eq!(
            parse("MPEGTS:1,MPEGTS:2,LOCAL:00:00.000"),
            Err(TimestampMapError::Malformed)
        );
        assert_eq!(
            parse("MPEGTS:-5,LOCAL:00:00.000"),
            Err(TimestampMapError::InvalidValue)
        );
        assert_eq!(
            parse("MPEGTS:1.5,LOCAL:00:00.000"),
            Err(TimestampMapError::InvalidValue)
        );
        assert_eq!(
            parse("MPEGTS:1,LOCAL:soon"),
            Err(TimestampMapError::InvalidValue)
        );
    }

    #[test]
    fn mpegts_converts_to_microseconds() {
        assert_eq!(
            MpegTsTimestamp90kHz::new(900_000).unwrap().as_micros(),
            10_000_000
        );
        assert_eq!(
            MpegTsTimestamp90kHz::new(MpegTsTimestamp90kHz::MODULUS - 1)
                .unwrap()
                .as_micros(),
            95_443_717_677
        );
        assert!(MpegTsTimestamp90kHz::new(MpegTsTimestamp90kHz::MODULUS).is_err());
    }
}
