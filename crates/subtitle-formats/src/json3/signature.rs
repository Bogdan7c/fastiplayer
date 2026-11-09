//! Определение json3 по содержимому: дёшево и только по началу текста.

/// Сколько байт начала текста просматривается: `wireMagic` и `pens` идут в начале файла
/// YouTube, а `events` — сразу за небольшими таблицами стилей.
const SIGNATURE_PREFIX_BYTES: usize = 4096;

/// Ключи верхнего уровня, по которым опознаётся json3.
const SIGNATURE_KEYS: [&str; 2] = ["\"wireMagic\"", "\"events\""];

/// json3: первый значимый символ (после BOM и пробелов) — `{`, а в начале текста есть ключ
/// `"wireMagic"` или `"events"` (за ним `:`). Проверка намеренно приблизительна: окончательное
/// решение принимает разбор (`NotJson3`).
pub(crate) fn has_json3_signature(text: &str) -> bool {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    let Some(after_brace) = trimmed.strip_prefix('{') else {
        return false;
    };
    let prefix = prefix_at_char_boundary(after_brace, SIGNATURE_PREFIX_BYTES);
    SIGNATURE_KEYS
        .iter()
        .any(|key| contains_key_with_colon(prefix, key))
}

/// Начало строки не длиннее `max_bytes` и не режущее символ посередине.
fn prefix_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Есть ли `key` и сразу за ним (с пробелами) двоеточие — то есть это ключ, а не строковое значение.
fn contains_key_with_colon(prefix: &str, key: &str) -> bool {
    prefix
        .match_indices(key)
        .any(|(index, _)| prefix[index + key.len()..].trim_start().starts_with(':'))
}
