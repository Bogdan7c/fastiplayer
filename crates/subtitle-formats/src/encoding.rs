//! Превращение байтов файла субтитров в текст с определением кодировки.
//!
//! Порядок решений (первое сработавшее побеждает):
//! 1. BOM: UTF-8, UTF-16LE, UTF-16BE.
//! 2. UTF-16 без BOM по шаблону нулевых байтов.
//! 3. Корректный UTF-8.
//! 4. Эвристика `chardetng` для легаси-кодировок (cp1251, KOI8-R, ...).
//!
//! Шаг 2 стоит ДО проверки UTF-8 сознательно: нулевой байт — валидный UTF-8, поэтому
//! UTF-16 с латиницей без BOM иначе был бы принят за UTF-8 с NUL-символами. В настоящих
//! текстовых субтитрах нулевых байтов не бывает.

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];

/// Минимальная доля пар байт, где «старший» байт равен нулю, чтобы счесть текст UTF-16
/// без BOM (в процентах). Латиница/цифры/пробелы/знаки времени в UTF-16 дают NUL в каждой
/// паре, но кириллица и CJK — нет, поэтому порог низкий: достаточно заметной доли ASCII
/// (тайминги `00:00:01,000` есть в каждой реплике SRT).
const UTF16_MIN_NUL_PAIR_PERCENT: usize = 30;

/// Максимальная доля NUL на противоположной чётности (в процентах). В настоящем UTF-16
/// эта чётность почти не содержит нулей (исключение — символы вроде U+0100); больший
/// процент означает бинарный мусор, а не текст.
const UTF16_MAX_OPPOSITE_NUL_PERCENT: usize = 5;

/// Подсказка, какую группу легаси-кодировок ожидать. Определяется языком интерфейса/дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyEncodingHint {
    /// Ожидается кириллица (cp1251, KOI8-R, cp866).
    Cyrillic,
    /// Подсказки нет.
    Generic,
}

impl LegacyEncodingHint {
    /// Домен верхнего уровня для `chardetng`. Берётся ТОЛЬКО из этого соответствия:
    /// `chardetng` паникует на не-ASCII/верхнем регистре/точке, поэтому
    /// пользовательский ввод сюда попасть не может.
    fn chardetng_top_level_domain(self) -> Option<&'static [u8]> {
        match self {
            Self::Cyrillic => Some(b"ru"),
            Self::Generic => None,
        }
    }
}

/// Какая кодировка в итоге использована.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedSubtitleEncoding {
    Utf8,
    Utf8WithBom,
    Utf16LeWithBom,
    Utf16BeWithBom,
    Utf16LeWithoutBom,
    Utf16BeWithoutBom,
    /// Легаси-кодировка, угаданная эвристикой (может ошибаться).
    Legacy(&'static Encoding),
}

impl DetectedSubtitleEncoding {
    /// Имя для показа пользователю («прочитано как windows-1251»).
    pub fn encoding_name(&self) -> &'static str {
        match self {
            Self::Utf8 | Self::Utf8WithBom => UTF_8.name(),
            Self::Utf16LeWithBom | Self::Utf16LeWithoutBom => UTF_16LE.name(),
            Self::Utf16BeWithBom | Self::Utf16BeWithoutBom => UTF_16BE.name(),
            Self::Legacy(encoding) => encoding.name(),
        }
    }

    /// Это семейство Unicode (кодировка известна точно, а не угадана).
    pub fn is_unicode(&self) -> bool {
        !matches!(self, Self::Legacy(_))
    }
}

/// Декодированный текст и как он получен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSubtitleText {
    /// Текст без BOM.
    pub text: String,
    pub encoding: DetectedSubtitleEncoding,
    /// В исходных байтах были невалидные последовательности, заменённые на U+FFFD.
    pub had_replacement_characters: bool,
}

