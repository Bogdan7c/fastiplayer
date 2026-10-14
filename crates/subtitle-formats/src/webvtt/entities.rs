//! Символьные сущности текста реплики: `&amp; &lt; &gt; &nbsp; &lrm; &rlm; &#NN; &#xHH;`.

/// Максимальная длина сущности вместе с `&` и `;` (`&#x10FFFF;` — 10 символов).
/// Ограничение не даёт искать `;` по всему тексту ради каждого `&`.
const MAX_ENTITY_LENGTH: usize = 10;

/// Неразрывный пробел.
const NO_BREAK_SPACE: char = '\u{00A0}';
/// Метки направления письма.
const LEFT_TO_RIGHT_MARK: char = '\u{200E}';
const RIGHT_TO_LEFT_MARK: char = '\u{200F}';

/// Разбирает сущность в начале `rest` (там стоит `&`).
///
/// Возвращает символ и число байтов, которые нужно пропустить. `None` — это не сущность
/// (неизвестное имя, нет `;`, нулевой или недопустимый код): `&` остаётся обычным текстом.
pub(super) fn decode_entity(rest: &str) -> Option<(char, usize)> {
    let end = rest
        .char_indices()
        .take(MAX_ENTITY_LENGTH)
        .find(|(_, ch)| *ch == ';')
        .map(|(index, _)| index)?;
    let name = rest.get(1..end)?;
    let decoded = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "nbsp" => NO_BREAK_SPACE,
        "lrm" => LEFT_TO_RIGHT_MARK,
        "rlm" => RIGHT_TO_LEFT_MARK,
        _ => decode_numeric(name.strip_prefix('#')?)?,
    };
    Some((decoded, end + 1))
}

/// Число после `#`: десятичное или шестнадцатеричное (`x`/`X`).
fn decode_numeric(digits: &str) -> Option<char> {
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) if is_all(hex, |b| b.is_ascii_hexdigit()) => u32::from_str_radix(hex, 16).ok()?,
        Some(_) => return None,
        None if is_all(digits, |b| b.is_ascii_digit()) => digits.parse().ok()?,
        None => return None,
    };
    // Ноль и суррогаты (`from_u32` их отвергает) недопустимы.
    if code == 0 {
        return None;
    }
    char::from_u32(code)
}

/// Непустая строка, все байты которой удовлетворяют условию.
fn is_all(text: &str, predicate: impl Fn(&u8) -> bool) -> bool {
    !text.is_empty() && text.bytes().all(|byte| predicate(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_entities_decode_with_consumed_length() {
        assert_eq!(decode_entity("&amp; rest"), Some(('&', 5)));
        assert_eq!(decode_entity("&lt;"), Some(('<', 4)));
        assert_eq!(decode_entity("&gt;"), Some(('>', 4)));
        assert_eq!(decode_entity("&nbsp;"), Some(('\u{A0}', 6)));
        assert_eq!(decode_entity("&lrm;"), Some(('\u{200E}', 5)));
        assert_eq!(decode_entity("&rlm;"), Some(('\u{200F}', 5)));
    }

    #[test]
    fn numeric_entities_decode_decimal_and_hex() {
        assert_eq!(decode_entity("&#65;"), Some(('A', 5)));
        assert_eq!(decode_entity("&#x41;"), Some(('A', 6)));
        assert_eq!(decode_entity("&#X1F600;"), Some(('\u{1F600}', 9)));
    }

    #[test]
    fn malformed_entities_are_not_entities() {
        for bad in [
            "&amp",
            "&unknown;",
            "&;",
            "&#;",
            "&#x;",
            "&#0;",
            "&#xD800;",
            "&#12a;",
            "&#xZZ;",
            "&#x110000;",
            "& amp;",
            "&averyveryverylongname;",
        ] {
            assert_eq!(decode_entity(bad), None, "{bad:?}");
        }
    }
}
