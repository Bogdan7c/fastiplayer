//! Реклама на экране (сессия 09d, решения 184, 204, 206, 207).
//!
//! Страница сообщает плееру только числа и метки: сколько реклам в блоке и можно ли уже
//! пропустить. Текста со страницы здесь нет и быть не может (инвариант 1): подписи плашки
//! плеер пишет сам. Пропускать или нет, решает страница: метки `available` и `on_page`
//! появляются, только когда страница сама показала свою кнопку.

use serde::{Deserialize, Serialize};

use crate::error::ValueViolation;
use crate::strict_shape::strict_object;

strict_object!(PageAdState, AdPod, AdSkip);

/// Сколько реклам может быть в одном блоке. Живые сайты показывают 1–2, запас от мусора.
pub const MAX_ADS_IN_POD: u16 = 64;

/// Самый длинный отсчёт до пропуска в секундах. Живые сайты показывают 5–15.
pub const MAX_AD_COUNTDOWN_SECONDS: u16 = 3_600;

/// Номер рекламы в блоке или размер блока: от 1 до [`MAX_ADS_IN_POD`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct AdPodNumber(u16);

impl AdPodNumber {
    /// Номер из числа.
    ///
    /// # Errors
    /// Ноль или больше лимита блока.
    pub const fn new(number: u16) -> Result<Self, ValueViolation> {
        if number == 0 || number > MAX_ADS_IN_POD {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(number))
    }

    /// Число.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for AdPodNumber {
    type Error = ValueViolation;

    fn try_from(number: u16) -> Result<Self, Self::Error> {
        Self::new(number)
    }
}

impl From<AdPodNumber> for u16 {
    fn from(number: AdPodNumber) -> Self {
        number.0
    }
}

/// Секунды до пропуска, как их показал сайт: от 0 до [`MAX_AD_COUNTDOWN_SECONDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct AdCountdownSeconds(u16);

impl AdCountdownSeconds {
    /// Отсчёт из числа секунд.
    ///
    /// # Errors
    /// Больше лимита.
    pub const fn new(seconds: u16) -> Result<Self, ValueViolation> {
        if seconds > MAX_AD_COUNTDOWN_SECONDS {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(seconds))
    }

    /// Число секунд.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for AdCountdownSeconds {
    type Error = ValueViolation;

    fn try_from(seconds: u16) -> Result<Self, Self::Error> {
        Self::new(seconds)
    }
}

impl From<AdCountdownSeconds> for u16 {
    fn from(seconds: AdCountdownSeconds) -> Self {
        seconds.0
    }
}

/// Какая реклама из блока идёт.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AdPod {
    /// Сайт счётчика не показывает или его органы нам незнакомы.
    Unknown {},
    /// Сайт показывает «N из M».
    Known {
        /// Номер идущей рекламы.
        index: AdPodNumber,
        /// Сколько всего в блоке.
        count: AdPodNumber,
    },
}

/// Можно ли пропустить идущую рекламу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AdSkip {
    /// Пропуска нет или сайт о нём пока не говорит.
    Unavailable {},
    /// Сайт показывает отсчёт до пропуска.
    Countdown {
        /// Секунды, как на сайте.
        seconds: AdCountdownSeconds,
    },
    /// Сайт показал кнопку, и наш клик она принимает: плеер предлагает «Skip».
    Available {},
    /// Сайт показал кнопку, но верит только настоящей мыши (YouTube, решения 207, 208):
    /// кнопка есть, нажать её может только человек на странице; плеер даёт подсказку.
    OnPage {},
}

/// Что сейчас с рекламой на странице.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PageAdState {
    /// Рекламы нет.
    None {},
    /// Идёт реклама.
    Showing {
        /// Какая из блока.
        pod: AdPod,
        /// Можно ли пропустить.
        skip: AdSkip,
    },
}

impl PageAdState {
    /// Правило между полями: номер рекламы не больше размера блока.
    ///
    /// # Errors
    /// Номер больше размера блока.
    pub const fn validate(&self) -> Result<(), ValueViolation> {
        if let Self::Showing {
            pod: AdPod::Known { index, count },
            ..
        } = self
            && index.get() > count.get()
        {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(())
    }
}