/// Декодирует байты файла субтитров. Не падает: невалидные байты заменяются на U+FFFD.
pub fn decode_subtitle_bytes(bytes: &[u8], hint: LegacyEncodingHint) -> DecodedSubtitleText {
    if let Some(rest) = bytes.strip_prefix(UTF8_BOM) {
        return decode_with(UTF_8, rest, DetectedSubtitleEncoding::Utf8WithBom);
    }
    if let Some(rest) = bytes.strip_prefix(UTF16_LE_BOM) {
        return decode_with(UTF_16LE, rest, DetectedSubtitleEncoding::Utf16LeWithBom);
    }
    if let Some(rest) = bytes.strip_prefix(UTF16_BE_BOM) {
        return decode_with(UTF_16BE, rest, DetectedSubtitleEncoding::Utf16BeWithBom);
    }
    if let Some(byte_order) = guess_utf16_without_bom(bytes) {
        return match byte_order {
            Utf16ByteOrder::LittleEndian => {
                decode_with(UTF_16LE, bytes, DetectedSubtitleEncoding::Utf16LeWithoutBom)
            }
            Utf16ByteOrder::BigEndian => {
                decode_with(UTF_16BE, bytes, DetectedSubtitleEncoding::Utf16BeWithoutBom)
            }
        };
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return DecodedSubtitleText {
            text: text.to_owned(),
            encoding: DetectedSubtitleEncoding::Utf8,
            had_replacement_characters: false,
        };
    }
    let legacy = guess_legacy_encoding(bytes, hint);
    decode_with(legacy, bytes, DetectedSubtitleEncoding::Legacy(legacy))
}

