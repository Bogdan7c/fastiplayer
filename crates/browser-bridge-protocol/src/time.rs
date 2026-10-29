//! Время, скорость и байтовые смещения протокола.
//!
//! Время в протоколе — целые микросекунды, а не дробные секунды:
//! - сравнение и равенство однозначны, нет ошибок округления `0.1 + 0.2`;
//! - фикстуры совпадают байт в байт на Rust и TypeScript;
//! - DVR-позиция live YouTube (~46 800 с) — это 4,7·10¹⁰ мкс, далеко до
//!   предела точных целых JavaScript (2⁵³ − 1 ≈ 285 лет).
//!
//! Экстеншен переводит секунды MSE (`double`) в микросекунды округлением.

use serde::{Deserialize, Serialize};

use crate::error::ValueViolation;
use crate::limits::{
    MAX_JSON_SAFE_INTEGER, MAX_PLAYBACK_RATE_PERMILLE, MAX_RANGE_FETCH_BYTES,
    MIN_PLAYBACK_RATE_PERMILLE,
};

/// Момент на шкале медиа, микросекунды от нуля, не больше 2⁵³ − 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct MediaTime(u64);

impl MediaTime {
    /// Начало шкалы.
    pub const ZERO: Self = Self(0);

    /// Момент из микросекунд.
    ///
    /// # Errors
    /// Значение больше точного целого JavaScript.
    pub const fn from_micros(micros: u64) -> Result<Self, ValueViolation> {
        if micros > MAX_JSON_SAFE_INTEGER {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(micros))
    }

    /// Микросекунды от нуля.
    #[must_use]
    pub const fn as_micros(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for MediaTime {
    type Error = ValueViolation;

    fn try_from(micros: u64) -> Result<Self, Self::Error> {
        Self::from_micros(micros)
    }
}

impl From<MediaTime> for u64 {
    fn from(time: MediaTime) -> Self {
        time.0
    }
}

/// Сдвиг времени со знаком, микросекунды (`SourceBuffer.timestampOffset`
/// бывает отрицательным). По модулю не больше 2⁵³ − 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct MediaTimeOffset(i64);

impl MediaTimeOffset {
    /// Сдвиг из микросекунд.
    ///
    /// # Errors
    /// Модуль больше точного целого JavaScript.
    pub const fn from_micros(micros: i64) -> Result<Self, ValueViolation> {
        if micros.unsigned_abs() > MAX_JSON_SAFE_INTEGER {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(micros))
    }

    /// Микросекунды со знаком.
    #[must_use]
    pub const fn as_micros(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for MediaTimeOffset {
    type Error = ValueViolation;

    fn try_from(micros: i64) -> Result<Self, Self::Error> {
        Self::from_micros(micros)
    }
}

impl From<MediaTimeOffset> for i64 {
    fn from(offset: MediaTimeOffset) -> Self {
        offset.0
    }
}

/// Длительность ролика, как её знает плеер страницы.
///
/// Три честных состояния вместо «0 значит неизвестно» (решение 50: длина часто
/// приходит позже, у live её нет вовсе).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaDuration {
    /// Плеер страницы длину ещё не сообщил (`NaN` в MSE).
    Unknown {},
    /// Конечная длина: перемотка за конец даёт `ended` (`results/00c.md`, п. 7).
    Finite {
        /// Длина ролика.
        duration_us: MediaTime,
    },
    /// Эфир без конца (`Infinity` в MSE).
    Live {},
}

/// Скорость воспроизведения в тысячных долях: 1000 = 1×, 2000 = 2×.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct PlaybackRate(u32);

impl PlaybackRate {
    /// Обычная скорость 1×.
    pub const NORMAL: Self = Self(1000);

    /// Скорость из тысячных долей.
    ///
    /// # Errors
    /// Вне диапазона, который принимают браузеры (0,063×–16×).
    pub const fn from_permille(permille: u32) -> Result<Self, ValueViolation> {
        if permille < MIN_PLAYBACK_RATE_PERMILLE || permille > MAX_PLAYBACK_RATE_PERMILLE {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(permille))
    }

    /// Тысячные доли скорости.
    #[must_use]
    pub const fn as_permille(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for PlaybackRate {
    type Error = ValueViolation;

    fn try_from(permille: u32) -> Result<Self, Self::Error> {
        Self::from_permille(permille)
    }
}

impl From<PlaybackRate> for u32 {
    fn from(rate: PlaybackRate) -> Self {
        rate.0
    }
}

/// Смещение в файле `<video src>`, байт, не больше 2⁵³ − 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct ByteOffset(u64);

impl ByteOffset {
    /// Смещение из числа байт.
    ///
    /// # Errors
    /// Больше точного целого JavaScript.
    pub const fn new(bytes: u64) -> Result<Self, ValueViolation> {
        if bytes > MAX_JSON_SAFE_INTEGER {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(bytes))
    }

    /// Число байт от начала файла.
    #[must_use]
    pub const fn as_bytes(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for ByteOffset {
    type Error = ValueViolation;

    fn try_from(bytes: u64) -> Result<Self, Self::Error> {
        Self::new(bytes)
    }
}

impl From<ByteOffset> for u64 {
    fn from(offset: ByteOffset) -> Self {
        offset.0
    }
}

/// Длина одного запроса байтов `<video src>`: от 1 байта до
/// [`MAX_RANGE_FETCH_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct RangeFetchLength(u64);

impl RangeFetchLength {
    /// Длина из числа байт.
    ///
    /// # Errors
    /// Ноль или больше лимита одного запроса.
    pub const fn new(bytes: u64) -> Result<Self, ValueViolation> {
        if bytes == 0 || bytes > MAX_RANGE_FETCH_BYTES {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(bytes))
    }

    /// Число байт.
    #[must_use]
    pub const fn as_bytes(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for RangeFetchLength {
    type Error = ValueViolation;

    fn try_from(bytes: u64) -> Result<Self, Self::Error> {
        Self::new(bytes)
    }
}

impl From<RangeFetchLength> for u64 {
    fn from(length: RangeFetchLength) -> Self {
        length.0
    }
}
