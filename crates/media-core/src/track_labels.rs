//! Человекочитаемые метки дорожки: язык, название и флаги назначения.
//!
//! Метки одинаково доступны для видео, аудио и субтитров. Контейнерные demuxer-ы
//! только заполняют их; нормализацию языка (BCP-47 / ISO 639-2 → отображаемое имя)
//! выполняет потребитель, поэтому здесь значение хранится «как в контейнере».

/// Язык дорожки в том виде, в котором его записал контейнер.
///
/// Это может быть BCP-47 (`en-US`, MKV `LanguageBCP47`) или ISO 639-2 (`eng`, `rus`).
/// Валидации и нормализации нет намеренно: решать, как показывать язык, должен UI.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackLanguage(String);

impl TrackLanguage {
    /// Оборачивает сырую строку языка из контейнера.
    #[must_use]
    pub fn new(raw_language: impl Into<String>) -> Self {
        Self(raw_language.into())
    }

    /// Возвращает сырую строку языка без изменений.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Флаги назначения дорожки (disposition) с именованными полями вместо позиционных bool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TrackDisposition {
    /// Контейнер пометил дорожку как выбираемую по умолчанию.
    pub is_default: bool,

    /// Дорожка «forced»: только реплики, которые нужно показывать всегда (иноязычные вставки).
    pub is_forced: bool,

    /// Дорожка для слабослышащих (SDH / hearing impaired).
    pub is_hearing_impaired: bool,

    /// Дорожка с комментариями режиссёра и т.п.
    pub is_commentary: bool,
}

/// Метки дорожки: язык, название и флаги назначения.
///
/// `Default` означает «контейнер ничего не сообщил».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackLabels {
    /// Язык дорожки, если контейнер его указал.
    pub language: Option<TrackLanguage>,

    /// Название дорожки (например, MKV `Name`), если контейнер его указал.
    pub title: Option<String>,

    /// Флаги назначения дорожки.
    pub disposition: TrackDisposition,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_labels_report_nothing_known() {
        let labels = TrackLabels::default();

        assert_eq!(labels.language, None);
        assert_eq!(labels.title, None);
        assert_eq!(labels.disposition, TrackDisposition::default());
        assert!(!labels.disposition.is_default && !labels.disposition.is_forced);
    }

    #[test]
    fn language_keeps_raw_container_string_unchanged() {
        assert_eq!(TrackLanguage::new("pt-BR").as_str(), "pt-BR");
        assert_eq!(TrackLanguage::new("RUS").as_str(), "RUS");
    }
}