/// Убирает символы NUL из текста SRT/WebVTT.
///
/// NUL в тексте реплики — мусор (некоторые редакторы добивают файл нулями до размера кластера),
/// а не содержимое: оставленный, он склеивается с последней репликой и раздувает её сверх
/// лимита растеризатора. Удаляются только сами `\0`, переводы строк не трогаются, поэтому
/// номера строк в отчёте остаются верными. Без NUL текст не копируется.
/// Для ASS не применяется: там обрезка по NUL — намеренное поведение libass.
pub(crate) fn strip_nul_characters(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\0') {
        std::borrow::Cow::Owned(text.replace('\0', ""))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Декодирует без повторной обработки BOM (он уже снят вызывающим).
fn decode_with(
    encoding: &'static Encoding,
    bytes: &[u8],
    label: DetectedSubtitleEncoding,
) -> DecodedSubtitleText {
    let (text, had_replacement_characters) = encoding.decode_without_bom_handling(bytes);
    DecodedSubtitleText {
        text: text.into_owned(),
        encoding: label,
        had_replacement_characters,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Utf16ByteOrder {
    LittleEndian,
    BigEndian,
}

/// Угадывает UTF-16 без BOM: чётная длина и нули доминируют на одной чётности байт.
///
/// LE: ASCII-символ = `[байт, 0x00]`, нули на нечётных индексах; BE — наоборот.
fn guess_utf16_without_bom(bytes: &[u8]) -> Option<Utf16ByteOrder> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(2) {
        return None;
    }
    let pair_count = bytes.len() / 2;
    let nul_at_even = bytes.iter().step_by(2).filter(|&&byte| byte == 0).count();
    let nul_at_odd = bytes
        .iter()
        .skip(1)
        .step_by(2)
        .filter(|&&byte| byte == 0)
        .count();
    let dominates = |dominant: usize, opposite: usize| {
        dominant * 100 >= pair_count * UTF16_MIN_NUL_PAIR_PERCENT
            && opposite * 100 <= pair_count * UTF16_MAX_OPPOSITE_NUL_PERCENT
    };
    if dominates(nul_at_odd, nul_at_even) {
        Some(Utf16ByteOrder::LittleEndian)
    } else if dominates(nul_at_even, nul_at_odd) {
        Some(Utf16ByteOrder::BigEndian)
    } else {
        None
    }
}

/// Запускает `chardetng` над всем входом (размер уже ограничен лимитом вызывающего).
fn guess_legacy_encoding(bytes: &[u8], hint: LegacyEncodingHint) -> &'static Encoding {
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    // `true` — это весь вход; результат feed (были ли не-ASCII байты) нам не нужен.
    let _saw_non_ascii = detector.feed(bytes, true);
    // UTF-8 мы уже проверили выше и отвергли, поэтому в детекторе он запрещён.
    detector.guess(hint.chardetng_top_level_domain(), Utf8Detection::Deny)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16_bytes(text: &str, big_endian: bool) -> Vec<u8> {
        text.encode_utf16()
            .flat_map(|unit| {
                if big_endian {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                }
            })
            .collect()
    }

    #[test]
    fn utf16_le_ascii_without_bom_is_recognized_and_decoded() {
        let bytes = utf16_bytes("1\n00:00:01,000 --> 00:00:02,000\nHello\n", false);
        let decoded = decode_subtitle_bytes(&bytes, LegacyEncodingHint::Generic);
        assert_eq!(
            decoded.encoding,
            DetectedSubtitleEncoding::Utf16LeWithoutBom
        );
        assert_eq!(decoded.text, "1\n00:00:01,000 --> 00:00:02,000\nHello\n");
    }

    #[test]
    fn utf16_be_ascii_without_bom_is_recognized_and_decoded() {
        let bytes = utf16_bytes("1\n00:00:01,000 --> 00:00:02,000\nHello\n", true);
        let decoded = decode_subtitle_bytes(&bytes, LegacyEncodingHint::Generic);
        assert_eq!(
            decoded.encoding,
            DetectedSubtitleEncoding::Utf16BeWithoutBom
        );
        assert_eq!(decoded.text, "1\n00:00:01,000 --> 00:00:02,000\nHello\n");
    }

    #[test]
    fn odd_length_nul_heavy_input_is_not_treated_as_utf16() {
        let mut bytes = utf16_bytes("Hello world", false);
        bytes.push(b'!');
        let decoded = decode_subtitle_bytes(&bytes, LegacyEncodingHint::Generic);
        assert_eq!(decoded.encoding, DetectedSubtitleEncoding::Utf8);
    }

    #[test]
    fn nuls_on_both_parities_are_not_utf16() {
        let bytes = vec![0u8; 16];
        assert_eq!(guess_utf16_without_bom(&bytes), None);
    }

    #[test]
    fn plain_ascii_without_nuls_is_utf8() {
        let decoded = decode_subtitle_bytes(b"hello", LegacyEncodingHint::Generic);
        assert_eq!(decoded.encoding, DetectedSubtitleEncoding::Utf8);
        assert_eq!(decoded.text, "hello");
        assert!(!decoded.had_replacement_characters);
    }

    #[test]
    fn empty_input_is_empty_utf8() {
        let decoded = decode_subtitle_bytes(b"", LegacyEncodingHint::Generic);
        assert_eq!(decoded.encoding, DetectedSubtitleEncoding::Utf8);
        assert_eq!(decoded.text, "");
    }

    #[test]
    fn utf8_bom_is_stripped_and_reported() {
        let decoded = decode_subtitle_bytes(b"\xEF\xBB\xBFabc", LegacyEncodingHint::Generic);
        assert_eq!(decoded.encoding, DetectedSubtitleEncoding::Utf8WithBom);
        assert_eq!(decoded.text, "abc");
    }

    #[test]
    fn utf16_be_bom_is_stripped_and_decoded() {
        let mut bytes = vec![0xFE, 0xFF];
        bytes.extend(utf16_bytes("Привет", true));
        let decoded = decode_subtitle_bytes(&bytes, LegacyEncodingHint::Generic);
        assert_eq!(decoded.encoding, DetectedSubtitleEncoding::Utf16BeWithBom);
        assert_eq!(decoded.text, "Привет");
    }

    #[test]
    fn broken_utf16_after_bom_reports_replacement_characters() {
        // Одиночный старший суррогат 0xD800 невалиден.
        let decoded = decode_subtitle_bytes(&[0xFF, 0xFE, 0x00, 0xD8], LegacyEncodingHint::Generic);
        assert!(decoded.had_replacement_characters);
        assert_eq!(decoded.text, "\u{FFFD}");
    }

    #[test]
    fn encoding_metadata_distinguishes_unicode_from_legacy() {
        let legacy = DetectedSubtitleEncoding::Legacy(encoding_rs::WINDOWS_1251);
        assert_eq!(legacy.encoding_name(), "windows-1251");
        assert!(!legacy.is_unicode());
        assert_eq!(
            DetectedSubtitleEncoding::Utf16BeWithBom.encoding_name(),
            "UTF-16BE"
        );
        assert!(DetectedSubtitleEncoding::Utf8WithBom.is_unicode());
    }
}
