//! Небольшая статическая таблица распространённых языков: коды ISO 639-1/639-2 и английское
//! название. Нужна, чтобы `rus` из имени файла `Movie.rus.srt` совпадал с предпочтением `ru`,
//! а меню могло показать «Russian». Таблица намеренно мала: редкие языки показываются
//! сырым тегом, это честнее, чем угадывать название.

/// Одна строка таблицы.
struct LanguageRow {
    /// Двухбуквенный код ISO 639-1 (каноническая форма).
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

/// Таблица языков (порядок значения не имеет).
static LANGUAGES: &[LanguageRow] = &[
    row("ar", "ara", "ara", "Arabic"),
    row("bg", "bul", "bul", "Bulgarian"),
    row("bn", "ben", "ben", "Bengali"),
    row("ca", "cat", "cat", "Catalan"),
    row("cs", "cze", "ces", "Czech"),
    row("da", "dan", "dan", "Danish"),
    row("de", "ger", "deu", "German"),
    row("el", "gre", "ell", "Greek"),
    row("en", "eng", "eng", "English"),
    row("es", "spa", "spa", "Spanish"),
    row("et", "est", "est", "Estonian"),
    row("fa", "per", "fas", "Persian"),
    row("fi", "fin", "fin", "Finnish"),
    row("fr", "fre", "fra", "French"),
    row("he", "heb", "heb", "Hebrew"),
    row("hi", "hin", "hin", "Hindi"),
    row("hr", "hrv", "hrv", "Croatian"),
    row("hu", "hun", "hun", "Hungarian"),
    row("id", "ind", "ind", "Indonesian"),
    row("is", "ice", "isl", "Icelandic"),
    row("it", "ita", "ita", "Italian"),
    row("ja", "jpn", "jpn", "Japanese"),
    row("ka", "geo", "kat", "Georgian"),
    row("kk", "kaz", "kaz", "Kazakh"),
    row("ko", "kor", "kor", "Korean"),
    row("lt", "lit", "lit", "Lithuanian"),
    row("lv", "lav", "lav", "Latvian"),
    row("ms", "may", "msa", "Malay"),
    row("nl", "dut", "nld", "Dutch"),
    row("no", "nor", "nor", "Norwegian"),
    row("pl", "pol", "pol", "Polish"),
    row("pt", "por", "por", "Portuguese"),
    row("ro", "rum", "ron", "Romanian"),
    row("ru", "rus", "rus", "Russian"),
    row("sk", "slo", "slk", "Slovak"),
    row("sl", "slv", "slv", "Slovenian"),
    row("sr", "srp", "srp", "Serbian"),
    row("sv", "swe", "swe", "Swedish"),
    row("th", "tha", "tha", "Thai"),
    row("tr", "tur", "tur", "Turkish"),
    row("uk", "ukr", "ukr", "Ukrainian"),
    row("vi", "vie", "vie", "Vietnamese"),
    row("zh", "chi", "zho", "Chinese"),
];

/// Находит строку по основному подтегу (регистр не важен): принимает 2-буквенный код и оба
/// 3-буквенных варианта.
fn find_row(primary_subtag: &str) -> Option<&'static LanguageRow> {
    LANGUAGES.iter().find(|language| {
        [
            language.alpha2,
            language.alpha3_bibliographic,
            language.alpha3_terminologic,
        ]
        .iter()
        .any(|code| code.eq_ignore_ascii_case(primary_subtag))
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
        assert!(LANGUAGES.len() >= 40);
    }
}
