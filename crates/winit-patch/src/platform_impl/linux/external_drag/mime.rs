//! Выбор лучшего формата данных из предложенных источником drag & drop.
//!
//! Правило одно для Wayland (`wl_data_offer`) и X11 (список атомов `XdndTypeList`):
//! берём самый информативный формат, который умеем разобрать.
//!
//! Порядок предпочтения:
//! 1. `text/uri-list` — файлы и ссылки в виде RFC 3986 URI;
//! 2. `text/x-moz-url` — ссылка из браузера (UTF-16, строки «url / заголовок»);
//! 3. `text/plain;charset=utf-8`;
//! 4. `UTF8_STRING` (X11-имя того же простого текста);
//! 5. `text/plain` (без указания кодировки считаем UTF-8).

/// Вид данных, который мы умеем разбирать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadFormat {
    /// `text/uri-list`: по URI на строку, `#` — комментарии.
    UriList,
    /// `text/x-moz-url`: UTF-16, чередуются строки «url» и «заголовок».
    MozUrl,
    /// Простой текст в UTF-8.
    PlainText,
}

/// Результат выбора: какой формат и под каким именем его просить у источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MimeChoice<'offered> {
    /// Как разбирать полученные байты.
    pub(crate) format: PayloadFormat,
    /// Точное имя типа из предложения источника (его нужно передать в `receive`).
    pub(crate) offered_name: &'offered str,
}

/// Классифицирует одно имя типа; `None` — формат нам не интересен.
///
/// Возвращает формат и ранг (меньше — лучше).
fn classify(offered_name: &str) -> Option<(PayloadFormat, u8)> {
    // Имя типа регистронезависимо, пробелы вокруг параметров не значимы.
    let normalized: String = offered_name
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();

    match normalized.as_str() {
        "text/uri-list" => Some((PayloadFormat::UriList, 0)),
        "text/x-moz-url" => Some((PayloadFormat::MozUrl, 1)),
        "text/plain;charset=utf-8" => Some((PayloadFormat::PlainText, 2)),
        "utf8_string" => Some((PayloadFormat::PlainText, 3)),
        "text/plain" => Some((PayloadFormat::PlainText, 4)),
        _ => None,
    }
}

/// Выбирает лучший поддерживаемый формат из предложенных типов.
///
/// `None` означает, что нет ни одного нужного формата: жест надо отклонить.
/// При равном ранге побеждает тип, предложенный раньше.
pub(crate) fn choose_mime<'offered>(
    offered_names: &'offered [String],
) -> Option<MimeChoice<'offered>> {
    offered_names
        .iter()
        .filter_map(|name| classify(name).map(|(format, rank)| (rank, format, name.as_str())))
        .min_by_key(|(rank, ..)| *rank)
        .map(|(_, format, offered_name)| MimeChoice { format, offered_name })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn uri_list_wins_over_everything() {
        let offered = names(&["text/plain", "text/x-moz-url", "text/uri-list", "UTF8_STRING"]);

        let choice = choose_mime(&offered).expect("choice");

        assert_eq!(
            choice,
            MimeChoice { format: PayloadFormat::UriList, offered_name: "text/uri-list" }
        );
    }

    #[test]
    fn moz_url_beats_plain_text_when_uri_list_is_absent() {
        let offered = names(&["text/plain", "text/x-moz-url"]);

        let choice = choose_mime(&offered).expect("choice");

        assert_eq!(choice.format, PayloadFormat::MozUrl);
        assert_eq!(choice.offered_name, "text/x-moz-url");
    }

    #[test]
    fn plain_text_preference_order_is_utf8_charset_then_utf8_string_then_bare() {
        let all = names(&["text/plain", "UTF8_STRING", "text/plain;charset=utf-8"]);
        assert_eq!(choose_mime(&all).expect("choice").offered_name, "text/plain;charset=utf-8");

        let without_charset = names(&["text/plain", "UTF8_STRING"]);
        assert_eq!(choose_mime(&without_charset).expect("choice").offered_name, "UTF8_STRING");

        let bare = names(&["text/plain"]);
        assert_eq!(choose_mime(&bare).expect("choice").format, PayloadFormat::PlainText);
    }

    #[test]
    fn charset_parameter_is_matched_case_and_space_insensitively() {
        let offered = names(&["text/plain; charset=UTF-8"]);

        let choice = choose_mime(&offered).expect("choice");

        assert_eq!(choice.format, PayloadFormat::PlainText);
        // Источнику возвращается исходное написание, а не нормализованное.
        assert_eq!(choice.offered_name, "text/plain; charset=UTF-8");
    }

    #[test]
    fn unsupported_types_are_rejected() {
        let offered = names(&["image/png", "application/x-kde-cutselection", "text/html"]);

        assert_eq!(choose_mime(&offered), None);
        assert_eq!(choose_mime(&[]), None);
    }
}
