//! Разбор байтов, полученных от источника drag & drop, в публичное содержимое жеста.
//!
//! Все функции чистые (без ввода-вывода), поэтому покрыты тестами напрямую.
//! Ничего не декодируем из percent-encoding и не превращаем `file://` в путь:
//! сырые URI отдаются приложению как есть (см. `ExternalDragPayload`).
//! Единственное исключение — [`file_uri_to_path`], нужная только для legacy-событий
//! `DroppedFile` на Wayland.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use tracing::warn;

use super::mime::PayloadFormat;
use crate::platform::external_drag::ExternalDragPayload;

/// Префикс локальных файловых URI.
const FILE_URI_PREFIX: &str = "file://";
/// Хост, который означает «эта же машина» в `file://localhost/...`.
const LOCALHOST_HOST: &str = "localhost";
/// Метка порядка байт UTF-16 (U+FEFF).
const UTF16_BYTE_ORDER_MARK: u16 = 0xFEFF;
/// Та же метка, прочитанная с неверным порядком байт.
const UTF16_SWAPPED_BYTE_ORDER_MARK: u16 = 0xFFFE;

/// Данные одного жеста не поместились в лимит [`MAX_DROP_PAYLOAD_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PayloadTooLarge {
    /// Сколько байт было бы накоплено с очередным куском.
    pub(crate) attempted_bytes: usize,
}

/// Добавляет очередной кусок данных, не выходя за лимит.
///
/// При превышении буфер не меняется, вызывающий обязан завершить жест ошибкой.
pub(crate) fn append_bounded(
    buffer: &mut Vec<u8>,
    chunk: &[u8],
    limit_bytes: usize,
) -> Result<(), PayloadTooLarge> {
    let attempted_bytes = buffer.len().saturating_add(chunk.len());
    if attempted_bytes > limit_bytes {
        return Err(PayloadTooLarge { attempted_bytes });
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

/// Собирает содержимое жеста из байтов выбранного формата.
pub(crate) fn payload_from_bytes(format: PayloadFormat, bytes: &[u8]) -> ExternalDragPayload {
    match format {
        PayloadFormat::UriList => ExternalDragPayload::new(parse_uri_list(bytes), None),
        PayloadFormat::MozUrl => ExternalDragPayload::new(parse_moz_url(bytes), None),
        PayloadFormat::PlainText => ExternalDragPayload::new(Vec::new(), decode_plain_text(bytes)),
    }
}

/// Разбирает `text/uri-list` (RFC 2483): по URI на строку, `#` — комментарий.
///
/// Порядок источника сохраняется, пустые строки и комментарии отбрасываются,
/// percent-encoding не трогается. Строки не-UTF-8 пропускаются с предупреждением
/// (URI по RFC 3986 состоят из ASCII).
pub(crate) fn parse_uri_list(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|raw_line| {
            let line = match std::str::from_utf8(raw_line) {
                Ok(line) => line.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\0'),
                Err(error) => {
                    warn!("drag & drop: строка text/uri-list не UTF-8, пропущена: {error}");
                    return None;
                },
            };
            (!line.is_empty() && !line.starts_with('#')).then(|| line.to_owned())
        })
        .collect()
}

/// Разбирает `text/x-moz-url`: UTF-16, строки чередуются «url», «заголовок».
///
/// Возвращает только ссылки (строки с чётными номерами), в исходном порядке.
pub(crate) fn parse_moz_url(bytes: &[u8]) -> Vec<String> {
    let Some(text) = decode_utf16(bytes) else {
        warn!("drag & drop: text/x-moz-url не является корректным UTF-16, пропущен");
        return Vec::new();
    };

    text.lines()
        .step_by(2)
        .map(|line| line.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Декодирует простой текст (UTF-8). Завершающие NUL-ы, которые добавляют некоторые
/// источники, отбрасываются. Невалидный UTF-8 — `None` с предупреждением.
pub(crate) fn decode_plain_text(bytes: &[u8]) -> Option<String> {
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let text = text.trim_end_matches('\0');
            (!text.is_empty()).then(|| text.to_owned())
        },
        Err(error) => {
            warn!("drag & drop: простой текст не UTF-8, пропущен: {error}");
            None
        },
    }
}

/// Декодирует UTF-16 с необязательной меткой порядка байт (по умолчанию little-endian,
/// как отдаёт Firefox на x86/ARM Linux).
fn decode_utf16(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut code_units: Vec<u16> =
        bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();

    match code_units.first() {
        Some(&UTF16_BYTE_ORDER_MARK) => {
            code_units.remove(0);
        },
        Some(&UTF16_SWAPPED_BYTE_ORDER_MARK) => {
            code_units.remove(0);
            for unit in &mut code_units {
                *unit = unit.swap_bytes();
            }
        },
        _ => {},
    }

    String::from_utf16(&code_units).ok()
}

/// Превращает `file://` URI в путь без потерь (percent-decoding в байты).
///
/// Принимаются `file:///path` и `file://localhost/path`; URI с другим хостом и
/// не-файловые схемы дают `None`. Нужна только для legacy-события `DroppedFile`.
pub(crate) fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    // Схема URI регистронезависима (RFC 3986): `FILE://` — тот же файловый URI.
    let prefix = uri.get(..FILE_URI_PREFIX.len())?;
    if !prefix.eq_ignore_ascii_case(FILE_URI_PREFIX) {
        return None;
    }
    let rest = &uri[FILE_URI_PREFIX.len()..];
    // `?query` и `#fragment` не часть пути; закодированные `%3F` / `%23` остаются
    // в имени, потому что обрезаем до percent-decoding.
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let encoded_path = match rest.strip_prefix(LOCALHOST_HOST) {
        Some(after_host) if after_host.starts_with('/') => after_host,
        _ => rest,
    };
    if !encoded_path.starts_with('/') {
        // Указан чужой хост (`file://host/...`): локального пути нет.
        return None;
    }
    let decoded = percent_decode_bytes(encoded_path.as_bytes())?;
    Some(PathBuf::from(OsString::from_vec(decoded)))
}

