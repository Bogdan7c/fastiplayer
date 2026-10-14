//! Описание дорожки субтитров: откуда она, в каком формате, на каком языке.

use crate::language::SubtitleLanguage;

/// Стабильный идентификатор дорожки внутри одного открытого медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubtitleTrackId(u64);

impl SubtitleTrackId {
    /// Создаёт идентификатор из числа.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Числовое значение идентификатора.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Откуда взялась дорожка.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SubtitleTrackOrigin {
    /// Встроена в контейнер; индекс — номер потока в контейнере.
    Embedded { container_track_index: u32 },
    /// Отдельный файл рядом с медиа.
    Sidecar,
    /// Получена из веб-источника.
    Web,
}

/// Формат хранения субтитров.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SubtitleFormat {
    Srt,
    WebVtt,
    Ass,
    Ssa,
    MovText,
    Srv3,
    Ttml,
    MicroDvd,
    Sami,
    Pgs,
}

/// Флаги дорожки; по умолчанию все выключены.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SubtitleTrackDisposition {
    /// Контейнер помечает дорожку как выбираемую по умолчанию.
    pub is_default: bool,
    /// Дорожка содержит только «принудительные» реплики (иностранная речь).
    pub is_forced: bool,
    /// Субтитры сгенерированы автоматически (например, авто-титры веба).
    pub is_auto_generated: bool,
}

/// Метаданные дорожки. Поля приватные: инварианты задаются конструктором и builder-методами.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrackDescriptor {
    id: SubtitleTrackId,
    origin: SubtitleTrackOrigin,
    format: SubtitleFormat,
    language: Option<SubtitleLanguage>,
    title: Option<String>,
    disposition: SubtitleTrackDisposition,
}

impl SubtitleTrackDescriptor {
    /// Минимальное описание: язык, название и флаги по умолчанию отсутствуют.
    pub fn new(id: SubtitleTrackId, origin: SubtitleTrackOrigin, format: SubtitleFormat) -> Self {
        Self {
            id,
            origin,
            format,
            language: None,
            title: None,
            disposition: SubtitleTrackDisposition::default(),
        }
    }

    /// Задаёт язык дорожки.
    #[must_use]
    pub fn with_language(mut self, language: SubtitleLanguage) -> Self {
        self.language = Some(language);
        self
    }

    /// Задаёт название дорожки.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Задаёт флаги дорожки.
    #[must_use]
    pub fn with_disposition(mut self, disposition: SubtitleTrackDisposition) -> Self {
        self.disposition = disposition;
        self
    }

    pub fn id(&self) -> SubtitleTrackId {
        self.id
    }

    pub fn origin(&self) -> SubtitleTrackOrigin {
        self.origin
    }

    pub fn format(&self) -> SubtitleFormat {
        self.format
    }

    pub fn language(&self) -> Option<&SubtitleLanguage> {
        self.language.as_ref()
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn disposition(&self) -> SubtitleTrackDisposition {
        self.disposition
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_descriptor_has_no_optional_metadata() {
        let descriptor = SubtitleTrackDescriptor::new(
            SubtitleTrackId::new(7),
            SubtitleTrackOrigin::Sidecar,
            SubtitleFormat::Srt,
        );
        assert_eq!(descriptor.id().get(), 7);
        assert_eq!(descriptor.origin(), SubtitleTrackOrigin::Sidecar);
        assert_eq!(descriptor.format(), SubtitleFormat::Srt);
        assert!(descriptor.language().is_none());
        assert!(descriptor.title().is_none());
        assert_eq!(
            descriptor.disposition(),
            SubtitleTrackDisposition::default()
        );
    }

    #[test]
    fn builders_fill_metadata() {
        let language = SubtitleLanguage::parse("ru").expect("valid tag");
        let descriptor = SubtitleTrackDescriptor::new(
            SubtitleTrackId::new(1),
            SubtitleTrackOrigin::Embedded {
                container_track_index: 3,
            },
            SubtitleFormat::WebVtt,
        )
        .with_language(language.clone())
        .with_title("Русские")
        .with_disposition(SubtitleTrackDisposition {
            is_forced: true,
            ..SubtitleTrackDisposition::default()
        });
        assert_eq!(descriptor.language(), Some(&language));
        assert_eq!(descriptor.title(), Some("Русские"));
        assert!(descriptor.disposition().is_forced);
        assert!(!descriptor.disposition().is_default);
        assert_eq!(
            descriptor.origin(),
            SubtitleTrackOrigin::Embedded {
                container_track_index: 3
            }
        );
    }
}
