//! Статическая таблица языков: все коды ISO 639-1 с английскими названиями, их трёхбуквенные
//! варианты 639-2/B и 639-2/T, несколько языков только с трёхбуквенным кодом (`fil`, `yue`…) и
//! устаревшие коды-синонимы, которые до сих пор отдаёт YouTube (`iw`, `in`, `ji`, `jw`).
//!
//! Таблица нужна, чтобы `rus` из имени файла совпадал с предпочтением `ru`, а меню показывало
//! «Russian», «Telugu», «Hebrew», а не сырой тег. Сами строки лежат в `rows`.
//!
//! # Два набора языков
//! - Полная таблица: названия и канонизация кодов. Здесь безопасно знать всё, потому что тег
//!   пришёл из контейнера или от yt-dlp и уже известен как тег.
//! - Короткий набор `is_common_sidecar_language`: только для догадки о языке по ИМЕНИ ФАЙЛА.
//!   В полной таблице много трёхбуквенных кодов, совпадающих с обычными словами (`ton`, `man`),
//!   и `The_Big_Ton.srt` не должен стать тонганским. Поэтому имя файла сверяется с прежним
//!   набором из 42 самых частых языков.

mod rows;

use rows::LANGUAGES;

/// Одна строка таблицы.
struct LanguageRow {
    /// Каноническая форма: двухбуквенный код ISO 639-1; для языков без него — трёхбуквенный.
    alpha2: &'static str,
    /// Трёхбуквенный библиографический код ISO 639-2/B.
    alpha3_bibliographic: &'static str,
    /// Трёхбуквенный терминологический код ISO 639-2/T.
    alpha3_terminologic: &'static str,
    /// Английское название языка.
    english_name: &'static str,
}

const fn row(
    alpha2: &'static str,
    alpha3_bibliographic: &'static str,
    alpha3_terminologic: &'static str,
    english_name: &'static str,
) -> LanguageRow {
    LanguageRow {
        alpha2,
        alpha3_bibliographic,
        alpha3_terminologic,
        english_name,
    }
}

/// Устаревшие коды ISO 639-1, которые всё ещё встречаются в данных YouTube/Java:
/// (устаревший код, каноническая форма). `mo` — молдавский, объединённый с румынским.
static LEGACY_ALIASES: &[(&str, &str)] = &[
    ("iw", "he"),
    ("in", "id"),
    ("ji", "yi"),
    ("jw", "jv"),
    ("mo", "ro"),
];

/// Языки, чей код разрешено угадывать из имени файла (прежний набор из 42 языков).
static COMMON_SIDECAR_LANGUAGES: &[&str] = &[
    "ar", "bg", "bn", "ca", "cs", "da", "de", "el", "en", "es", "et", "fa", "fi", "fr", "he", "hi",
    "hr", "hu", "id", "is", "it", "ja", "ka", "kk", "ko", "lt", "lv", "ms", "nl", "no", "pl", "pt",
    "ro", "ru", "sk", "sl", "sr", "sv", "th", "tr", "uk", "vi", "zh",
];

/// Находит строку по основному подтегу (регистр не важен): принимает 2-буквенный код, оба
/// 3-буквенных варианта и устаревшие синонимы.
fn find_row(primary_subtag: &str) -> Option<&'static LanguageRow> {
    let canonical_code = LEGACY_ALIASES
        .iter()
        .find(|(legacy, _)| legacy.eq_ignore_ascii_case(primary_subtag))
        .map_or(primary_subtag, |(_, canonical)| *canonical);
    LANGUAGES.iter().find(|language| {
        [
            language.alpha2,
            language.alpha3_bibliographic,
            language.alpha3_terminologic,
        ]
        .iter()
        .any(|code| code.eq_ignore_ascii_case(canonical_code))
    })
}

/// Каноническая форма основного языка: известный код (в том числе 3-буквенный) приводится к
/// двухбуквенному, неизвестный — просто нижний регистр.
pub(crate) fn canonical_primary_language(primary_subtag: &str) -> String {
    match find_row(primary_subtag) {
        Some(language) => language.alpha2.to_owned(),
        None => primary_subtag.to_ascii_lowercase(),
    }
}

