//! Запомненный язык звука web-видео (дубляж YouTube и другие extractor-ы).
//!
//! Язык звука web-источника выбирают между форматами одного ролика: у них нет номера потока,
//! а ссылки подписаны и живут недолго. Поэтому хранится только языковой тег; по нему при
//! следующем открытии ищется язык в свежем списке видео. Это отдельное значение, а не
//! `RememberedTrackHint`: подсказка описывает встроенную дорожку контейнера (кодек, порядковый
//! номер), которой у web-языка нет.

use std::fmt;

use super::hint::MAX_HINT_LANGUAGE_BYTES;

/// Почему язык звука нельзя запомнить.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RememberedWebAudioLanguageError {
    /// Пустой тег: по нему язык в списке не найти.
    Empty,
    /// Длиннее [`MAX_HINT_LANGUAGE_BYTES`].
    TooLong,
}

impl fmt::Display for RememberedWebAudioLanguageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "remembered web audio language is empty",
            Self::TooLong => "remembered web audio language is too long",
        })
    }
}

impl std::error::Error for RememberedWebAudioLanguageError {}

/// Языковой тег звука web-видео (`en-US`, `ru`).
///
/// Инвариант: тег непустой и не длиннее [`MAX_HINT_LANGUAGE_BYTES`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RememberedWebAudioLanguage(String);

impl RememberedWebAudioLanguage {
    /// Проверяет тег и строит значение.
    pub fn new(language: String) -> Result<Self, RememberedWebAudioLanguageError> {
        if language.is_empty() {
            return Err(RememberedWebAudioLanguageError::Empty);
        }
        if language.len() > MAX_HINT_LANGUAGE_BYTES {
            return Err(RememberedWebAudioLanguageError::TooLong);
        }
        Ok(Self(language))
    }

    /// Языковой тег.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Совпадает ли тег с другим (регистр не важен: `en-us` == `en-US`).
    #[must_use]
    pub fn matches_tag(&self, other_tag: &str) -> bool {
        self.0.eq_ignore_ascii_case(other_tag)
    }
}
