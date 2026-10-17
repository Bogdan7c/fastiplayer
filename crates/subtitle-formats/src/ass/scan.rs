// Порт поведения libass 0.17.5 (ISC): ass.c (string2timecode, parse_int_header, mystrtou32_modulo, parse_bool, next_token), ass_utils.h (mystrtoi32, skip_spaces).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
// Copyright (C) 2009 Grigori Goronzy <greg@geekmind.org>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Разбор чисел и токенов «как в libass»: `strtoll`, `ass_strtod`, `sscanf`, `next_token`.
//!
//! Файлы ASS пишут десятки редакторов, поэтому libass терпим к мусору: нечисловое значение
//! даёт 0, а не ошибку. Здесь повторены именно эти правила, чтобы результат совпадал.
//! Все функции работают по байтам: разделители ASCII, а срезы берутся только по границам
//! ASCII-символов (корректные границы UTF-8).

/// Пробел или табуляция: то, что libass считает «пробелами» в `skip_spaces`.
pub(super) fn is_ass_space(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

/// Пробелы в смысле C `isspace`/`ass_isspace` (включая переводы строк).
fn is_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Строка до первого NUL: C-строки libass обрываются на нём.
pub(super) fn c_string_prefix(text: &str) -> &str {
    text.split('\0').next().unwrap_or_default()
}

/// Убирает пробелы и табуляции слева (`skip_spaces`).
pub(super) fn skip_spaces(text: &str) -> &str {
    text.trim_start_matches([' ', '\t'])
}

/// Убирает пробелы и табуляции справа (`rskip_spaces`).
pub(super) fn rskip_spaces(text: &str) -> &str {
    text.trim_end_matches([' ', '\t'])
}

/// Ведущий знак числа.
fn take_sign(bytes: &[u8]) -> (bool, usize) {
    match bytes.first() {
        Some(b'-') => (true, 1),
        Some(b'+') => (false, 1),
        _ => (false, 0),
    }
}

fn digit_value(byte: u8, base: u32) -> Option<u32> {
    char::from(byte).to_digit(base)
}

/// Аналог C `strtoll`: пробелы, знак, для base 16 необязательный `0x`, затем цифры.
/// Переполнение насыщается. `None` — цифр нет (`strtoll` вернул бы 0 без продвижения).
fn strtoll_value(text: &str, base: u32) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut pos = bytes.iter().take_while(|byte| is_c_space(**byte)).count();
    let (negative, sign_len) = take_sign(bytes.get(pos..).unwrap_or_default());
    pos += sign_len;
    if base == 16 {
        let has_prefix = matches!(bytes.get(pos..pos + 2), Some([b'0', b'x' | b'X']));
        let digit_follows = bytes
            .get(pos + 2)
            .is_some_and(|byte| digit_value(*byte, 16).is_some());
        if has_prefix && digit_follows {
            pos += 2;
        }
    }
    let mut magnitude: i64 = 0;
    let mut seen_digit = false;
    for byte in bytes.get(pos..).unwrap_or_default() {
        let Some(digit) = digit_value(*byte, base) else {
            break;
        };
        seen_digit = true;
        magnitude = magnitude
            .saturating_mul(i64::from(base))
            .saturating_add(i64::from(digit));
    }
    seen_digit.then(|| if negative { -magnitude } else { magnitude })
}

/// `mystrtoi32`: `strtoll`, зажатый в диапазон `i32`; мусор — 0.
pub(super) fn parse_i32_clamped(text: &str, base: u32) -> i32 {
    let value = strtoll_value(text, base).unwrap_or(0);
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// C `atoi` (поле `ReadOrder`): десятичное, мусор — 0, переполнение зажимается.
pub(super) fn atoi(text: &str) -> i32 {
    parse_i32_clamped(text, 10)
}

/// `parse_int_header`: десятичное, либо `&H`/`0x` шестнадцатеричное; переполнение —
/// по модулю 2^32 (так читает VSFilter), мусор — 0.
pub(super) fn parse_int_header(text: &str) -> i32 {
    let bytes = text.as_bytes();
    let has_hex_prefix = bytes.get(..2).is_some_and(|prefix| {
        prefix.eq_ignore_ascii_case(b"&h") || prefix.eq_ignore_ascii_case(b"0x")
    });
    let (rest, base) = if has_hex_prefix {
        (text.get(2..).unwrap_or_default(), 16)
    } else {
        (text, 10)
    };
    modulo_u32(rest, base) as i32
}

/// `mystrtou32_modulo`: пробелы, знак, для base 16 необязательный `0x`, цифры по модулю 2^32.
fn modulo_u32(text: &str, base: u32) -> u32 {
    let mut rest = skip_spaces(text).as_bytes();
    let negative = match rest.first() {
        Some(b'+') => {
            rest = rest.get(1..).unwrap_or_default();
            false
        }
        Some(b'-') => {
            rest = rest.get(1..).unwrap_or_default();
            true
        }
        _ => false,
    };
    if base == 16
        && rest
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"0x"))
    {
        rest = rest.get(2..).unwrap_or_default();
    }
    let mut value: u32 = 0;
    let mut seen_digit = false;
    for byte in rest {
        let Some(digit) = digit_value(*byte, base) else {
            break;
        };
        seen_digit = true;
        value = value.wrapping_mul(base).wrapping_add(digit);
    }
    if !seen_digit {
        return 0;
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

/// `parse_bool`: `yes` (без учёта регистра) или целое > 0.
pub(super) fn parse_bool(text: &str) -> bool {
    let trimmed = skip_spaces(text);
    let is_yes = trimmed
        .as_bytes()
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"yes"));
    is_yes || strtoll_value(trimmed, 10).is_some_and(|value| value > 0)
}

