//! Разбор брошенных URI в элементы [`ExternalOpenItem`].
//!
//! Патч winit отдаёт URI «как есть» (RFC 3986, `file://` не раскодирован), потому что
//! в winit 0.31 они приходят так же строками. Поэтому раскодирование живёт здесь, в
//! приложении: тогда при смене источника логика не меняется.
//!
//! Главный инвариант: путь из `file://` декодируется **в байты**, а не в `String`.
//! Имя файла в Linux — произвольные байты, и `%FF` в URI обязано стать байтом `0xFF`
//! в `PathBuf`, а не символом замены.

use std::path::PathBuf;

use super::request::{DroppedWebUrl, ExternalOpenItem};

/// Хосты, которые считаются «этим же компьютером» в `file://хост/путь`.
const LOCAL_FILE_URI_HOST: &str = "localhost";

/// Собирает элементы броска: URI в порядке источника, а при их отсутствии — ссылки из текста.
///
/// Текст используется только как запасной вариант: если источник дал и URI, и текст
/// (так делают браузеры), URI точнее и текст игнорируется.
pub(crate) fn items_from_drop_payload(
    uris: &[String],
    plain_text: Option<&str>,
) -> Vec<ExternalOpenItem> {
    let uri_items: Vec<ExternalOpenItem> = uris
        .iter()
        .map(|uri| uri.trim())
        .filter(|uri| !uri.is_empty())
        .map(item_from_uri)
        .collect();
    if !uri_items.is_empty() {
        return uri_items;
    }
    plain_text.map_or_else(Vec::new, web_urls_from_plain_text)
}

/// Разбирает один URI: локальный файл, веб-ссылка или неподдерживаемое.
pub(crate) fn item_from_uri(uri: &str) -> ExternalOpenItem {
    let uri = uri.trim();
    let scheme = uri_scheme(uri).unwrap_or_default().to_ascii_lowercase();
    match scheme.as_str() {
        "file" => local_path_from_file_uri(uri).map_or(
            ExternalOpenItem::Unsupported { scheme },
            ExternalOpenItem::LocalPath,
        ),
        "http" | "https" => ExternalOpenItem::WebUrl(DroppedWebUrl::new(uri.to_owned())),
        _ => ExternalOpenItem::Unsupported { scheme },
    }
}

/// Берёт из простого текста строки, похожие на http(s)-ссылки (остальные строки молча
/// пропускаются: это запасной путь, а не разбор произвольного текста).
fn web_urls_from_plain_text(text: &str) -> Vec<ExternalOpenItem> {
    text.lines()
        .map(str::trim)
        .filter(|line| {
            matches!(
                uri_scheme(line).map(str::to_ascii_lowercase).as_deref(),
                Some("http" | "https")
            ) && line.contains("://")
        })
        .map(|line| ExternalOpenItem::WebUrl(DroppedWebUrl::new(line.to_owned())))
        .collect()
}

/// Схема по RFC 3986: `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` перед первым `:`.
fn uri_scheme(uri: &str) -> Option<&str> {
    let (scheme, _rest) = uri.split_once(':')?;
    let mut chars = scheme.chars();
    let first_is_alpha = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let rest_is_valid = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    (first_is_alpha && rest_is_valid).then_some(scheme)
}

/// `file://[host]/путь` → путь. `None`, если хост чужой или путь некорректен.
fn local_path_from_file_uri(uri: &str) -> Option<PathBuf> {
    // `file:` регистронезависим по RFC; схема уже проверена, срезаем ровно 5 байт.
    let after_scheme = uri.get("file:".len()..)?;
    let after_slashes = after_scheme.strip_prefix("//")?;
    let path_start = after_slashes.find('/')?;
    let (host, raw_path) = after_slashes.split_at(path_start);
    if !host.is_empty() && !host.eq_ignore_ascii_case(LOCAL_FILE_URI_HOST) {
        return None;
    }
    // Query и fragment в `file://` не часть пути: литеральные `?` и `#` приходят как %3F/%23.
    let raw_path = raw_path.split(['?', '#']).next().unwrap_or(raw_path);
    let decoded_bytes = percent_decode_to_bytes(raw_path);
    // NUL в пути невозможен в файловой системе и обрезал бы путь на границе C-строки.
    if decoded_bytes.contains(&0) {
        return None;
    }
    path_from_bytes(decoded_bytes)
}

/// Раскодирует `%XX` в байты. Некорректная последовательность (`%zz`, хвостовой `%`)
/// остаётся буквальными символами: лучше открыть файл с «%» в имени, чем потерять его.
fn percent_decode_to_bytes(encoded: &str) -> Vec<u8> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(decode_hex_pair);
        match escaped {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    decoded
}

/// Два hex-символа → байт.
fn decode_hex_pair(pair: &[u8]) -> Option<u8> {
    let high = hex_digit_value(*pair.first()?)?;
    let low = hex_digit_value(*pair.get(1)?)?;
    Some((high << 4) | low)
}

/// Значение одной hex-цифры.
const fn hex_digit_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// Байты → `PathBuf` без потерь на Unix.
#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// На не-Unix путь обязан быть UTF-8; `/C:/dir` превращается в `C:/dir`.
/// (Эти платформы получают пути из устаревших событий winit, а не из URI.)
#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    let text = String::from_utf8(bytes).ok()?;
    let without_drive_slash = match text.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => &text[1..],
        _ => text.as_str(),
    };
    Some(PathBuf::from(without_drive_slash))
}

#[cfg(test)]
mod tests;