/// Раскодирует `%XX` в байты; битая последовательность — `None`.
fn percent_decode_bytes(encoded: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'%' {
            let high = hex_value(*encoded.get(index + 1)?)?;
            let low = hex_value(*encoded.get(index + 2)?)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(encoded[index]);
            index += 1;
        }
    }
    Some(decoded)
}

/// Значение одной шестнадцатеричной цифры ASCII.
fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt;

    use super::*;
    use crate::platform_impl::external_drag::MAX_DROP_PAYLOAD_BYTES;

    /// Кодирует строку как UTF-16LE (формат `text/x-moz-url` у Firefox).
    fn utf16_le(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn uri_list_with_crlf_keeps_order_and_drops_comments_and_blanks() {
        let bytes = b"# comment\r\nfile:///a.mkv\r\n\r\nhttps://example.org/v?x=1\r\n#tail\r\nfile:///b.mkv\r\n";

        let uris = parse_uri_list(bytes);

        assert_eq!(uris, ["file:///a.mkv", "https://example.org/v?x=1", "file:///b.mkv"]);
    }

    #[test]
    fn uri_list_with_lf_only_and_no_trailing_newline_is_parsed() {
        let uris = parse_uri_list(b"file:///one\nfile:///two");

        assert_eq!(uris, ["file:///one", "file:///two"]);
    }

    #[test]
    fn uri_list_leaves_percent_encoding_untouched() {
        let uris = parse_uri_list(b"file:///home/u/%D1%84%D0%B8%D0%BB%D1%8C%D0%BC%20one.mkv\r\n");

        assert_eq!(uris, ["file:///home/u/%D1%84%D0%B8%D0%BB%D1%8C%D0%BC%20one.mkv"]);
    }

    #[test]
    fn uri_list_strips_trailing_nul_and_skips_non_utf8_lines() {
        let mut bytes = b"file:///ok\r\n".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE, b'\r', b'\n']);
        bytes.extend_from_slice(b"file:///also-ok\0");

        let uris = parse_uri_list(&bytes);

        assert_eq!(uris, ["file:///ok", "file:///also-ok"]);
    }

    #[test]
    fn empty_uri_list_yields_no_uris() {
        assert!(parse_uri_list(b"").is_empty());
        assert!(parse_uri_list(b"\r\n# only comment\r\n").is_empty());
    }

    #[test]
    fn moz_url_extracts_urls_and_skips_titles() {
        let bytes = utf16_le(
            "https://example.org/watch?v=1\nПример заголовка\nhttps://example.org/2\nTitle 2",
        );

        let urls = parse_moz_url(&bytes);

        assert_eq!(urls, ["https://example.org/watch?v=1", "https://example.org/2"]);
    }

    #[test]
    fn moz_url_honours_byte_order_marks_and_trailing_nul() {
        let mut little_endian = vec![0xFF, 0xFE];
        little_endian.extend(utf16_le("https://a.example/\nTitle\0"));
        assert_eq!(parse_moz_url(&little_endian), ["https://a.example/"]);

        let mut big_endian = vec![0xFE, 0xFF];
        big_endian.extend("https://b.example/\nT".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(parse_moz_url(&big_endian), ["https://b.example/"]);
    }

    #[test]
    fn moz_url_with_odd_length_or_lone_surrogate_is_rejected() {
        assert!(parse_moz_url(&[0x68, 0x00, 0x69]).is_empty());
        // Одинокий старший суррогат D800 — невалидный UTF-16.
        assert!(parse_moz_url(&[0x00, 0xD8, 0x41, 0x00]).is_empty());
    }

    #[test]
    fn plain_text_is_decoded_and_nul_terminator_removed() {
        assert_eq!(decode_plain_text("привет, мир\0".as_bytes()), Some("привет, мир".to_owned()));
        assert_eq!(decode_plain_text(b""), None);
        assert_eq!(decode_plain_text(&[0xC3, 0x28]), None);
    }

    #[test]
    fn payload_from_bytes_routes_each_format_to_the_right_field() {
        let uri_payload = payload_from_bytes(PayloadFormat::UriList, b"file:///x\r\n");
        assert_eq!(uri_payload.uris(), ["file:///x"]);
        assert_eq!(uri_payload.plain_text(), None);

        let text_payload = payload_from_bytes(PayloadFormat::PlainText, b"https://example.org/");
        assert!(text_payload.uris().is_empty());
        assert_eq!(text_payload.plain_text(), Some("https://example.org/"));

        let moz_payload =
            payload_from_bytes(PayloadFormat::MozUrl, &utf16_le("https://m.example/\nT"));
        assert_eq!(moz_payload.uris(), ["https://m.example/"]);
    }

    #[test]
    fn append_bounded_accepts_up_to_the_limit_and_rejects_beyond_it() {
        let mut buffer = Vec::new();

        assert_eq!(append_bounded(&mut buffer, b"abcd", 6), Ok(()));
        assert_eq!(append_bounded(&mut buffer, b"ef", 6), Ok(()));
        assert_eq!(
            append_bounded(&mut buffer, b"g", 6),
            Err(PayloadTooLarge { attempted_bytes: 7 })
        );

        assert_eq!(buffer, b"abcdef", "при отказе буфер не меняется");
    }

    #[test]
    fn production_limit_is_one_mebibyte() {
        assert_eq!(MAX_DROP_PAYLOAD_BYTES, 1_048_576);
        let mut buffer = vec![0_u8; MAX_DROP_PAYLOAD_BYTES];
        assert!(append_bounded(&mut buffer, b"x", MAX_DROP_PAYLOAD_BYTES).is_err());
    }

    #[test]
    fn file_uri_is_decoded_to_exact_bytes_including_non_utf8() {
        let path = file_uri_to_path("file:///tmp/%D1%84%20x%FF.mkv").expect("path");

        assert_eq!(path.as_os_str().as_bytes(), b"/tmp/\xD1\x84 x\xFF.mkv");
    }

    #[test]
    fn file_uri_accepts_localhost_and_rejects_foreign_hosts_and_other_schemes() {
        assert_eq!(file_uri_to_path("file://localhost/etc/a"), Some(PathBuf::from("/etc/a")));
        assert_eq!(file_uri_to_path("file://otherhost/etc/a"), None);
        assert_eq!(file_uri_to_path("https://example.org/a"), None);
        assert_eq!(file_uri_to_path("fil"), None);
        assert_eq!(file_uri_to_path("file:///broken%2"), None);
        assert_eq!(file_uri_to_path("file:///broken%zz"), None);
    }

    #[test]
    fn file_uri_drops_query_and_fragment_but_keeps_encoded_marks() {
        assert_eq!(
            file_uri_to_path("file:///tmp/a.mkv?download=1#t=5"),
            Some(PathBuf::from("/tmp/a.mkv"))
        );
        // Закодированные `?` и `#` — часть имени файла.
        assert_eq!(
            file_uri_to_path("file:///tmp/a%23b%3Fc.mkv"),
            Some(PathBuf::from("/tmp/a#b?c.mkv"))
        );
    }

    #[test]
    fn file_uri_scheme_is_case_insensitive() {
        assert_eq!(file_uri_to_path("FILE:///tmp/a.mkv"), Some(PathBuf::from("/tmp/a.mkv")));
        assert_eq!(file_uri_to_path("File://localhost/tmp/a"), Some(PathBuf::from("/tmp/a")));
    }
}