/// `ass_strtod`: пробелы, знак, мантисса с точкой, необязательная экспонента.
/// Мусор — 0.0. Само преобразование делает `f64::from_str` (точнее, чем самодельный
/// алгоритм libass; различия в последних битах не заметны на экране).
pub(super) fn parse_f64(text: &str) -> f64 {
    let bytes = text.as_bytes();
    let mut pos = bytes.iter().take_while(|byte| is_c_space(**byte)).count();
    let number_start = pos;
    pos += take_sign(bytes.get(pos..).unwrap_or_default()).1;
    let mut mantissa_digits = 0usize;
    let mut seen_point = false;
    while let Some(byte) = bytes.get(pos) {
        if byte.is_ascii_digit() {
            mantissa_digits += 1;
        } else if *byte == b'.' && !seen_point {
            seen_point = true;
        } else {
            break;
        }
        pos += 1;
    }
    if mantissa_digits == 0 {
        return 0.0;
    }
    // Экспонента учитывается, только если за `e` идут цифры (иначе `e` — мусор после числа).
    if matches!(bytes.get(pos), Some(b'e' | b'E')) {
        let mut exponent_pos = pos + 1;
        exponent_pos += take_sign(bytes.get(exponent_pos..).unwrap_or_default()).1;
        let digits = bytes
            .get(exponent_pos..)
            .unwrap_or_default()
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits > 0 {
            pos = exponent_pos + digits;
        }
    }
    text.get(number_start..pos)
        .and_then(|number| number.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// Читает `%d` из `sscanf`: пробелы, знак, цифры. Возвращает значение и остаток.
fn scan_decimal(text: &str) -> Option<(i32, &str)> {
    let bytes = text.as_bytes();
    let start = bytes.iter().take_while(|byte| is_c_space(**byte)).count();
    let digits_start = start + take_sign(bytes.get(start..).unwrap_or_default()).1;
    let digit_count = bytes
        .get(digits_start..)
        .unwrap_or_default()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digit_count == 0 {
        return None;
    }
    let end = digits_start + digit_count;
    let value = parse_i32_clamped(text.get(start..end)?, 10);
    Some((value, text.get(end..)?))
}

/// `string2timecode`: `H:MM:SS.cc` через `sscanf("%d:%d:%d.%d")`, результат в миллисекундах.
///
/// Причуда libass/VSFilter: последняя часть — САНТИСЕКУНДЫ и умножается на 10 независимо от
/// числа цифр (`0:00:01.5` = 1050 мс, `0:00:01.123` = 2230 мс). Если разобраны не все четыре
/// числа — время 0 (в libass только предупреждение). Возвращает `None` в этом случае, чтобы
/// вызывающий мог посчитать проблему; значение libass для `None` — 0.
pub(super) fn string_to_timecode_ms(text: &str) -> Option<i64> {
    let (hours, rest) = scan_decimal(text)?;
    let (minutes, rest) = scan_decimal(rest.strip_prefix(':')?)?;
    let (seconds, rest) = scan_decimal(rest.strip_prefix(':')?)?;
    let (centis, _) = scan_decimal(rest.strip_prefix('.')?)?;
    let total_seconds = (i64::from(hours) * 60 + i64::from(minutes)) * 60 + i64::from(seconds);
    Some(
        total_seconds
            .saturating_mul(1000)
            .saturating_add(i64::from(centis) * 10),
    )
}

/// Курсор по списку полей через запятую (`next_token`): общий для `Format:`, `Style:`,
/// `Dialogue:`.
pub(super) struct FieldCursor<'a> {
    rest: &'a str,
}

impl<'a> FieldCursor<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        Self { rest: text }
    }

    /// Следующее поле. Ведущие пробелы пропускаются; `trim_end` — убирать ли пробелы справа
    /// (имена полей формата — да, значения — нет). `None`, когда строка кончилась.
    ///
    /// Пустое поле между запятыми — это `Some("")`; `None` только если после запятой пусто.
    pub(super) fn next_token(&mut self, trim_end: TrailingSpaces) -> Option<&'a str> {
        let trimmed = skip_spaces(self.rest);
        if trimmed.is_empty() {
            self.rest = trimmed;
            return None;
        }
        let (token, remainder) = match trimmed.split_once(',') {
            Some((token, remainder)) => (token, remainder),
            None => (trimmed, ""),
        };
        self.rest = remainder;
        Some(match trim_end {
            TrailingSpaces::Trim => rskip_spaces(token),
            TrailingSpaces::Keep => token,
        })
    }

    /// Остаток строки как есть (для поля `Text`).
    pub(super) fn remainder(&self) -> &'a str {
        self.rest
    }
}

