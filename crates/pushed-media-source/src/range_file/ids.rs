//! Номера запросов диапазонов и сами запросы — то, что источник просит у страницы.
//!
//! У рамок байтов нет «поколения»: у каждого нового файла (нового поколения
//! страницы) свой поток запросов, но номера **сквозные на весь сеанс**. Поэтому
//! запоздавшую рамку прежнего файла можно узнать по номеру ниже первого номера
//! нового потока — это не атака, её молча отбрасывают.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::PushedFileError;

/// Номер запроса диапазона. Выдаётся только [`RangeRequestIds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RangeRequestId(u32);

impl RangeRequestId {
    /// Номер из числа провода (приложение переводит рамки протокола в этот тип).
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Число для провода.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for RangeRequestId {
    /// Номер печатается числом: он ничего не раскрывает.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "#{}", self.0)
    }
}

/// Выдавался ли номер этим выдавателем (см. [`RangeRequestIds::standing_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeIdStanding {
    /// Номер выдан (какому файлу — не важно): рамка с ним может быть честным запоздавшим ответом.
    Issued,
    /// Номера никто не выдавал: рамка или итог с ним — выдумка страницы.
    NeverIssued,
}

/// Выдаватель номеров запросов, общий на сеанс: клон делит один счётчик.
///
/// Номера начинаются с 1 и растут без повторов. Счётчик хранит «следующий номер» как
/// `u64`, поэтому после последнего `u32` выдача честно заканчивается ошибкой, а не
/// переполняется и не начинает выдавать старые номера заново.
#[derive(Debug, Clone)]
pub struct RangeRequestIds {
    /// Следующий номер (на единицу больше последнего выданного).
    next: Arc<AtomicU64>,
}

impl Default for RangeRequestIds {
    fn default() -> Self {
        Self::new()
    }
}

impl RangeRequestIds {
    /// Первый номер сеанса.
    const FIRST: u64 = 1;

    /// Новый счётчик сеанса: первый выданный номер — 1.
    #[must_use]
    pub fn new() -> Self {
        Self::starting_at(Self::FIRST)
    }

    /// Счётчик, который выдаст `first` первым (в тестах — для проверки исчерпания).
    #[cfg(test)]
    pub(crate) fn starting_from_value(first: u32) -> Self {
        Self::starting_at(u64::from(first))
    }

    /// Счётчик с заданным следующим номером.
    fn starting_at(first: u64) -> Self {
        Self {
            next: Arc::new(AtomicU64::new(first)),
        }
    }

    /// Выдаёт следующий номер; после `u32::MAX` — ошибка исчерпания.
    pub(crate) fn issue(&self) -> Result<RangeRequestId, PushedFileError> {
        let issued = self
            .next
            .try_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                // Последний допустимый номер — u32::MAX; дальше счётчик замирает.
                (next <= u64::from(u32::MAX)).then_some(next + 1)
            })
            .map_err(|_| PushedFileError::RequestIdsExhausted)?;
        // Проверка выше гарантирует, что `issued` помещается в u32.
        u32::try_from(issued)
            .map(RangeRequestId)
            .map_err(|_| PushedFileError::RequestIdsExhausted)
    }

    /// Выдавался ли номер `id` этим выдавателем (или его клоном) за всё время.
    ///
    /// Нужен владельцу сеанса, когда у него нет текущего файла (поколение сменилось):
    /// выданный номер — запоздавший ответ прежнего файла, невыданный — нарушение
    /// страницы. Номера начинаются с 1, поэтому 0 не выдавался никогда.
    #[must_use]
    pub fn standing_of(&self, id: RangeRequestId) -> RangeIdStanding {
        let raw = u64::from(id.get());
        if raw >= Self::FIRST && raw < self.next_unissued() {
            RangeIdStanding::Issued
        } else {
            RangeIdStanding::NeverIssued
        }
    }

    /// Номер, который будет выдан следующим (не выдавая его): всё, что ниже, — из
    /// прошлого, до создания файла.
    pub(crate) fn next_unissued(&self) -> u64 {
        self.next.load(Ordering::Acquire)
    }
}

/// Просьба к странице: «пришли байты `[offset, offset + length)`».
///
/// Приложение переводит её в `fetch_range` протокола и шлёт странице в порядке
/// выдачи ([`PushedFileRequests::take_requests`](super::PushedFileRequests::take_requests)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeRequest {
    /// Номер запроса: по нему рамки байтов и итог привязываются к просьбе.
    pub id: RangeRequestId,
    /// Первый запрошенный байт файла.
    pub offset: u64,
    /// Сколько байтов просим (не больше [`MAX_RANGE_REQUEST_BYTES`](super::MAX_RANGE_REQUEST_BYTES)).
    pub length: NonZeroU32,
}

/// Будильник для того, кто отправляет запросы страницы (у приложения — главный цикл).
///
/// Вызывается каждый раз, когда появился новый запрос, **без удержания внутренних
/// замков источника**: реализация вправе сразу вызывать
/// [`PushedFileRequests::take_requests`](super::PushedFileRequests::take_requests).
pub trait RangeRequestNotifier: Send + Sync {
    /// Появился новый запрос, который надо отправить странице.
    fn range_requested(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_start_at_one_and_clones_share_the_counter() {
        let ids = RangeRequestIds::new();
        let clone = ids.clone();
        let issued: Vec<u32> = [&ids, &clone, &ids, &clone]
            .iter()
            .map(|source| source.issue().expect("id").get())
            .collect();
        assert_eq!(issued, [1, 2, 3, 4]);
        assert_eq!(ids.next_unissued(), 5);
    }

    #[test]
    fn standing_tells_issued_numbers_from_invented_ones_across_clones() {
        let ids = RangeRequestIds::new();
        let clone = ids.clone();
        let first = ids.issue().expect("id");
        let second = clone.issue().expect("id");

        for issued in [first, second] {
            assert_eq!(ids.standing_of(issued), RangeIdStanding::Issued);
            assert_eq!(clone.standing_of(issued), RangeIdStanding::Issued);
        }
        for invented in [0, 3, 1_000] {
            assert_eq!(
                ids.standing_of(RangeRequestId::new(invented)),
                RangeIdStanding::NeverIssued,
                "{invented}"
            );
        }
        // Выданный следом становится выданным, ничего другого не меняется.
        let third = ids.issue().expect("id");
        assert_eq!(clone.standing_of(third), RangeIdStanding::Issued);
    }

    #[test]
    fn exhaustion_is_an_error_and_never_wraps_around() {
        let ids = RangeRequestIds::starting_from_value(u32::MAX - 1);
        assert_eq!(ids.issue().expect("second to last").get(), u32::MAX - 1);
        assert_eq!(ids.issue().expect("last").get(), u32::MAX);
        for _ in 0..3 {
            assert_eq!(ids.issue(), Err(PushedFileError::RequestIdsExhausted));
        }
    }
}
