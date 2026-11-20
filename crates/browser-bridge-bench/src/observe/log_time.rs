//! Время строки журнала плеера: отметка, которую ставит сам плеер.
//!
//! Плеер пишет журнал через `tracing` в формате по умолчанию: строка начинается с отметки
//! UTC вида `2026-11-20T16:17:52.496257Z`. Судьи «кадры выросли за T» (сессия 16b3c2)
//! меряют время между двумя сводками по этим отметкам, а не по часам стенда: стенд опрашивает
//! файл раз в 200 мс и под нагрузкой параллельных строк может прочитать строку позже, а
//! отметка плеера от этого не сдвигается.

use std::time::Duration;

/// Момент строки журнала: время от начала эпохи Unix (UTC) по отметке плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlayerLogTime(Duration);

impl PlayerLogTime {
    /// Сколько прошло от `earlier` до этого момента; `None`, если `earlier` позже.
    pub fn since(self, earlier: PlayerLogTime) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }
}

/// Отметка в начале строки журнала плеера; `None`, если строка начинается не с неё
/// (строки `dbus-daemon` в том же файле, оборванная запись).
pub fn parse_log_time(line: &str) -> Option<PlayerLogTime> {
    let stamp = line.split_whitespace().next()?;
    let stamp = stamp.strip_suffix('Z')?;
    let (date, time) = stamp.split_once('T')?;
    let days = days_since_unix_epoch(date)?;
    let seconds_of_day = seconds_of_day(time)?;
    let whole_days = Duration::from_secs(days.checked_mul(SECONDS_PER_DAY)?);
    Some(PlayerLogTime(whole_days.checked_add(seconds_of_day)?))
}

/// Секунд в сутках (секунд координации отметка `tracing` не пишет).
const SECONDS_PER_DAY: u64 = 86_400;

/// `ГГГГ-ММ-ДД` → число суток от 1970-01-01. Раньше эпохи плеер не пишет, такие даты
/// отвергаются.
fn days_since_unix_epoch(date: &str) -> Option<u64> {
    let mut parts = date.split('-');
    let year: i64 = parse_digits(parts.next()?, 4)?;
    let month: i64 = parse_digits(parts.next()?, 2)?;
    let day: i64 = parse_digits(parts.next()?, 2)?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    u64::try_from(days_from_civil(year, month, day)).ok()
}

/// Число суток от 1970-01-01 для григорианской даты (алгоритм Говарда Хиннанта
/// `days_from_civil`: год считается с марта, чтобы високосный день был последним).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    // Январь и февраль относятся к прошлому «мартовскому» году.
    let year = if month <= 2 { year - 1 } else { year };
    // Эра — 400 лет: столько длится полный цикл високосных правил.
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    // Номер месяца от марта (март = 0) → день года от 1 марта.
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    // 719 468 — суток от 0000-03-01 до 1970-01-01.
    era * 146_097 + day_of_era - 719_468
}

/// `ЧЧ:ММ:СС[.доли]` → время от начала суток.
fn seconds_of_day(time: &str) -> Option<Duration> {
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let mut parts = clock.split(':');
    let hours: u64 = parse_digits(parts.next()?, 2)?;
    let minutes: u64 = parse_digits(parts.next()?, 2)?;
    let seconds: u64 = parse_digits(parts.next()?, 2)?;
    if parts.next().is_some() || hours > 23 || minutes > 59 || seconds > 60 {
        return None;
    }
    let whole = Duration::from_secs(hours * 3_600 + minutes * 60 + seconds);
    let nanos = match fraction {
        Some(fraction) => fraction_nanos(fraction)?,
        None => 0,
    };
    Some(whole + Duration::from_nanos(nanos))
}

/// Доли секунды (1–9 цифр) → наносекунды.
fn fraction_nanos(fraction: &str) -> Option<u64> {
    if fraction.is_empty()
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let value: u64 = fraction.parse().ok()?;
    // Дописываем нули справа до девяти знаков: «4962» = 496 200 000 нс.
    let missing_digits = u32::try_from(9 - fraction.len()).ok()?;
    value.checked_mul(10_u64.pow(missing_digits))
}

/// Ровно `width` десятичных цифр (без знака и пробелов) → число.
fn parse_digits<Number: std::str::FromStr>(text: &str, width: usize) -> Option<Number> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(line: &str) -> PlayerLogTime {
        parse_log_time(line).expect("a player log time")
    }

    #[test]
    fn the_unix_epoch_is_zero() {
        assert_eq!(
            time("1970-01-01T00:00:00Z  INFO x"),
            PlayerLogTime(Duration::ZERO)
        );
    }

    #[test]
    fn a_real_summary_line_gets_its_exact_moment() {
        // 2026-11-20 — 20 777 суток от эпохи (проверено `date -u -d 2026-11-20 +%s` / 86400).
        let line = "2026-11-20T16:17:52.496257Z  INFO Browser playback summary kind=periodic";
        let expected = Duration::from_secs(20_777 * 86_400 + 16 * 3_600 + 17 * 60 + 52)
            + Duration::from_micros(496_257);
        assert_eq!(time(line), PlayerLogTime(expected));
    }

    #[test]
    fn the_gap_between_two_lines_crosses_midnight_and_a_leap_day() {
        let before = time("2028-02-28T23:59:59.500Z  INFO a");
        let after = time("2028-02-29T00:00:00.250Z  INFO b");
        assert_eq!(after.since(before), Some(Duration::from_millis(750)));
        let march = time("2028-03-01T00:00:00Z  INFO c");
        assert_eq!(march.since(after), Some(Duration::from_millis(86_399_750)));
    }

    #[test]
    fn an_earlier_line_has_no_positive_gap() {
        let before = time("2026-11-20T16:17:52Z  INFO a");
        let after = time("2026-11-20T16:17:53Z  INFO b");
        assert_eq!(before.since(after), None);
    }

    #[test]
    fn lines_without_a_player_stamp_have_no_time() {
        for line in [
            "",
            "dbus-daemon[3118839]: [session uid=1000] Reloaded configuration",
            "2026-11-20T16:17:52.496257  INFO no zone",
            "2026-13-20T16:17:52Z  INFO month 13",
            "2026-11-20T24:00:00Z  INFO hour 24",
            "2026-11-20T16:17:52.Z  INFO empty fraction",
            "2026-11-20T16:17:52.1234567890Z  INFO ten digits",
            "26-11-20T16:17:52Z  INFO short year",
        ] {
            assert_eq!(parse_log_time(line), None, "{line:?}");
        }
    }
}