/// Нужно ли убирать пробелы в конце поля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrailingSpaces {
    Trim,
    Keep,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_header_handles_hex_decimal_and_garbage() {
        assert_eq!(parse_int_header("1920"), 1920);
        assert_eq!(parse_int_header("  -5"), -5);
        assert_eq!(parse_int_header("&H00FFFFFF"), 0x00ff_ffff);
        assert_eq!(parse_int_header("&h10"), 16);
        assert_eq!(parse_int_header("0x1F"), 31);
        assert_eq!(parse_int_header("abc"), 0);
        // Переполнение — по модулю 2^32, как у VSFilter.
        assert_eq!(parse_int_header("&HFFFFFFFF") as u32, u32::MAX);
        assert_eq!(parse_int_header("4294967297"), 1);
    }

    #[test]
    fn clamped_i32_saturates_and_garbage_is_zero() {
        assert_eq!(parse_i32_clamped("99999999999", 10), i32::MAX);
        assert_eq!(parse_i32_clamped("-99999999999", 10), i32::MIN);
        assert_eq!(parse_i32_clamped("zz", 10), 0);
        assert_eq!(parse_i32_clamped("ff&", 16), 255);
        assert_eq!(parse_i32_clamped("0xff", 16), 255);
        // `0x` без цифр: читается только «0».
        assert_eq!(parse_i32_clamped("0xg", 16), 0);
    }

    #[test]
    fn bool_accepts_yes_and_positive_numbers() {
        assert!(parse_bool(" Yes"));
        assert!(parse_bool("1"));
        assert!(!parse_bool("0"));
        assert!(!parse_bool("no"));
        assert!(!parse_bool("-1"));
        assert!(!parse_bool(""));
    }

    #[test]
    fn float_parsing_follows_strtod_rules() {
        assert_eq!(parse_f64("2.5"), 2.5);
        assert_eq!(parse_f64("  -.5x"), -0.5);
        assert_eq!(parse_f64("1e2"), 100.0);
        // `e` без цифр — мусор после числа.
        assert_eq!(parse_f64("3e"), 3.0);
        assert_eq!(parse_f64("."), 0.0);
        assert_eq!(parse_f64("abc"), 0.0);
        assert_eq!(parse_f64("+7"), 7.0);
    }

    #[test]
    fn timecode_last_part_is_centiseconds_times_ten() {
        assert_eq!(string_to_timecode_ms("0:00:01.50"), Some(1500));
        assert_eq!(string_to_timecode_ms("0:00:01.5"), Some(1050));
        assert_eq!(string_to_timecode_ms("0:00:01.123"), Some(2230));
        assert_eq!(string_to_timecode_ms("1:02:03.04"), Some(3_723_040));
        assert_eq!(string_to_timecode_ms("0:00:01"), None);
        assert_eq!(string_to_timecode_ms("garbage"), None);
    }

    #[test]
    fn field_cursor_splits_like_next_token() {
        let mut cursor = FieldCursor::new(" a , b ,,c");
        assert_eq!(cursor.next_token(TrailingSpaces::Trim), Some("a"));
        assert_eq!(cursor.next_token(TrailingSpaces::Keep), Some("b "));
        assert_eq!(cursor.next_token(TrailingSpaces::Trim), Some(""));
        assert_eq!(cursor.next_token(TrailingSpaces::Trim), Some("c"));
        assert_eq!(cursor.next_token(TrailingSpaces::Trim), None);
        let mut trailing_comma = FieldCursor::new("x,");
        assert_eq!(trailing_comma.next_token(TrailingSpaces::Trim), Some("x"));
        assert_eq!(trailing_comma.next_token(TrailingSpaces::Trim), None);
    }

    #[test]
    fn remainder_keeps_commas_and_spaces() {
        let mut cursor = FieldCursor::new("a, hello, world ");
        cursor.next_token(TrailingSpaces::Trim);
        assert_eq!(cursor.remainder(), " hello, world ");
    }
}
