#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозные тесты кодировок и лимитов загрузки.

mod common;

use common::{ROOMY_LIMITS, active_texts_at, try_load_text};
use encoding_rs::WINDOWS_1251;
use subtitle_formats::{
    DetectedSubtitleEncoding, LegacyEncodingHint, SubtitleLoadError, SubtitleParseLimits,
};

/// SRT-загрузка для тестов этого файла.
fn try_load_srt(
    bytes: &[u8],
    hint: LegacyEncodingHint,
    limits: &subtitle_formats::SubtitleParseLimits,
) -> Result<subtitle_formats::LoadedTextSubtitle, subtitle_formats::SubtitleLoadError> {
    try_load_text(
        subtitle_formats::TextSubtitleFormat::Srt,
        bytes,
        hint,
        limits,
    )
}

const RUSSIAN_SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nПривет, как твои дела сегодня вечером?\n\n                           2\n00:00:04,000 --> 00:00:06,000\nВсё хорошо, спасибо, что спросил меня.\n";

#[test]
fn cp1251_without_bom_is_detected_and_decoded_to_correct_cyrillic() {
    let (bytes, _, had_unmappable) = WINDOWS_1251.encode(RUSSIAN_SRT);
    assert!(!had_unmappable);
    let loaded = try_load_srt(&bytes, LegacyEncodingHint::Cyrillic, &ROOMY_LIMITS).unwrap();
    assert_eq!(
        loaded.encoding(),
        DetectedSubtitleEncoding::Legacy(WINDOWS_1251)
    );
    assert!(!loaded.had_replacement_characters());
    assert_eq!(
        active_texts_at(&loaded, 1_500),
        ["Привет, как твои дела сегодня вечером?"]
    );
    assert_eq!(
        active_texts_at(&loaded, 4_500),
        ["Всё хорошо, спасибо, что спросил меня."]
    );
}

#[test]
fn utf16_le_with_bom_is_decoded() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(RUSSIAN_SRT.encode_utf16().flat_map(u16::to_le_bytes));
    let loaded = try_load_srt(&bytes, LegacyEncodingHint::Generic, &ROOMY_LIMITS).unwrap();
    assert_eq!(loaded.encoding(), DetectedSubtitleEncoding::Utf16LeWithBom);
    assert_eq!(
        active_texts_at(&loaded, 1_500),
        ["Привет, как твои дела сегодня вечером?"]
    );
}

#[test]
fn utf16_be_without_bom_ascii_content_is_decoded() {
    let ascii = "1\n00:00:01,000 --> 00:00:02,000\nHello\n";
    let bytes: Vec<u8> = ascii.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let loaded = try_load_srt(&bytes, LegacyEncodingHint::Generic, &ROOMY_LIMITS).unwrap();
    assert_eq!(
        loaded.encoding(),
        DetectedSubtitleEncoding::Utf16BeWithoutBom
    );
    assert_eq!(active_texts_at(&loaded, 1_500), ["Hello"]);
}

#[test]
fn utf8_bom_is_stripped_so_first_cue_is_not_lost() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice("1\n00:00:01,000 --> 00:00:02,000\nПривет\n".as_bytes());
    let loaded = try_load_srt(&bytes, LegacyEncodingHint::Generic, &ROOMY_LIMITS).unwrap();
    assert_eq!(loaded.encoding(), DetectedSubtitleEncoding::Utf8WithBom);
    assert_eq!(loaded.document().report().skipped_cues(), 0);
    assert_eq!(active_texts_at(&loaded, 1_500), ["Привет"]);
}

#[test]
fn plain_utf8_is_not_flagged_as_legacy() {
    let loaded = try_load_srt(
        "1\n00:00:01,000 --> 00:00:02,000\nПривет\n".as_bytes(),
        LegacyEncodingHint::Cyrillic,
        &ROOMY_LIMITS,
    )
    .unwrap();
    assert_eq!(loaded.encoding(), DetectedSubtitleEncoding::Utf8);
}

#[test]
fn input_over_byte_limit_is_rejected_before_decoding() {
    let limits = SubtitleParseLimits::new(10, 100, 10);
    // Невалидный UTF-8 с BOM UTF-16: если бы декодирование шло первым, оно бы отработало.
    let bytes = vec![0xFF; 11];
    let error = try_load_srt(&bytes, LegacyEncodingHint::Generic, &limits).unwrap_err();
    assert_eq!(
        error,
        SubtitleLoadError::InputTooLarge {
            actual_bytes: 11,
            limit_bytes: 10
        }
    );
}

#[test]
fn input_exactly_at_byte_limit_is_accepted() {
    let source = "1\n00:00:01,000 --> 00:00:02,000\nx\n";
    let limits = SubtitleParseLimits::new(source.len(), 100, 10);
    let loaded = try_load_srt(source.as_bytes(), LegacyEncodingHint::Generic, &limits).unwrap();
    assert_eq!(loaded.document().events().len(), 1);
}

#[test]
fn more_cues_than_limit_is_fatal_but_exactly_limit_is_fine() {
    let cue =
        |index: u32| format!("{index}\n00:00:{index:02},000 --> 00:00:{index:02},500\ncue\n\n");
    let three: String = (1..=3).map(cue).collect();
    let at_limit = SubtitleParseLimits::new(1 << 20, 3, 10);
    let loaded = try_load_srt(three.as_bytes(), LegacyEncodingHint::Generic, &at_limit).unwrap();
    assert_eq!(loaded.document().events().len(), 3);

    let below = SubtitleParseLimits::new(1 << 20, 2, 10);
    let error = try_load_srt(three.as_bytes(), LegacyEncodingHint::Generic, &below).unwrap_err();
    assert_eq!(error, SubtitleLoadError::TooManyEvents { limit: 2 });
}

/// Короткий файл (одна-две реплики) — частый случай у старых субтитров к трейлерам и
/// клипам; детектору мало статистики, поэтому подсказка «кириллица» должна решить исход.
#[test]
fn short_cp1251_file_with_cyrillic_hint_is_still_read_as_windows_1251() {
    let short_srt =
        "1\n00:00:01,000 --> 00:00:02,000\nДа.\n\n2\n00:00:03,000 --> 00:00:04,000\nНет, ёжик!\n";
    let (bytes, _, had_unmappable) = WINDOWS_1251.encode(short_srt);
    assert!(!had_unmappable);
    let loaded = try_load_srt(&bytes, LegacyEncodingHint::Cyrillic, &ROOMY_LIMITS).unwrap();
    assert_eq!(
        loaded.encoding(),
        DetectedSubtitleEncoding::Legacy(WINDOWS_1251)
    );
    assert_eq!(
        active_texts_at(&loaded, 3_500),
        vec!["Нет, ёжик!".to_owned()]
    );
}
