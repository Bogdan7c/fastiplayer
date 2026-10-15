//! Правила имён: совпадение основы файла с основой видео и имена `Subs`-подобных папок.

use std::ffi::OsStr;

/// Имена папок с субтитрами (сравнение ASCII без учёта регистра).
const SUBTITLE_DIRECTORY_NAMES: [&str; 4] = ["subs", "sub", "subtitles", "subtitle"];

/// Результат сравнения основы файла с основой видео.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StemMatch {
    /// Основа файла не начинается с основы видео или после неё нет разделителя.
    NoMatch,
    /// Основы равны (`Movie.srt` для `Movie.mkv`).
    Exact,
    /// После основы видео идёт разделитель и суффикс (`.ru`, ` [Forced]`); суффикс — строка
    /// только для подсказок языка (невалидные байты заменены), на пути он не влияет.
    WithSuffix(String),
}

/// Папка с таким именем считается папкой субтитров (`Subs`, `subtitles`, …).
pub(crate) fn is_subtitle_directory_name(name: &OsStr) -> bool {
    SUBTITLE_DIRECTORY_NAMES
        .iter()
        .any(|known_name| name.eq_ignore_ascii_case(known_name))
}

/// Имя папки равно основе видео (для `Subs/<Movie>/`), без учёта регистра.
pub(crate) fn is_movie_named_directory(name: &OsStr, video_stem: &OsStr) -> bool {
    matches!(match_video_stem(name, video_stem), StemMatch::Exact)
}

/// Сравнивает основу файла с основой видео по правилам решения 13.
///
/// Регистр: если обе основы валидный UTF-8 — сравнение через `to_lowercase` посимвольно
/// (простое Unicode-сворачивание, длина строки при этом может меняться, поэтому идём по
/// символам, а не по байтам). Если хоть одна не UTF-8 — только точный байтовый префикс.
pub(crate) fn match_video_stem(file_stem: &OsStr, video_stem: &OsStr) -> StemMatch {
    let file_bytes = os_str_bytes(file_stem);
    let video_bytes = os_str_bytes(video_stem);
    match (
        std::str::from_utf8(&file_bytes),
        std::str::from_utf8(&video_bytes),
    ) {
        (Ok(file_text), Ok(video_text)) => match_utf8(file_text, video_text),
        _ => match_exact_bytes(&file_bytes, &video_bytes),
    }
}

fn match_utf8(file_text: &str, video_text: &str) -> StemMatch {
    let mut expected = video_text.chars().flat_map(char::to_lowercase).peekable();
    let mut remainder_start = file_text.len();
    for (offset, file_char) in file_text.char_indices() {
        if expected.peek().is_none() {
            // Основа видео полностью покрыта: остаток начинается с этого символа.
            remainder_start = offset;
            break;
        }
        for lowered in file_char.to_lowercase() {
            if expected.next() != Some(lowered) {
                return StemMatch::NoMatch;
            }
        }
    }
    if expected.peek().is_some() {
        // Файл короче основы видео (или свёртка закончилась посреди символа).
        return StemMatch::NoMatch;
    }
    classify_remainder(&file_text[remainder_start..])
}

fn match_exact_bytes(file_bytes: &[u8], video_bytes: &[u8]) -> StemMatch {
    match file_bytes.strip_prefix(video_bytes) {
        Some(remainder) => classify_remainder(&String::from_utf8_lossy(remainder)),
        None => StemMatch::NoMatch,
    }
}

/// Остаток после основы: пусто — точное совпадение; первый символ обязан быть разделителем.
fn classify_remainder(remainder: &str) -> StemMatch {
    match remainder.chars().next() {
        None => StemMatch::Exact,
        Some('.' | '_' | '-' | ' ' | '[' | '(') => StemMatch::WithSuffix(remainder.to_owned()),
        Some(_) => StemMatch::NoMatch,
    }
}

/// Байты имени без потерь на unix; на остальных платформах имя приводится к UTF-8 с
/// заменой (только для сравнения — сам путь остаётся `PathBuf`).
fn os_str_bytes(name: &OsStr) -> std::borrow::Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::borrow::Cow::Borrowed(name.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::borrow::Cow::Owned(name.to_string_lossy().into_owned().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matched(file: &str, video: &str) -> StemMatch {
        match_video_stem(OsStr::new(file), OsStr::new(video))
    }

    #[test]
    fn equal_and_separated_suffixes_match() {
        assert_eq!(matched("Movie", "Movie"), StemMatch::Exact);
        assert_eq!(matched("movie", "MOVIE"), StemMatch::Exact);
        for suffix in [".ru", "_en", "-en", " en", " [Forced]", "(x)", "[x]"] {
            let name = format!("Movie{suffix}");
            assert_eq!(
                matched(&name, "Movie"),
                StemMatch::WithSuffix(suffix.to_owned())
            );
        }
    }

    #[test]
    fn non_separator_continuation_or_short_name_does_not_match() {
        assert_eq!(matched("Movie2", "Movie"), StemMatch::NoMatch);
        assert_eq!(matched("Movies", "Movie"), StemMatch::NoMatch);
        assert_eq!(matched("Mov", "Movie"), StemMatch::NoMatch);
        assert_eq!(matched("Other", "Movie"), StemMatch::NoMatch);
    }

    #[test]
    fn unicode_case_folding_matches_cyrillic() {
        assert_eq!(
            matched("ФИЛЬМ.ru", "Фильм"),
            StemMatch::WithSuffix(".ru".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_use_exact_byte_prefix() {
        use std::os::unix::ffi::OsStrExt;
        let video = OsStr::from_bytes(b"Mo\xFFvie");
        let same_case = OsStr::from_bytes(b"Mo\xFFvie.en");
        let other_case = OsStr::from_bytes(b"MO\xFFVIE.en");
        assert_eq!(
            match_video_stem(same_case, video),
            StemMatch::WithSuffix(".en".to_owned())
        );
        assert_eq!(match_video_stem(other_case, video), StemMatch::NoMatch);
    }

    #[test]
    fn subtitle_directory_names_ignore_ascii_case() {
        for name in ["Subs", "SUB", "subtitles", "Subtitle"] {
            assert!(is_subtitle_directory_name(OsStr::new(name)), "{name}");
        }
        assert!(!is_subtitle_directory_name(OsStr::new("Extras")));
        assert!(!is_subtitle_directory_name(OsStr::new("Subs2")));
    }

    #[test]
    fn movie_named_directory_requires_equal_name() {
        assert!(is_movie_named_directory(
            OsStr::new("MOVIE"),
            OsStr::new("Movie")
        ));
        assert!(!is_movie_named_directory(
            OsStr::new("Movie.en"),
            OsStr::new("Movie")
        ));
    }
}
