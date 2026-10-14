//! Время WebVTT: строгая грамматика `[hh:]mm:ss.ttt` и строка `start --> end [settings]`.
//!
//! В отличие от SRT здесь нет толерантности: ровно две цифры минут и секунд, ровно три
//! цифры долей и только точка. Так требует W3C WebVTT; нарушение делает реплику битой.

use subtitle_core::SubtitleTime;

/// Разделитель начала и конца в строке времени.
pub(super) const TIMING_ARROW: &str = "-->";
/// Минимум цифр в часах по грамматике WebVTT («две или более»).
const MIN_HOUR_DIGITS: usize = 2;
/// Максимум цифр в часах: 6 цифр (до 999999 ч) гарантированно не переполняют микросекунды `i64`.
const MAX_HOUR_DIGITS: usize = 6;
/// Минуты и секунды всегда занимают ровно две цифры.
const SEXAGESIMAL_DIGITS: usize = 2;
/// Доли секунды всегда занимают ровно три цифры (миллисекунды).
const FRACTION_DIGITS: usize = 3;
/// Верхняя (исключительная) граница минут и секунд.
const SEXAGESIMAL_LIMIT: u64 = 60;
const MICROS_PER_MILLI: i64 = 1_000;
const MILLIS_PER_SECOND: i64 = 1_000;
const SECONDS_PER_MINUTE: i64 = 60;
const MINUTES_PER_HOUR: i64 = 60;

/// Почему строка времени реплики не разобрана.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimingLineError {
    /// Нет разделителя `-->`.
    MissingArrow,
    /// Хотя бы одно из времён нарушает грамматику.
    InvalidTimestamp,
}

/// Разобранная строка времени: два момента и «хвост» настроек реплики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimingLine<'line> {
    pub(super) start: SubtitleTime,
    pub(super) end: SubtitleTime,
    /// Текст после конечного времени (настройки через пробелы); может быть пустым.
    pub(super) settings: &'line str,
}

/// Разбирает одно время `[hh:]mm:ss.ttt`; `None` — грамматика нарушена.
pub(super) fn parse_webvtt_timestamp(text: &str) -> Option<SubtitleTime> {
    let (clock, fraction) = text.split_once('.')?;
    let fields: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [minutes, seconds] => (
            0,
            parse_fixed_digits(minutes, SEXAGESIMAL_DIGITS)?,
            parse_fixed_digits(seconds, SEXAGESIMAL_DIGITS)?,
        ),
        [hours, minutes, seconds] => (
            parse_hours(hours)?,
            parse_fixed_digits(minutes, SEXAGESIMAL_DIGITS)?,
            parse_fixed_digits(seconds, SEXAGESIMAL_DIGITS)?,
        ),
        _ => return None,
    };
    if minutes >= SEXAGESIMAL_LIMIT || seconds >= SEXAGESIMAL_LIMIT {
        return None;
    }
    let millis = parse_fixed_digits(fraction, FRACTION_DIGITS)?;
    let total_seconds = (i64::try_from(hours).ok()? * MINUTES_PER_HOUR
        + i64::try_from(minutes).ok()?)
        * SECONDS_PER_MINUTE
        + i64::try_from(seconds).ok()?;
    let total_millis = total_seconds * MILLIS_PER_SECOND + i64::try_from(millis).ok()?;
    Some(SubtitleTime::from_micros(total_millis * MICROS_PER_MILLI))
}

/// Разбирает `start --> end [settings]`.
///
/// Пробелы вокруг стрелки необязательны (так делают браузеры); конечное время — первое
/// «слово» после стрелки, всё остальное уходит в `settings`. Порядок времён не проверяется.
pub(super) fn parse_timing_line(line: &str) -> Result<TimingLine<'_>, TimingLineError> {
    let (start_text, after_arrow) = line
        .split_once(TIMING_ARROW)
        .ok_or(TimingLineError::MissingArrow)?;
    let start = parse_webvtt_timestamp(start_text.trim_matches(is_webvtt_space))
        .ok_or(TimingLineError::InvalidTimestamp)?;
    let after_arrow = after_arrow.trim_start_matches(is_webvtt_space);
    let end_length = after_arrow
        .find(is_webvtt_space)
        .unwrap_or(after_arrow.len());
    let (end_text, settings) = after_arrow.split_at(end_length);
    let end = parse_webvtt_timestamp(end_text).ok_or(TimingLineError::InvalidTimestamp)?;
    Ok(TimingLine {
        start,
        end,
        settings: settings.trim_matches(is_webvtt_space),
    })
}

/// Разделители WebVTT внутри строки: только пробел и табуляция.
pub(super) fn is_webvtt_space(ch: char) -> bool {
    ch == ' ' || ch == '\t'
}

/// Часы: две и более цифры, не больше [`MAX_HOUR_DIGITS`].
fn parse_hours(field: &str) -> Option<u64> {
    if field.len() < MIN_HOUR_DIGITS || field.len() > MAX_HOUR_DIGITS {
        return None;
    }
    parse_ascii_digits(field)
}

/// Поле ровно из `digits` ASCII-цифр.
fn parse_fixed_digits(field: &str, digits: usize) -> Option<u64> {
    if field.len() != digits {
        return None;
    }
    parse_ascii_digits(field)
}

fn parse_ascii_digits(field: &str) -> Option<u64> {
    if field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros(text: &str) -> Option<i64> {
        parse_webvtt_timestamp(text).map(SubtitleTime::as_micros)
    }

    #[test]
    fn full_and_short_forms_give_the_same_instant() {
        assert_eq!(micros("00:01:02.345"), Some(62_345_000));
        assert_eq!(micros("01:02.345"), Some(62_345_000));
        assert_eq!(micros("100:00:00.000"), Some(360_000_000_000));
    }

    #[test]
    fn grammar_violations_are_rejected() {
        for bad in [
            "00:01:02,345",  // запятая вместо точки
            "00:01:02.34",   // две цифры долей
            "00:01:02.3456", // четыре цифры долей
            "1:02.345",      // одна цифра минут
            "00:1:02.345",   // одна цифра минут при часах
            "00:01:2.345",   // одна цифра секунд
            "0:00:01.000",   // одна цифра часов
            "00:60:00.000",  // минуты >= 60
            "00:00:60.000",  // секунды >= 60
            "00:01:02",      // нет долей
            "01.345",        // нет минут
            "a0:01.000",
            "",
        ] {
            assert_eq!(micros(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn timing_line_splits_end_and_settings() {
        let line = parse_timing_line("00:01.000 --> 00:02.500  align:start\tline:0").unwrap();
        assert_eq!(line.start, SubtitleTime::from_millis(1_000));
        assert_eq!(line.end, SubtitleTime::from_millis(2_500));
        assert_eq!(line.settings, "align:start\tline:0");
    }

    #[test]
    fn timing_line_without_spaces_around_arrow_is_accepted() {
        let line = parse_timing_line("00:01.000-->00:02.000").unwrap();
        assert_eq!(line.end, SubtitleTime::from_millis(2_000));
        assert_eq!(line.settings, "");
    }

    #[test]
    fn timing_line_errors_are_distinguished() {
        assert_eq!(
            parse_timing_line("00:01.000 00:02.000"),
            Err(TimingLineError::MissingArrow)
        );
        assert_eq!(
            parse_timing_line("00:01,000 --> 00:02.000"),
            Err(TimingLineError::InvalidTimestamp)
        );
        assert_eq!(
            parse_timing_line("00:01.000 --> soon"),
            Err(TimingLineError::InvalidTimestamp)
        );
    }
}
