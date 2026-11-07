//! Что движок просит у внешней стороны: номерок ресурса, часть ресурса и потолок.
//!
//! Типы нейтральны: «номерок» — просто число, которое внешняя сторона дала ресурсу
//! (у моста — экстеншен переписал адрес в плейлисте на номерок). Что за ним стоит
//! (адрес, куки), crate не знает и знать не должен.

use std::num::NonZeroU64;

use crate::range_file::RangeRequestId;

/// Номерок ресурса, который выдала внешняя сторона.
///
/// Плеер адресов не видит: у него только номерки, и в запросе уходит только номерок.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalResourceHandle(u32);

impl ExternalResourceHandle {
    /// Номерок из числа (приложение переводит его из адреса-заглушки).
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

impl std::fmt::Display for ExternalResourceHandle {
    /// Номерок печатается числом: он ничего не раскрывает.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "resource {}", self.0)
    }
}

/// Диапазон байтов ресурса `[offset, offset + length)`.
///
/// Конец всегда помещается в `u64`: это проверяет [`Self::new`], поэтому дальше
/// никакой арифметике с концом переполнение не грозит.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceByteRange {
    /// Первый байт.
    offset: u64,
    /// Сколько байтов.
    length: NonZeroU64,
}

/// Диапазон, конец которого не помещается в `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("byte range at offset {offset} with length {length} ends past u64::MAX")]
pub struct ResourceByteRangeOverflow {
    /// Смещение.
    pub offset: u64,
    /// Длина.
    pub length: u64,
}

impl ResourceByteRange {
    /// Диапазон; конец за пределом `u64` — ошибка.
    ///
    /// # Errors
    /// [`ResourceByteRangeOverflow`], если `offset + length` переполняет `u64`.
    pub const fn new(offset: u64, length: NonZeroU64) -> Result<Self, ResourceByteRangeOverflow> {
        if offset.checked_add(length.get()).is_none() {
            return Err(ResourceByteRangeOverflow {
                offset,
                length: length.get(),
            });
        }
        Ok(Self { offset, length })
    }

    /// Первый байт.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Длина.
    #[must_use]
    pub const fn length(self) -> NonZeroU64 {
        self.length
    }
}

/// Какую часть ресурса просим.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceExtent {
    /// Весь ресурс (плейлист, ключ, сегмент без byte-range).
    Whole,
    /// Только диапазон (`EXT-X-BYTERANGE` и подобные).
    Range(ResourceByteRange),
}

/// Просьба движка: «дай ресурс `handle` (его часть `extent`), не больше `max_bytes`».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceFetchSpec {
    /// Номерок ресурса.
    pub handle: ExternalResourceHandle,
    /// Весь ресурс или диапазон.
    pub extent: ResourceExtent,
    /// Сколько байтов движок согласен принять (не больше потолка политики очереди).
    pub max_bytes: NonZeroU64,
}

impl ResourceFetchSpec {
    /// Сколько байтов тела может прийти по этому запросу: `max_bytes`, а у диапазона
    /// — не больше его длины. Больше — нарушение страницы.
    #[must_use]
    pub fn body_limit(&self) -> u64 {
        match self.extent {
            ResourceExtent::Whole => self.max_bytes.get(),
            ResourceExtent::Range(range) => self.max_bytes.get().min(range.length().get()),
        }
    }
}

/// Запрос, который приложение отправляет внешней стороне (у моста —
/// `fetch_resource`), в порядке выдачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceRequest {
    /// Номер запроса: общий счётчик сеанса, тот же, что у файла по запросам байтов.
    pub id: RangeRequestId,
    /// Номерок ресурса.
    pub handle: ExternalResourceHandle,
    /// Весь ресурс или диапазон.
    pub extent: ResourceExtent,
    /// Потолок тела, байт.
    pub max_bytes: NonZeroU64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_end_past_u64_is_refused_and_a_fitting_range_keeps_its_numbers() {
        let length = NonZeroU64::new(10).expect("non-zero");
        assert_eq!(
            ResourceByteRange::new(u64::MAX - 5, length),
            Err(ResourceByteRangeOverflow {
                offset: u64::MAX - 5,
                length: 10
            })
        );
        let fitting = ResourceByteRange::new(u64::MAX - 10, length).expect("fits exactly");
        assert_eq!(fitting.offset(), u64::MAX - 10);
        assert_eq!(fitting.length(), length);
    }

    #[test]
    fn body_limit_is_the_smaller_of_max_bytes_and_range_length() {
        let range =
            ResourceByteRange::new(100, NonZeroU64::new(50).expect("non-zero")).expect("range");
        let spec = |extent, max_bytes| ResourceFetchSpec {
            handle: ExternalResourceHandle::new(1),
            extent,
            max_bytes: NonZeroU64::new(max_bytes).expect("non-zero"),
        };
        assert_eq!(spec(ResourceExtent::Whole, 70).body_limit(), 70);
        assert_eq!(spec(ResourceExtent::Range(range), 70).body_limit(), 50);
        assert_eq!(spec(ResourceExtent::Range(range), 20).body_limit(), 20);
    }
}
