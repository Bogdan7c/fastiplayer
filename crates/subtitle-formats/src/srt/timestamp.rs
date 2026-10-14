//! Время SRT: `HH:MM:SS,mmm` и толерантные варианты, а также строка `start --> end`.
//!
//! WebVTT имеет собственную, более строгую грамматику (`[hh:]mm:ss.ttt`) и разбирается отдельно.

use subtitle_core::SubtitleTime;

/// Разделитель начала и конца в строке времени.
const TIMING_ARROW: &str = "-->";
/// Максимум цифр в часах: 4 цифры (до 9999 ч) не переполняют микросекунды в `i64`.
const MAX_HOUR_DIGITS: usize = 4;
/// Максимум цифр в минутах и секундах.
const MAX_MINUTE_OR_SECOND_DIGITS: usize = 2;
/// Максимум цифр дробной части (миллисекунды).
const MAX_FRACTION_DIGITS: usize = 3;
/// Минут в часе и секунд в минуте: верхняя (исключительная) граница этих полей.
const SEXAGESIMAL_LIMIT: u64 = 60;
const MICROS_PER_MILLI: i64 = 1_000;
const MILLIS_PER_SECOND: i64 = 1_000;
const SECONDS_PER_MINUTE: i64 = 60;
const MINUTES_PER_HOUR: i64 = 60;

/// Почему строка времени не разобрана.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimingLineError {
    /// Нет разделителя `-->`.
    MissingArrow,
    /// Хотя бы одно из времён некорректно.
    InvalidTimestamp,
}

/// Разбирает одно SRT-время; `None` — формат нарушен.
///
/// Допускается: запятая или точка перед долями, пробелы вокруг разделителей,
/// 1–2 цифры в часах/минутах/секундах, отсутствие часов (`MM:SS,mmm`).
/// Доли — это десятичная дробь секунды: `,5` = 500 мс, `,05` = 50 мс, `,005` = 5 мс.
/// Минуты и секунды обязаны быть меньше 60.
pub(crate) fn parse_srt_timestamp(text: &str) -> Option<SubtitleTime> {
    // Пробелы внутри времени (`00 : 00 : 01 , 000`) встречаются в кривых редакторах — убираем все.
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    let (clock, fraction) = compact.rsplit_once([',', '.'])?;
    let fields: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [minutes, seconds] => (
            0,
            parse_digits(minutes, MAX_MINUTE_OR_SECOND_DIGITS)?,
            parse_digits(seconds, MAX_MINUTE_OR_SECOND_DIGITS)?,
        ),
        [hours, minutes, seconds] => (
            parse_digits(hours, MAX_HOUR_DIGITS)?,
            parse_digits(minutes, MAX_MINUTE_OR_SECOND_DIGITS)?,
            parse_digits(seconds, MAX_MINUTE_OR_SECOND_DIGITS)?,
        ),
        _ => return None,
    };
    if minutes >= SEXAGESIMAL_LIMIT || seconds >= SEXAGESIMAL_LIMIT {
        return None;
    }
    let millis = parse_fraction_as_millis(fraction)?;
    let total_seconds = (i64::try_from(hours).ok()? * MINUTES_PER_HOUR
        + i64::try_from(minutes).ok()?)
        * SECONDS_PER_MINUTE
        + i64::try_from(seconds).ok()?;
    let total_millis = total_seconds * MILLIS_PER_SECOND + millis;
    Some(SubtitleTime::from_micros(total_millis * MICROS_PER_MILLI))
}

