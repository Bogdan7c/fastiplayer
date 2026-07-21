use std::fmt;

/// Максимальная длина любого registry identity в UTF-8 bytes.
const MAX_DEMUX_IDENTITY_BYTES: usize = 128;

/// Ошибка построения bounded identity для demux boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DemuxIdentityError {
    /// Пустое значение не может однозначно идентифицировать registration или hint.
    #[error("{kind} не может быть пустым")]
    Empty {
        /// Человекочитаемое имя проверяемого типа.
        kind: &'static str,
    },

    /// Значение превышает общий безопасный diagnostic bound.
    #[error("{kind} превышает лимит {max_bytes} bytes")]
    TooLong {
        /// Человекочитаемое имя проверяемого типа.
        kind: &'static str,
        /// Допустимый предел UTF-8 bytes.
        max_bytes: usize,
    },

    /// Значение содержит символы вне стабильной registry grammar.
    #[error("{kind} содержит недопустимый символ `{character}`")]
    InvalidCharacter {
        /// Человекочитаемое имя проверяемого типа.
        kind: &'static str,
        /// Первый недопустимый символ.
        character: char,
    },
}

/// Общая owned-реализация bounded registry identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct BoundedIdentity(String);

impl BoundedIdentity {
    /// Проверяет и сохраняет canonical ASCII identity без скрытой нормализации.
    fn new(value: impl Into<String>, kind: &'static str) -> Result<Self, DemuxIdentityError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DemuxIdentityError::Empty { kind });
        }
        if value.len() > MAX_DEMUX_IDENTITY_BYTES {
            return Err(DemuxIdentityError::TooLong {
                kind,
                max_bytes: MAX_DEMUX_IDENTITY_BYTES,
            });
        }
        if let Some(character) = value.chars().find(|character| {
            !(character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_' | '.' | '+' | '/'))
        }) {
            return Err(DemuxIdentityError::InvalidCharacter { kind, character });
        }
        Ok(Self(value))
    }

    /// Возвращает exact canonical identity.
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BoundedIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Identity").field(&self.0).finish()
    }
}

macro_rules! define_demux_identity {
    ($name:ident, $kind:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(BoundedIdentity);

        impl $name {
            /// Создаёт bounded canonical identity без case-folding или alias expansion.
            pub fn new(value: impl Into<String>) -> Result<Self, DemuxIdentityError> {
                BoundedIdentity::new(value, $kind).map(Self)
            }

            /// Возвращает exact canonical identity.
            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.as_str())
                    .finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

define_demux_identity!(
    DemuxFactoryId,
    "demux factory ID",
    "Stable process-local identity конкретного demux factory."
);
define_demux_identity!(
    DemuxContainerId,
    "demux container ID",
    "Neutral canonical container identity, например `iso-bmff` или `matroska`."
);
define_demux_identity!(
    DemuxFixtureId,
    "demux fixture ID",
    "Stable evidence identity, которым registration ссылается на focused fixture."
);
define_demux_identity!(
    DemuxSourceExtension,
    "demux source extension",
    "Extension hint без ведущей точки; hint никогда не заменяет content sniff."
);
define_demux_identity!(
    DemuxMimeType,
    "demux MIME type",
    "Canonical MIME hint в lowercase ASCII форме."
);

#[cfg(test)]
mod tests {
    use super::{DemuxFactoryId, DemuxIdentityError, DemuxMimeType};

    /// Identity grammar не выполняет скрытый lowercase, чтобы disagreement был видимым.
    #[test]
    fn identity_rejects_non_canonical_case() {
        let error = DemuxFactoryId::new("Symphonia").expect_err("uppercase must be rejected");
        assert!(matches!(
            error,
            DemuxIdentityError::InvalidCharacter { character: 'S', .. }
        ));
    }

    /// MIME grammar допускает обычный `type/subtype` separator.
    #[test]
    fn mime_identity_accepts_canonical_separator() {
        let mime = DemuxMimeType::new("video/mp4").expect("canonical MIME");
        assert_eq!(mime.as_str(), "video/mp4");
    }
}
