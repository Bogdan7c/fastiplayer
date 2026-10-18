//! Запомненный выбор субтитров потока (HLS-рендиции и другие потоковые источники).
//!
//! Дорожка потока не имеет ни файла, ни стабильного номера (индекс меняется между открытиями,
//! а адрес секретен). Её описывают то, что остаётся прежним в манифесте: язык (если он задан),
//! название дорожки и признак «только форсированные». По нему при следующем открытии находится
//! дорожка в свежем списке. Адресов и токенов здесь нет и быть не может.

use std::fmt;

use super::hint::{MAX_HINT_LANGUAGE_BYTES, MAX_HINT_TITLE_BYTES};

/// Почему описание дорожки потока нельзя запомнить.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RememberedStreamSubtitleError {
    /// Языковой тег задан, но пуст.
    EmptyLanguage,
    /// Языковой тег длиннее [`MAX_HINT_LANGUAGE_BYTES`].
    LanguageTooLong,
    /// Название пустое: без него дорожки одного языка не различить.
    EmptyName,
    /// Название длиннее [`MAX_HINT_TITLE_BYTES`].
    NameTooLong,
}

impl fmt::Display for RememberedStreamSubtitleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyLanguage => "remembered stream subtitle language is empty",
            Self::LanguageTooLong => "remembered stream subtitle language is too long",
            Self::EmptyName => "remembered stream subtitle name is empty",
            Self::NameTooLong => "remembered stream subtitle name is too long",
        })
    }
}

impl std::error::Error for RememberedStreamSubtitleError {}

/// Описание дорожки потока: язык, название и признак форсированных.
///
/// Инварианты: язык, если есть, непустой и не длиннее [`MAX_HINT_LANGUAGE_BYTES`]; название
/// непустое и не длиннее [`MAX_HINT_TITLE_BYTES`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RememberedStreamSubtitle {
    language: Option<String>,
    name: String,
    forced: bool,
}

impl RememberedStreamSubtitle {
    /// Проверяет значения и строит описание.
    pub fn new(
        language: Option<String>,
        name: String,
        forced: bool,
    ) -> Result<Self, RememberedStreamSubtitleError> {
        if let Some(language) = &language {
            if language.is_empty() {
                return Err(RememberedStreamSubtitleError::EmptyLanguage);
            }
            if language.len() > MAX_HINT_LANGUAGE_BYTES {
                return Err(RememberedStreamSubtitleError::LanguageTooLong);
            }
        }
        if name.is_empty() {
            return Err(RememberedStreamSubtitleError::EmptyName);
        }
        if name.len() > MAX_HINT_TITLE_BYTES {
            return Err(RememberedStreamSubtitleError::NameTooLong);
        }
        Ok(Self {
            language,
            name,
            forced,
        })
    }

    /// Языковой тег манифеста; `None` - манифест язык не указал.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Название дорожки из манифеста.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Дорожка «только форсированные».
    #[must_use]
    pub const fn forced(&self) -> bool {
        self.forced
    }
}
