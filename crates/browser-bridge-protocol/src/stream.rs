//! Номера потока: поколение, SourceBuffer, порядковый номер куска.
//!
//! Поток данных идёт **по SourceBuffer** (`results/00.md`): каждый SourceBuffer
//! плеера страницы — свой байтовый поток fMP4 или WebM, куски режутся где
//! угодно, даже посреди бокса. Чтобы приёмник собрал байты правильно и выбросил
//! устаревшие (инвариант 6), у каждого куска есть три номера:
//!
//! - **поколение** — новый `MediaSource` на элементе: реклама YouTube,
//!   следующий ролик, серия, озвучка (решения 66, 67). Начинается с 1 и растёт
//!   ровно на 1. Поколение куска определено точно: каждый SourceBuffer
//!   принадлежит одному `MediaSource`, и экстеншен помечает его при
//!   `addSourceBuffer`. Поэтому байты старого поколения можно смело выбрасывать;
//! - **SourceBuffer** — индекс в поколении, от 0 до 7;
//! - **порядковый номер** — с 0 для каждого SourceBuffer поколения, растёт на 1
//!   с каждой рамкой. Пропуск номера — ошибка: значит, кусок потерян, и байты
//!   дальше склеивать нельзя.
//!
//! **Перемотка не меняет номеров и ничего не выбрасывает.** Она — событие
//! `seek_started` (`crate::session::page`). Пометить кусок «до» или «после»
//! перемотки точно нельзя: у Twitch `MediaSource` живёт в воркере, а перемотка
//! случается на элементе в главном потоке, и порядок между ними не гарантирован.
//! Разрыв времени после перемотки приёмник видит по меткам времени в самих
//! байтах (`tfdt`, кластеры WebM) — так же, как это делает браузер в MSE, — а
//! сброс разбора по `abort()` приходит отдельным сообщением
//! `source_buffer_aborted`.

use serde::{Deserialize, Serialize};

use crate::error::ValueViolation;
use crate::limits::MAX_SOURCE_BUFFERS_PER_GENERATION;

/// Номер поколения потока (новый `MediaSource`), от 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct StreamGeneration(u32);

impl StreamGeneration {
    /// Первое поколение сеанса.
    pub const FIRST: Self = Self(1);

    /// Поколение из номера.
    ///
    /// # Errors
    /// Ноль: поколения считаются с 1.
    pub const fn new(number: u32) -> Result<Self, ValueViolation> {
        if number == 0 {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(number))
    }

    /// Номер поколения.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Следующее поколение; `None` после `u32::MAX` (4 млрд смен — не бывает).
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(number) => Some(Self(number)),
            None => None,
        }
    }
}

impl TryFrom<u32> for StreamGeneration {
    type Error = ValueViolation;

    fn try_from(number: u32) -> Result<Self, Self::Error> {
        Self::new(number)
    }
}

impl From<StreamGeneration> for u32 {
    fn from(generation: StreamGeneration) -> Self {
        generation.0
    }
}

/// Индекс SourceBuffer в поколении, от 0 до 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct SourceBufferIndex(u16);

impl SourceBufferIndex {
    /// Индекс из числа.
    ///
    /// # Errors
    /// Не меньше [`MAX_SOURCE_BUFFERS_PER_GENERATION`].
    pub const fn new(index: u16) -> Result<Self, ValueViolation> {
        if index >= MAX_SOURCE_BUFFERS_PER_GENERATION {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(index))
    }

    /// Индекс как число.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Индекс для таблиц приёмника (всегда меньше 8).
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<u16> for SourceBufferIndex {
    type Error = ValueViolation;

    fn try_from(index: u16) -> Result<Self, Self::Error> {
        Self::new(index)
    }
}

impl From<SourceBufferIndex> for u16 {
    fn from(index: SourceBufferIndex) -> Self {
        index.0
    }
}

/// Порядковый номер рамки в SourceBuffer поколения, от 0. Только в двоичных рамках.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppendSequence(pub u64);

/// Номер перемотки, которую попросил плеер (`seek`), — чтобы узнать её в ответе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeekId(pub u32);

/// Номер запроса байтов `<video src>` (`fetch_range`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RangeRequestId(pub u32);
