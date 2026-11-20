//! Сколько частей прогона стенда идут одновременно (`--jobs`, сессия 16b3c2).
//!
//! Каждая часть — свой невидимый KWin, браузер и плеер. Строки стенда чувствительны ко
//! времени (окно «под тенью», рост кадров за 10 с), поэтому по умолчанию берётся число
//! с запасом: [`JobCount::default_for_machine`].

use std::num::NonZeroUsize;

use super::UsageError;

/// Логических процессоров на одну часть по умолчанию. Замер 16b3c2 на 16 потоках: быстрый
/// набор при 8 частях сразу грузит машину в среднем на ~2 ядра (строки больше ждут, чем
/// считают), и ни одна строка по времени не поплыла; два потока на часть — запас вчетверо.
const PROCESSORS_PER_JOB: usize = 2;
/// Потолок по умолчанию: при 8 частях быстрый набор уже упирается в самую долгую строку
/// (память выбора, ~1,5 мин), а каждый лишний браузер — сотни МиБ памяти.
const DEFAULT_JOBS_CEILING: usize = 8;

/// Число одновременных частей прогона (не меньше одной).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobCount(NonZeroUsize);

impl JobCount {
    /// Одна часть за раз (как стенд шёл до 16b3c2).
    pub const ONE: Self = Self(NonZeroUsize::MIN);

    /// По умолчанию для этой машины: логические процессоры / 2, от 1 до потолка.
    pub fn default_for_machine() -> Self {
        let processors = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        Self::for_processors(processors)
    }

    /// Чистая часть [`Self::default_for_machine`].
    fn for_processors(processors: usize) -> Self {
        let jobs = (processors / PROCESSORS_PER_JOB).clamp(1, DEFAULT_JOBS_CEILING);
        Self(NonZeroUsize::new(jobs).unwrap_or(NonZeroUsize::MIN))
    }

    /// Сколько частей сразу.
    pub fn get(self) -> usize {
        self.0.get()
    }

    /// Больше одной части сразу.
    pub fn is_parallel(self) -> bool {
        self.get() > 1
    }
}

/// Разбор `--jobs N` (целое больше нуля).
pub(super) fn parse_jobs(value: &str) -> Result<JobCount, UsageError> {
    value
        .parse::<usize>()
        .ok()
        .and_then(NonZeroUsize::new)
        .map(JobCount)
        .ok_or_else(|| UsageError(format!("--jobs expects a positive integer, got '{value}'")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_keeps_two_processors_per_job_within_bounds() {
        assert_eq!(JobCount::for_processors(1).get(), 1);
        assert_eq!(JobCount::for_processors(4).get(), 2);
        assert_eq!(JobCount::for_processors(8).get(), 4);
        assert_eq!(JobCount::for_processors(16).get(), 8);
        assert_eq!(JobCount::for_processors(64).get(), DEFAULT_JOBS_CEILING);
    }

    #[test]
    fn jobs_must_be_a_positive_integer() {
        assert_eq!(parse_jobs("3").map(JobCount::get), Ok(3));
        for value in ["0", "-1", "two", ""] {
            assert!(parse_jobs(value).is_err(), "{value:?}");
        }
    }
}