/// Только ASCII-цифры, длина `1..=max_digits`.
fn parse_digits(field: &str, max_digits: usize) -> Option<u64> {
    if field.is_empty() || field.len() > max_digits || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

/// 1–3 цифры доли секунды -> миллисекунды (дополнение нулями справа).
fn parse_fraction_as_millis(fraction: &str) -> Option<i64> {
    let digits = parse_digits(fraction, MAX_FRACTION_DIGITS)?;
    let missing_digits = u32::try_from(MAX_FRACTION_DIGITS - fraction.len()).ok()?;
    i64::try_from(digits)
        .ok()?
        .checked_mul(10_i64.pow(missing_digits))
}

/// Разбирает строку `start --> end`; хвост после конца (координаты `X1:..`) игнорируется.
///
/// Порядок времён здесь НЕ проверяется (`end > start` проверяет конструктор события).
pub(crate) fn parse_timing_line(
    line: &str,
) -> Result<(SubtitleTime, SubtitleTime), TimingLineError> {
    let (start_text, end_and_tail) = line
        .split_once(TIMING_ARROW)
        .ok_or(TimingLineError::MissingArrow)?;
    let start = parse_srt_timestamp(start_text).ok_or(TimingLineError::InvalidTimestamp)?;
    // Сначала пробуем весь остаток (допускает пробелы внутри времени), затем только первое слово
    // (отбрасывает хвост вида `X1:100 X2:200`).
    let end = parse_srt_timestamp(end_and_tail)
        .or_else(|| {
            end_and_tail
                .split_whitespace()
                .next()
                .and_then(parse_srt_timestamp)
        })
        .ok_or(TimingLineError::InvalidTimestamp)?;
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros(text: &str) -> Option<i64> {
        parse_srt_timestamp(text).map(SubtitleTime::as_micros)
    }

    #[test]
    fn comma_and_dot_separators_give_same_time() {
        assert_eq!(micros("01:02:03,456"), Some(3_723_456_000));
        assert_eq!(micros("01:02:03.456"), Some(3_723_456_000));
    }

    #[test]
    fn spaces_around_separators_are_tolerated() {
        assert_eq!(micros("  00 : 00 : 01 , 500  "), Some(1_500_000));
    }

    #[test]
    fn missing_hours_means_zero_hours() {
        assert_eq!(micros("02:03,004"), Some(123_004_000));
    }

    #[test]
    fn one_and_two_digit_fields_are_accepted() {
        assert_eq!(micros("1:2:3,004"), Some(3_723_004_000));
    }

    #[test]
    fn fraction_digits_are_decimal_fraction_of_second() {
        assert_eq!(micros("00:00:01,5"), Some(1_500_000));
        assert_eq!(micros("00:00:01,05"), Some(1_050_000));
        assert_eq!(micros("00:00:01,005"), Some(1_005_000));
    }

    #[test]
    fn out_of_range_minutes_and_seconds_are_rejected() {
        assert_eq!(micros("00:60:00,000"), None);
        assert_eq!(micros("00:00:60,000"), None);
        assert_eq!(micros("00:59:59,999"), Some(3_599_999_000));
    }

    #[test]
    fn malformed_timestamps_are_rejected() {
        for bad in [
            "",
            "00:00:01",
            "00:00:01,",
            "00:00:01,1234",
            "aa:00:01,000",
            "-1:00:00,000",
            "1,000",
            "00:00:00:01,000",
            "00:00:+1,000",
        ] {
            assert_eq!(micros(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn timing_line_parses_both_times_and_ignores_coordinates_tail() {
        let (start, end) =
            parse_timing_line("00:00:01,000 --> 00:00:02,500 X1:100 X2:200").expect("valid line");
        assert_eq!(start.as_micros(), 1_000_000);
        assert_eq!(end.as_micros(), 2_500_000);
    }

    #[test]
    fn timing_line_errors_are_distinguished() {
        assert_eq!(
            parse_timing_line("just text"),
            Err(TimingLineError::MissingArrow)
        );
        assert_eq!(
            parse_timing_line("00:00:01,000 --> soon"),
            Err(TimingLineError::InvalidTimestamp)
        );
        assert_eq!(
            parse_timing_line("oops --> 00:00:02,000"),
            Err(TimingLineError::InvalidTimestamp)
        );
    }
}
