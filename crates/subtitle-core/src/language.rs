//! Нормализованный BCP 47 языковой тег дорожки субтитров.

use oxilangtag::LanguageTag;

use crate::language_table;

/// Некорректный языковой тег.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid subtitle language tag `{tag}`: {reason}")]
pub struct InvalidSubtitleLanguage {
    tag: String,
    reason: String,
}

impl InvalidSubtitleLanguage {
    /// Исходный текст тега.
    pub fn tag(&self) -> &str {
        &self.tag
    }
}

/// Валидный язык в канонической регистровой форме (`en-US`, `ru`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubtitleLanguage(LanguageTag<String>);

impl SubtitleLanguage {
    /// Разбирает и нормализует тег; невалидный тег — ошибка, а не молчаливый пропуск.
    pub fn parse(tag: &str) -> Result<Self, InvalidSubtitleLanguage> {
        LanguageTag::parse_and_normalize(tag)
            .map(Self)
            .map_err(|error| InvalidSubtitleLanguage {
                tag: tag.to_owned(),
                reason: error.to_string(),
            })
    }

    /// Полный нормализованный тег.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Основной язык без региона и скрипта (`en` для `en-US`).
    pub fn primary_language(&self) -> &str {
        self.0.primary_language()
    }

    /// Основной язык в канонической форме: известный 3-буквенный код (`rus`, `ger`) приводится
    /// к двухбуквенному (`ru`, `de`), неизвестный остаётся в нижнем регистре. Именно эту форму
    /// нужно сравнивать с предпочтениями пользователя.
    pub fn canonical_primary_language(&self) -> String {
        language_table::canonical_primary_language(self.primary_language())
    }

    /// Английское название языка для меню (`Russian`); `None` — языка нет в таблице. Регион
    /// и скрипт в название не входят.
    pub fn english_display_name(&self) -> Option<&'static str> {
        language_table::english_name(self.primary_language())
    }

    /// Можно ли принять язык из ИМЕНИ ФАЙЛА: только частые языки, чтобы случайное слово из
    /// двух-трёх букв в названии не стало «языком» (см. `language_table`).
    pub fn is_common_sidecar_language(&self) -> bool {
        language_table::is_common_sidecar_language(self.primary_language())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_normalized_to_canonical_case() {
        let language = SubtitleLanguage::parse("en-us").expect("valid tag");
        assert_eq!(language.as_str(), "en-US");
        assert_eq!(language.primary_language(), "en");
    }

    #[test]
    fn simple_tag_keeps_primary_language() {
        let language = SubtitleLanguage::parse("RU").expect("valid tag");
        assert_eq!(language.as_str(), "ru");
        assert_eq!(language.primary_language(), "ru");
    }

    #[test]
    fn invalid_tag_is_rejected_with_original_text() {
        let error = SubtitleLanguage::parse("not a tag!").expect_err("invalid tag");
        assert_eq!(error.tag(), "not a tag!");
    }

    #[test]
    fn three_letter_sidecar_code_matches_the_two_letter_preference() {
        let sidecar = SubtitleLanguage::parse("rus").expect("valid tag");
        let preference = SubtitleLanguage::parse("ru-RU").expect("valid tag");
        assert_eq!(sidecar.canonical_primary_language(), "ru");
        assert_eq!(
            sidecar.canonical_primary_language(),
            preference.canonical_primary_language()
        );
        assert_eq!(sidecar.english_display_name(), Some("Russian"));
        assert_eq!(preference.english_display_name(), Some("Russian"));
    }

    #[test]
    fn equal_after_normalization() {
        assert_eq!(
            SubtitleLanguage::parse("pt-br").expect("valid"),
            SubtitleLanguage::parse("pt-BR").expect("valid")
        );
    }
}