/// Английское название основного языка, если он есть в таблице.
pub(crate) fn english_name(primary_subtag: &str) -> Option<&'static str> {
    find_row(primary_subtag).map(|language| language.english_name)
}

/// Можно ли принять этот код как язык из ИМЕНИ ФАЙЛА (см. «Два набора языков» выше).
pub(crate) fn is_common_sidecar_language(primary_subtag: &str) -> bool {
    let canonical = canonical_primary_language(primary_subtag);
    COMMON_SIDECAR_LANGUAGES.contains(&canonical.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_letter_codes_of_both_kinds_map_to_the_two_letter_code() {
        assert_eq!(canonical_primary_language("rus"), "ru");
        assert_eq!(canonical_primary_language("ger"), "de", "bibliographic");
        assert_eq!(canonical_primary_language("deu"), "de", "terminologic");
        assert_eq!(canonical_primary_language("CHI"), "zh");
        assert_eq!(canonical_primary_language("ukr"), "uk");
    }

    #[test]
    fn two_letter_codes_are_kept_and_unknown_codes_are_lowercased() {
        assert_eq!(canonical_primary_language("EN"), "en");
        assert_eq!(canonical_primary_language("TLH"), "tlh");
    }

    #[test]
    fn english_names_are_found_by_any_code_form() {
        assert_eq!(english_name("ru"), Some("Russian"));
        assert_eq!(english_name("deu"), Some("German"));
        assert_eq!(english_name("ger"), Some("German"));
        assert_eq!(english_name("zho"), Some("Chinese"));
        assert_eq!(english_name("tlh"), None);
    }

    #[test]
    fn table_has_no_code_shared_between_languages() {
        let mut seen = std::collections::HashSet::new();
        for language in LANGUAGES {
            // Внутри строки B и T коды часто совпадают — это не дубль.
            let own_codes: std::collections::BTreeSet<&str> = [
                language.alpha2,
                language.alpha3_bibliographic,
                language.alpha3_terminologic,
            ]
            .into_iter()
            .collect();
            for code in own_codes {
                assert!(seen.insert(code), "code {code} belongs to two languages");
            }
        }
        for (legacy, canonical) in LEGACY_ALIASES {
            assert!(
                seen.insert(legacy),
                "alias {legacy} collides with a real code"
            );
            assert!(
                LANGUAGES
                    .iter()
                    .any(|language| language.alpha2 == *canonical),
                "alias {legacy} points to unknown language {canonical}"
            );
        }
        assert!(
            LANGUAGES.len() >= 180,
            "all ISO 639-1 languages must be present"
        );
    }

    #[test]
    fn youtube_dub_languages_have_english_names() {
        assert_eq!(english_name("te"), Some("Telugu"));
        assert_eq!(english_name("ta"), Some("Tamil"));
        assert_eq!(english_name("pa"), Some("Punjabi"));
        assert_eq!(english_name("ml"), Some("Malayalam"));
        assert_eq!(english_name("fil"), Some("Filipino"));
        assert_eq!(
            english_name("tel"),
            Some("Telugu"),
            "alpha-3 of a new language"
        );
    }

    #[test]
    fn legacy_aliases_are_the_same_language_as_the_modern_code() {
        for (legacy, modern, name) in [
            ("iw", "he", "Hebrew"),
            ("in", "id", "Indonesian"),
            ("ji", "yi", "Yiddish"),
            ("jw", "jv", "Javanese"),
        ] {
            assert_eq!(canonical_primary_language(legacy), modern);
            assert_eq!(canonical_primary_language(&legacy.to_uppercase()), modern);
            assert_eq!(english_name(legacy), Some(name));
            assert_eq!(english_name(modern), Some(name));
        }
    }

    #[test]
    fn filename_detection_stays_limited_to_the_common_set() {
        assert!(is_common_sidecar_language("ru"));
        assert!(is_common_sidecar_language("rus"));
        assert!(
            is_common_sidecar_language("iw"),
            "alias of a common language"
        );
        assert!(
            !is_common_sidecar_language("ton"),
            "Tongan looks like a word"
        );
        assert!(!is_common_sidecar_language("te"));
        assert!(!is_common_sidecar_language("lie"));
    }
}
