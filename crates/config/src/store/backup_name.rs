//! Имя резервной копии повреждённого config-файла.
//!
//! Решение владельца (сессия 05): имя читается человеком —
//! `config.toml.broken-2026-10-05_14-30-12.bak`. Время в UTC: местный часовой пояс
//! в многопоточной программе на Linux надёжно не узнать (чтение `TZ`/`localtime`
//! гоняется с `setenv`). Копии хранятся все — они крошечные.
//!
//! Перевод секунд в дату сделан здесь, без crate `time`: подключение `time` к `config`
//! добавляло вторую реализацию `PartialEq` для `std::time::Duration` во все crate-ы,
//! зависящие от config, и ломало вывод типов в их тестах (`assert_eq!(durations, [])`).

use std::time::{SystemTime, UNIX_EPOCH};

/// Секунд в сутках.
const SECONDS_PER_DAY: u64 = 86_400;
/// Секунд в часе.
const SECONDS_PER_HOUR: u64 = 3_600;
/// Секунд в минуте.
const SECONDS_PER_MINUTE: u64 = 60;

/// Генератор вариантов имени резервной копии для одного момента времени.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BrokenConfigBackupName {
    /// Общая часть имени без суффикса попытки и расширения:
    /// `config.toml.broken-2026-10-05_14-30-12`.
    stem: String,
}

impl BrokenConfigBackupName {
    /// Готовит имя для файла `config_file_name`, повреждение которого найдено в `now`.
    pub(super) fn new(config_file_name: &str, now: SystemTime) -> Self {
        Self {
            stem: format!("{config_file_name}.broken-{}", utc_timestamp_label(now)),
        }
    }

    /// Вариант имени для попытки `attempt` (с 1).
    ///
    /// Первая попытка — без суффикса; если имя занято (два сбоя за одну секунду),
    /// дальше идут `-2`, `-3`…
    pub(super) fn candidate(&self, attempt: u32) -> String {
        if attempt <= 1 {
            format!("{}.bak", self.stem)
        } else {
            format!("{}-{attempt}.bak", self.stem)
        }
    }
}

/// `2026-10-05_14-30-12` в UTC; без двоеточий, чтобы имя было допустимым везде.
///
/// Часы до 1970 года дают `unknown-time`: имя всё равно уникализируется суффиксом
/// попытки, а данные пользователя сохраняются.
fn utc_timestamp_label(now: SystemTime) -> String {
    let Ok(since_epoch) = now.duration_since(UNIX_EPOCH) else {
        return "unknown-time".to_string();
    };
    let total_seconds = since_epoch.as_secs();
    let seconds_of_day = total_seconds % SECONDS_PER_DAY;
    let date = civil_date_from_days_since_epoch(total_seconds / SECONDS_PER_DAY);
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        date.year,
        date.month,
        date.day,
        seconds_of_day / SECONDS_PER_HOUR,
        seconds_of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
        seconds_of_day % SECONDS_PER_MINUTE
    )
}

/// Календарная дата (григорианский календарь).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CivilDate {
    year: u64,
    month: u64,
    day: u64,
}

