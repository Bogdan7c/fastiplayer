//! Язык системы пользователя: запасной язык субтитров, когда дорожка своего не объявляет.
//!
//! Чтение окружения (`read_system_language`) вызывается ОДИН раз при сборке состояния
//! приложения; на покадровом пути окружение не читается. Разбор (`parse_posix_locale`,
//! `system_language_from_values`) чистый и тестируется без окружения.

use subtitle_core::SubtitleLanguage;

/// Значения переменных локали в порядке приоритета POSIX: `LC_ALL`, `LC_MESSAGES`, `LANG`.
const LOCALE_VARIABLES: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANG"];

/// Читает язык системы из окружения процесса; `None` — язык неизвестен (локаль `C`/`POSIX`,
/// переменные не заданы или не разобрались).
pub(in crate::state) fn read_system_language() -> Option<SubtitleLanguage> {
    let [lc_all, lc_messages, lang] = LOCALE_VARIABLES.map(|name| {
        // Значение не в UTF-8 для нас равносильно «не задано»: разобрать его нечем.
        std::env::var_os(name).and_then(|value| value.into_string().ok())
    });
    system_language_from_values(lc_all.as_deref(), lc_messages.as_deref(), lang.as_deref())
}

/// Язык системы по значениям переменных (чистая функция).
///
/// По правилу POSIX решает первая НЕпустая переменная из `LC_ALL`, `LC_MESSAGES`, `LANG`;
/// если она `C`/`POSIX`, язык неизвестен (к следующей переменной не переходим: пользователь
/// явно выбрал нейтральную локаль).
pub(super) fn system_language_from_values(
    lc_all: Option<&str>,
    lc_messages: Option<&str>,
    lang: Option<&str>,
) -> Option<SubtitleLanguage> {
    [lc_all, lc_messages, lang]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .and_then(parse_posix_locale)
}

/// Разбирает имя локали POSIX (`ja_JP.UTF-8`, `zh_TW@euro`, `sr_RS.UTF-8@latin`) в тег BCP 47
/// (`ja-JP`, `zh-TW`). `C`, `POSIX` и нераспознанное дают `None`.
pub(super) fn parse_posix_locale(locale: &str) -> Option<SubtitleLanguage> {
    // Отбрасываем кодировку (`.UTF-8`) и модификатор (`@latin`): они не язык.
    let language_part = locale
        .split(['.', '@'])
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())?;
    if language_part == "C" || language_part == "POSIX" {
        return None;
    }
    SubtitleLanguage::parse(&language_part.replace('_', "-")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_of(locale: &str) -> Option<String> {
        parse_posix_locale(locale).map(|language| language.as_str().to_owned())
    }

    #[test]
    fn locale_names_become_language_tags() {
        assert_eq!(tag_of("ja_JP.UTF-8"), Some("ja-JP".to_owned()));
        assert_eq!(tag_of("zh_TW"), Some("zh-TW".to_owned()));
        assert_eq!(tag_of("zh_CN.GB2312"), Some("zh-CN".to_owned()));
        assert_eq!(tag_of("ru_UA.UTF-8"), Some("ru-UA".to_owned()));
        assert_eq!(tag_of("en"), Some("en".to_owned()));
        assert_eq!(tag_of("sr_RS.UTF-8@latin"), Some("sr-RS".to_owned()));
    }

    #[test]
    fn neutral_and_broken_locales_are_unknown() {
        assert_eq!(tag_of("C"), None);
        assert_eq!(tag_of("POSIX"), None);
        assert_eq!(tag_of("C.UTF-8"), None);
        assert_eq!(tag_of(""), None);
        assert_eq!(tag_of(".UTF-8"), None);
        assert_eq!(tag_of("not a locale!"), None);
    }

    #[test]
    fn first_non_empty_variable_decides() {
        let tag = |all, messages, lang| {
            system_language_from_values(all, messages, lang)
                .map(|language| language.as_str().to_owned())
        };
        assert_eq!(
            tag(Some("ja_JP.UTF-8"), Some("ru_RU"), Some("en_US")),
            Some("ja-JP".to_owned())
        );
        assert_eq!(
            tag(Some(""), Some("ko_KR.UTF-8"), Some("en_US")),
            Some("ko-KR".to_owned())
        );
        assert_eq!(
            tag(None, None, Some("zh_HK.UTF-8")),
            Some("zh-HK".to_owned())
        );
        assert_eq!(tag(None, None, None), None);
    }

    #[test]
    fn c_locale_in_a_higher_priority_variable_is_not_overridden_by_lang() {
        let language = system_language_from_values(Some("C"), None, Some("ja_JP.UTF-8"));
        assert_eq!(language, None);
    }
}
