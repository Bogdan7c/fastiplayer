//! Общий декодер текста `.m3u`/`.cue`: UTF-8 с запасным вариантом Windows-1251.
//!
//! # Решение владельца (UX edge cases 2, B1)
//!
//! Раньше crate принимал только UTF-8 и сознательно не угадывал legacy code page.
//! Но плейлисты и CUE-листы, созданные на Windows (Winamp, foobar2000, EAC),
//! массово лежат в Windows-1251, и честный отказ «не UTF-8» оставлял пользователя
//! ни с чем. Поэтому порядок теперь такой:
//!
//! 1. UTF-8 (с BOM или без) — как и раньше, без каких-либо уведомлений;
//! 2. если байты не являются валидным UTF-8 — декодируем как Windows-1251 и
//!    сообщаем об этом типизированным [`PlaylistTextEncoding::Windows1251`],
//!    чтобы приложение показало короткое уведомление.
//!
//! Это не «угадывание» в смысле детекторов кодировок: набор фиксирован (ровно две
//! кодировки), порядок детерминирован, а Windows-1251 — однобайтная кодировка, у
//! которой любая последовательность байтов декодируется без ошибок.
//!
//! # Исключения
//!
//! - `.m3u8` по спецификации (RFC 8216, HLS) всегда UTF-8, поэтому для него запасной
//!   вариант запрещён ([`LegacyFallback::Forbidden`]) и невалидный UTF-8 остаётся
//!   честной ошибкой.
//! - XSPF — это XML: кодировку объявляет сам документ и разбирает XML-граница
//!   (`bounded-xml-reader`), поэтому здесь XSPF не трогаем.
//! - UTF-16 для `.cue` с BOM поддерживался и раньше (см. `cue/encoding.rs`); M3U
//!   UTF-16 по-прежнему не поддерживает. Не расширяем без необходимости.
//!
//! # Пути внутри плейлиста
//!
//! После декодирования пути — обычные Unicode-строки, а затем локальные пути
//! используются как пути файловой системы Linux (UTF-8). Для плейлистов, созданных
//! на Windows с кириллицей, это верно: файлы, перенесённые на Linux, имеют те же
//! имена, записанные в UTF-8.

use std::borrow::Cow;

use encoding_rs::WINDOWS_1251;

/// Кодировка, в которой фактически был прочитан текст плейлиста.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistTextEncoding {
    /// Обычный UTF-8 без BOM.
    Utf8,
    /// UTF-8 с BOM (BOM снимается самим parser-ом).
    Utf8WithBom,
    /// Запасной вариант: байты не были валидным UTF-8, прочитаны как Windows-1251.
    Windows1251,
}

/// Разрешён ли запасной вариант Windows-1251 для данного формата.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LegacyFallback {
    /// Невалидный UTF-8 декодируется как Windows-1251.
    Windows1251,
    /// Невалидный UTF-8 — ошибка (`.m3u8`: по спецификации всегда UTF-8).
    Forbidden,
}

/// Невалидный UTF-8 при запрещённом fallback-е.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidUtf8;

/// Декодирует байты как UTF-8, а при невалидном UTF-8 — по политике `fallback`.
///
/// UTF-8 BOM остаётся в тексте: его снимает и учитывает вызывающий parser (M3U сообщает
/// предупреждение, CUE снимает отдельно). Для валидного UTF-8 копирования нет.
pub(crate) fn decode_utf8_or_windows_1251(
    bytes: &[u8],
    fallback: LegacyFallback,
) -> Result<(Cow<'_, str>, PlaylistTextEncoding), InvalidUtf8> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let encoding = if text.starts_with('\u{feff}') {
            PlaylistTextEncoding::Utf8WithBom
        } else {
            PlaylistTextEncoding::Utf8
        };
        return Ok((Cow::Borrowed(text), encoding));
    }
    match fallback {
        LegacyFallback::Forbidden => Err(InvalidUtf8),
        LegacyFallback::Windows1251 => {
            // Без обработки BOM: UTF-16/UTF-8 BOM в невалидном UTF-8 — просто байты.
            // Для однобайтной кодировки ошибок декодирования не бывает; `None`
            // всё равно честно превращается в отказ, а не в «?»-замены.
            WINDOWS_1251
                .decode_without_bom_handling_and_without_replacement(bytes)
                .map(|text| (text, PlaylistTextEncoding::Windows1251))
                .ok_or(InvalidUtf8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// «Привет» в Windows-1251.
    const PRIVET_CP1251: [u8; 6] = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];

    #[test]
    fn valid_utf8_is_borrowed_and_reports_no_fallback() {
        let (text, encoding) =
            decode_utf8_or_windows_1251("Привет".as_bytes(), LegacyFallback::Windows1251)
                .expect("UTF-8 decodes");
        assert_eq!(text, "Привет");
        assert_eq!(encoding, PlaylistTextEncoding::Utf8);
        assert!(matches!(text, Cow::Borrowed(_)));
    }

    #[test]
    fn utf8_bom_is_reported_and_kept_for_the_parser() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"abc");
        let (text, encoding) =
            decode_utf8_or_windows_1251(&bytes, LegacyFallback::Windows1251).expect("decodes");
        assert_eq!(text, "\u{feff}abc");
        assert_eq!(encoding, PlaylistTextEncoding::Utf8WithBom);
    }

    #[test]
    fn invalid_utf8_falls_back_to_windows_1251() {
        let (text, encoding) =
            decode_utf8_or_windows_1251(&PRIVET_CP1251, LegacyFallback::Windows1251)
                .expect("cp1251 decodes");
        assert_eq!(text, "Привет");
        assert_eq!(encoding, PlaylistTextEncoding::Windows1251);
    }

    #[test]
    fn forbidden_fallback_keeps_invalid_utf8_an_error() {
        assert_eq!(
            decode_utf8_or_windows_1251(&PRIVET_CP1251, LegacyFallback::Forbidden),
            Err(InvalidUtf8)
        );
    }
}