/// Переводит число дней от 1970-01-01 в дату (алгоритм `civil_from_days` Говарда Хиннанта).
///
/// Идея: считать год с 1 марта, тогда високосный день 29 февраля оказывается в конце
/// года, а длины месяцев март…январь повторяются с шагом 153 дня на 5 месяцев.
/// Календарь повторяется каждые 400 лет («эра» = 146 097 дней).
fn civil_date_from_days_since_epoch(days_since_epoch: u64) -> CivilDate {
    // Сдвиг начала отсчёта с 1970-01-01 на 0000-03-01 (719 468 дней).
    let days_since_era_origin = days_since_epoch + 719_468;
    let era = days_since_era_origin / 146_097;
    // День внутри 400-летней эры: 0..=146096.
    let day_of_era = days_since_era_origin - era * 146_097;
    // Год внутри эры с поправками на високосные 4/100/400 лет: 0..=399.
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    // День внутри «мартовского» года: 0..=365.
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // Месяц, считая март нулевым: 0..=11.
    let march_based_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_based_month + 2) / 5 + 1;
    // Перевод обратно в привычные месяцы: март..декабрь → 3..12, январь/февраль → 1/2.
    let month = if march_based_month < 10 {
        march_based_month + 3
    } else {
        march_based_month - 9
    };
    // Январь и февраль относятся к следующему календарному году.
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    CivilDate { year, month, day }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{BrokenConfigBackupName, CivilDate, civil_date_from_days_since_epoch};

    /// Известные даты, включая границы месяцев и високосный день.
    #[test]
    fn days_since_epoch_convert_to_known_dates() {
        let known_dates = [
            (
                0,
                CivilDate {
                    year: 1970,
                    month: 1,
                    day: 1,
                },
            ),
            (
                59,
                CivilDate {
                    year: 1970,
                    month: 3,
                    day: 1,
                },
            ),
            (
                11_016,
                CivilDate {
                    year: 2000,
                    month: 2,
                    day: 29,
                },
            ),
            (
                19_782,
                CivilDate {
                    year: 2024,
                    month: 2,
                    day: 29,
                },
            ),
            (
                19_783,
                CivilDate {
                    year: 2024,
                    month: 3,
                    day: 1,
                },
            ),
            (
                20_453,
                CivilDate {
                    year: 2025,
                    month: 12,
                    day: 31,
                },
            ),
            (
                20_731,
                CivilDate {
                    year: 2026,
                    month: 10,
                    day: 5,
                },
            ),
        ];
        for (days_since_epoch, expected_date) in known_dates {
            assert_eq!(
                civil_date_from_days_since_epoch(days_since_epoch),
                expected_date,
                "день {days_since_epoch}"
            );
        }
    }

    /// Сверка с наивным счётом «день за днём» на каждом дне 1970–2200 годов.
    #[test]
    fn conversion_matches_day_by_day_calendar_walk() {
        let mut expected_date = CivilDate {
            year: 1970,
            month: 1,
            day: 1,
        };
        let mut days_since_epoch = 0;
        while expected_date.year <= 2200 {
            assert_eq!(
                civil_date_from_days_since_epoch(days_since_epoch),
                expected_date,
                "день {days_since_epoch}"
            );
            expected_date = next_day(expected_date);
            days_since_epoch += 1;
        }
    }

    /// Следующий день по простому правилу длины месяцев.
    fn next_day(date: CivilDate) -> CivilDate {
        let is_leap_year = date.year.is_multiple_of(4)
            && (!date.year.is_multiple_of(100) || date.year.is_multiple_of(400));
        let days_in_month = match date.month {
            2 if is_leap_year => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if date.day < days_in_month {
            CivilDate {
                day: date.day + 1,
                ..date
            }
        } else if date.month < 12 {
            CivilDate {
                month: date.month + 1,
                day: 1,
                ..date
            }
        } else {
            CivilDate {
                year: date.year + 1,
                month: 1,
                day: 1,
            }
        }
    }

    /// 2026-10-05 14:30:12 UTC — точная дата в имени и суффиксы следующих попыток.
    #[test]
    fn backup_name_contains_utc_date_time_and_attempt_suffix() {
        let moment = UNIX_EPOCH + Duration::from_secs(1_791_210_612);

        let backup_name = BrokenConfigBackupName::new("config.toml", moment);

        assert_eq!(
            backup_name.candidate(1),
            "config.toml.broken-2026-10-05_14-30-12.bak"
        );
        assert_eq!(
            backup_name.candidate(2),
            "config.toml.broken-2026-10-05_14-30-12-2.bak"
        );
    }

    /// Часы до 1970 года не ломают восстановление: имя остаётся валидным.
    #[test]
    fn clock_before_epoch_uses_unknown_time_label() {
        let moment = UNIX_EPOCH - Duration::from_secs(10);

        let backup_name = BrokenConfigBackupName::new("config.toml", moment);

        assert_eq!(
            backup_name.candidate(1),
            "config.toml.broken-unknown-time.bak"
        );
    }
}
