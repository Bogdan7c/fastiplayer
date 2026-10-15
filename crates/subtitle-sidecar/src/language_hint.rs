//! Подсказки из имени файла: язык, forced, SDH и «человеческое» название.

use subtitle_core::{SubtitleLanguage, SubtitleTrackDisposition};

/// Английские названия языков → BCP 47 (без учёта регистра). Намеренно маленькая таблица.
const LANGUAGE_NAMES: [(&str, &str); 14] = [
    ("english", "en"),
    ("russian", "ru"),
    ("german", "de"),
    ("french", "fr"),
    ("spanish", "es"),
    ("italian", "it"),
    ("japanese", "ja"),
    ("chinese", "zh"),
    ("korean", "ko"),
    ("ukrainian", "uk"),
    ("polish", "pl"),
    ("portuguese", "pt"),
    ("arabic", "ar"),
    ("hebrew", "he"),
];

/// Токены-метки слабослышащих: только запоминаем, на выбор дорожки не влияют.
const HEARING_IMPAIRED_TOKENS: [&str; 3] = ["sdh", "cc", "hi"];

/// Что удалось понять из части имени файла.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NameHints {
    pub(crate) language: Option<SubtitleLanguage>,
    pub(crate) disposition: SubtitleTrackDisposition,
    pub(crate) is_hearing_impaired: bool,
    pub(crate) title: Option<String>,
}

/// Разбирает суффикс имени (`.ru`, ` [Forced]`, `2_English`) на подсказки.
///
/// Разделители токенов: `. _ пробел [ ] ( )`. Дефис внутри токена сохраняется, чтобы
/// распознать `pt-BR`/`zh-Hans`; если токен с дефисом не тег — он делится по дефисам.
/// Язык берётся из ПЕРВОГО распознанного токена; метки forced/sdh/cc/hi проверяются раньше
/// кодов (`hi` — ещё и хинди, но трактуется как метка). Двух- и трёхбуквенный токен
/// принимается за код языка, поэтому редкое слово из трёх букв может дать ложный язык.
pub(crate) fn parse_name_hints(name_part: &str) -> NameHints {
    let mut hints = NameHints::default();
    let mut title_tokens: Vec<&str> = Vec::new();
    for token in name_part
        .split(['.', '_', ' ', '[', ']', '(', ')'])
        .filter(|token| !token.is_empty())
    {
        if !consume_token(token, &mut hints) {
            // Токен с дефисом мог быть составным («Full-Subs»): пробуем части по отдельности.
            // Все части обязаны пройти consume_token (у него есть побочный эффект на hints).
            let mut parts_consumed = false;
            if token.contains('-') {
                for part in token.split('-').filter(|part| !part.is_empty()) {
                    parts_consumed |= consume_token(part, &mut hints);
                }
            }
            if !parts_consumed {
                title_tokens.push(token);
            }
        }
    }
    if !title_tokens.is_empty() {
        hints.title = Some(title_tokens.join(" "));
    }
    hints
}

/// Пытается использовать токен как метку или язык; `true`, если токен «съеден».
fn consume_token(token: &str, hints: &mut NameHints) -> bool {
    let lowered = token.to_ascii_lowercase();
    if lowered == "forced" {
        hints.disposition.is_forced = true;
        return true;
    }
    if HEARING_IMPAIRED_TOKENS.contains(&lowered.as_str()) {
        hints.is_hearing_impaired = true;
        return true;
    }
    if hints.language.is_some() {
        return false;
    }
    match language_from_token(token, &lowered) {
        Some(language) => {
            hints.language = Some(language);
            true
        }
        None => false,
    }
}

fn language_from_token(token: &str, lowered: &str) -> Option<SubtitleLanguage> {
    if let Some((_, code)) = LANGUAGE_NAMES.iter().find(|(name, _)| *name == lowered) {
        return SubtitleLanguage::parse(code).ok();
    }
    let primary_part = token.split('-').next().unwrap_or(token);
    let looks_like_code = matches!(primary_part.len(), 2 | 3)
        && primary_part.bytes().all(|byte| byte.is_ascii_alphabetic());
    if looks_like_code {
        SubtitleLanguage::parse(token).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn language_of(name_part: &str) -> Option<String> {
        parse_name_hints(name_part)
            .language
            .map(|language| language.as_str().to_owned())
    }

    #[test]
    fn codes_with_region_and_script_are_recognized() {
        assert_eq!(language_of(".ru"), Some("ru".to_owned()));
        assert_eq!(language_of("_pt-br"), Some("pt-BR".to_owned()));
        assert_eq!(language_of(".zh-Hans"), Some("zh-Hans".to_owned()));
        assert_eq!(language_of(".eng"), Some("eng".to_owned()));
    }

    #[test]
    fn english_names_map_to_codes_case_insensitively() {
        assert_eq!(language_of("2_English"), Some("en".to_owned()));
        assert_eq!(language_of(" RUSSIAN"), Some("ru".to_owned()));
        assert_eq!(language_of(".Hebrew"), Some("he".to_owned()));
    }

    #[test]
    fn forced_and_hearing_impaired_markers_are_recorded() {
        let hints = parse_name_hints(" [Forced].en.SDH");
        assert!(hints.disposition.is_forced);
        assert!(hints.is_hearing_impaired);
        assert_eq!(
            hints.language.map(|l| l.as_str().to_owned()),
            Some("en".to_owned())
        );
        assert_eq!(hints.title, None);
        // «hi» — метка слабослышащих, а не хинди.
        let hindi_marker = parse_name_hints(".hi");
        assert!(hindi_marker.is_hearing_impaired);
        assert_eq!(hindi_marker.language, None);
    }

    #[test]
    fn unknown_tokens_become_title_and_digits_are_not_languages() {
        let hints = parse_name_hints("2_Commentary_Track");
        assert_eq!(hints.language, None);
        assert_eq!(hints.title.as_deref(), Some("2 Commentary Track"));
    }

    #[test]
    fn empty_suffix_has_no_hints() {
        assert_eq!(parse_name_hints(""), NameHints::default());
    }
}
